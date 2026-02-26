use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use agent_client_protocol::{StopReason, ToolCallStatus};
use serde_json::json;
use tokio::sync::{Mutex, mpsc};

use super::bootstrap;
use super::codex_rpc::CodexRpcConnection;

#[derive(Debug, Clone)]
pub(super) struct PromptRunContext {
    pub session_key: String,
    pub cwd: PathBuf,
    pub prompt_text: String,
    pub tool_call_id: String,
    pub cancel_requested: Arc<AtomicBool>,
    pub stream_chunk_delay: Duration,
}

#[derive(Debug, Clone)]
pub(super) enum BackendEvent {
    ToolStart {
        tool_call_id: String,
        title: String,
        status: ToolCallStatus,
    },
    MessageChunk(String),
    ToolUpdate {
        tool_call_id: String,
        status: ToolCallStatus,
    },
}

#[async_trait::async_trait(?Send)]
pub(super) trait PromptBackend: 'static {
    async fn run_prompt(
        &self,
        context: PromptRunContext,
        event_tx: mpsc::UnboundedSender<BackendEvent>,
    ) -> agent_client_protocol::Result<StopReason>;
}

pub(super) enum PromptBackendImpl {
    Codex(CodexBackend),
}

#[async_trait::async_trait(?Send)]
impl PromptBackend for PromptBackendImpl {
    async fn run_prompt(
        &self,
        context: PromptRunContext,
        event_tx: mpsc::UnboundedSender<BackendEvent>,
    ) -> agent_client_protocol::Result<StopReason> {
        match self {
            Self::Codex(backend) => backend.run_prompt(context, event_tx).await,
        }
    }
}

pub(super) struct CodexBackend {
    codex_binary_path: Option<PathBuf>,
    default_model: Option<String>,
    startup_timeout: Duration,
    thread_by_session: Mutex<HashMap<String, String>>,
}

impl CodexBackend {
    pub(super) fn new(
        codex_binary_path: Option<PathBuf>,
        default_model: Option<String>,
        startup_timeout: Duration,
    ) -> Self {
        Self {
            codex_binary_path,
            default_model,
            startup_timeout,
            thread_by_session: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl PromptBackend for CodexBackend {
    async fn run_prompt(
        &self,
        context: PromptRunContext,
        event_tx: mpsc::UnboundedSender<BackendEvent>,
    ) -> agent_client_protocol::Result<StopReason> {
        let _ = event_tx.send(BackendEvent::ToolStart {
            tool_call_id: context.tool_call_id.clone(),
            title: "prompt_turn:codex".to_string(),
            status: ToolCallStatus::InProgress,
        });

        let result = self.run_prompt_with_recovery(&context, &event_tx).await;
        match &result {
            Ok(StopReason::EndTurn) => {
                let _ = event_tx.send(BackendEvent::ToolUpdate {
                    tool_call_id: context.tool_call_id,
                    status: ToolCallStatus::Completed,
                });
            }
            Ok(StopReason::Cancelled) => {
                let _ = event_tx.send(BackendEvent::ToolUpdate {
                    tool_call_id: context.tool_call_id,
                    status: ToolCallStatus::Failed,
                });
            }
            Ok(_) | Err(_) => {
                let _ = event_tx.send(BackendEvent::ToolUpdate {
                    tool_call_id: context.tool_call_id,
                    status: ToolCallStatus::Failed,
                });
            }
        }

        result
    }
}

impl CodexBackend {
    async fn run_prompt_with_recovery(
        &self,
        context: &PromptRunContext,
        event_tx: &mpsc::UnboundedSender<BackendEvent>,
    ) -> agent_client_protocol::Result<StopReason> {
        let first_attempt = self.run_prompt_inner(context, event_tx).await;
        let err = match first_attempt {
            Ok(stop_reason) => return Ok(stop_reason),
            Err(err) => err,
        };

        if !should_restart_after_error(&err) {
            return Err(err);
        }

        tracing::warn!("codex backend connection failed; restarting app-server and retrying once");
        if let Err(restart_err) = bootstrap::restart_codex_app_server(
            self.codex_binary_path.as_deref(),
            self.startup_timeout,
        )
        .await
        {
            tracing::warn!("failed to restart codex app-server: {restart_err}");
            return Err(err);
        }

        self.thread_by_session.lock().await.clear();
        self.run_prompt_inner(context, event_tx).await
    }

    async fn run_prompt_inner(
        &self,
        context: &PromptRunContext,
        event_tx: &mpsc::UnboundedSender<BackendEvent>,
    ) -> agent_client_protocol::Result<StopReason> {
        let app_server_url = bootstrap::ensure_codex_app_server_ready(
            self.codex_binary_path.as_deref(),
            self.startup_timeout,
        )
        .await?;
        let mut rpc = CodexRpcConnection::connect(&app_server_url).await?;
        rpc.initialize().await?;
        rpc.send_notification("initialized", json!({})).await?;

        let thread_id = {
            let maybe_existing = self
                .thread_by_session
                .lock()
                .await
                .get(&context.session_key)
                .cloned();
            match maybe_existing {
                Some(id) => id,
                None => {
                    let id = rpc
                        .start_thread(context.cwd.clone(), self.default_model.clone())
                        .await?;
                    self.thread_by_session
                        .lock()
                        .await
                        .insert(context.session_key.clone(), id.clone());
                    id
                }
            }
        };

        let turn_id = rpc
            .start_turn(
                &thread_id,
                context.prompt_text.clone(),
                self.default_model.clone(),
            )
            .await?;

        let stop_reason = rpc
            .wait_turn_completion(
                &thread_id,
                &turn_id,
                &context.cancel_requested,
                event_tx,
                context.stream_chunk_delay,
            )
            .await?;

        Ok(stop_reason)
    }
}

fn should_restart_after_error(err: &agent_client_protocol::Error) -> bool {
    let message = err.to_string();
    [
        "failed to connect codex app-server",
        "codex app-server websocket closed",
        "failed to read codex app-server message",
        "failed to send codex app-server message",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::Error;

    use super::should_restart_after_error;

    #[test]
    fn restart_is_enabled_for_transport_failures() {
        let err = Error::internal_error().data("failed to read codex app-server message");
        assert!(should_restart_after_error(&err));
    }

    #[test]
    fn restart_is_disabled_for_non_transport_failures() {
        let err = Error::invalid_params().data("prompt must include text content");
        assert!(!should_restart_after_error(&err));
    }
}
