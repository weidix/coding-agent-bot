mod backend;
mod bootstrap;
mod codex_rpc;
mod integrated_agent;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agent_client_protocol::{
    Agent, AgentSideConnection, CancelNotification, Client, ClientCapabilities,
    ClientSideConnection, ContentBlock, Error, Implementation, InitializeRequest, PermissionOption,
    PermissionOptionKind, ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    StopReason, TextContent, ToolCallStatus,
};
use anyhow::{Context, Result, anyhow};
use tokio::io::{duplex, split};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::task_types::{TaskEvent, TaskEventKind, TaskId};

use self::backend::{CodexBackend, PromptBackendImpl};
use self::integrated_agent::IntegratedAcpAgent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpBackend {
    Codex,
}

impl Default for AcpBackend {
    fn default() -> Self {
        Self::Codex
    }
}

impl std::str::FromStr for AcpBackend {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            other => Err(format!(
                "unsupported backend '{other}', currently only supports 'codex'"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcpRuntimeConfig {
    pub backend: AcpBackend,
    pub codex_binary_path: Option<PathBuf>,
    pub codex_model: Option<String>,
    pub codex_startup_timeout_ms: u64,
    pub stream_chunk_delay_ms: u64,
    pub io_channel_buffer_size: usize,
}

impl Default for AcpRuntimeConfig {
    fn default() -> Self {
        Self {
            backend: AcpBackend::default(),
            codex_binary_path: None,
            codex_model: None,
            codex_startup_timeout_ms: 5_000,
            stream_chunk_delay_ms: 25,
            io_channel_buffer_size: 64 * 1024,
        }
    }
}

#[derive(Debug)]
pub enum AcpWorkerCommand {
    Prompt(String),
    Cancel,
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct EventEmitter {
    sequence: Arc<AtomicU64>,
    event_tx: broadcast::Sender<TaskEvent>,
    task_id: TaskId,
    chat_id: i64,
    user_id: i64,
}

impl EventEmitter {
    pub fn new(
        sequence: Arc<AtomicU64>,
        event_tx: broadcast::Sender<TaskEvent>,
        task_id: TaskId,
        chat_id: i64,
        user_id: i64,
    ) -> Self {
        Self {
            sequence,
            event_tx,
            task_id,
            chat_id,
            user_id,
        }
    }

    pub fn emit(&self, kind: TaskEventKind) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let _ = self.event_tx.send(TaskEvent {
            sequence,
            task_id: self.task_id,
            chat_id: self.chat_id,
            user_id: self.user_id,
            kind,
        });
    }
}

pub fn spawn_acp_worker(
    runtime_cfg: AcpRuntimeConfig,
    task_id: TaskId,
    cwd: PathBuf,
    event_emitter: EventEmitter,
) -> Result<mpsc::UnboundedSender<AcpWorkerCommand>> {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let emitter_for_thread = event_emitter.clone();

    std::thread::Builder::new()
        .name(format!("integrated-acp-task-{task_id}"))
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();

            match runtime {
                Ok(rt) => {
                    if let Err(err) = rt.block_on(run_worker(
                        runtime_cfg,
                        command_rx,
                        cwd,
                        emitter_for_thread.clone(),
                    )) {
                        emitter_for_thread.emit(TaskEventKind::Error {
                            message: format!("task runtime failed: {err}"),
                        });
                    }
                }
                Err(err) => {
                    emitter_for_thread.emit(TaskEventKind::Error {
                        message: format!("failed to start runtime: {err}"),
                    });
                }
            }
        })
        .with_context(|| format!("failed to spawn task thread for task {task_id}"))?;

    event_emitter.emit(TaskEventKind::Created);
    Ok(command_tx)
}

async fn run_worker(
    runtime_cfg: AcpRuntimeConfig,
    mut command_rx: mpsc::UnboundedReceiver<AcpWorkerCommand>,
    cwd: PathBuf,
    event_emitter: EventEmitter,
) -> Result<()> {
    let client = AcpClient {
        event_emitter: event_emitter.clone(),
    };

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            let (client_stream, agent_stream) = duplex(runtime_cfg.io_channel_buffer_size);
            let (client_reader, client_writer) = split(client_stream);
            let (agent_reader, agent_writer) = split(agent_stream);

            let (connection, client_io_task) = ClientSideConnection::new(
                client,
                client_writer.compat_write(),
                client_reader.compat(),
                |future| {
                    tokio::task::spawn_local(future);
                },
            );

            let integrated_agent = std::rc::Rc::new(IntegratedAcpAgent::new(
                Duration::from_millis(runtime_cfg.stream_chunk_delay_ms),
                build_prompt_backend(&runtime_cfg),
            ));
            let (agent_connection, agent_io_task) = AgentSideConnection::new(
                integrated_agent.clone(),
                agent_writer.compat_write(),
                agent_reader.compat(),
                |future| {
                    tokio::task::spawn_local(future);
                },
            );
            integrated_agent
                .attach_client(std::rc::Rc::new(agent_connection))
                .await;

            tokio::task::spawn_local({
                let event_emitter = event_emitter.clone();
                async move {
                    if let Err(err) = client_io_task.await {
                        event_emitter.emit(TaskEventKind::Error {
                            message: format!("acp client io loop failed: {err}"),
                        });
                    }
                }
            });
            tokio::task::spawn_local({
                let event_emitter = event_emitter.clone();
                async move {
                    if let Err(err) = agent_io_task.await {
                        event_emitter.emit(TaskEventKind::Error {
                            message: format!("acp agent io loop failed: {err}"),
                        });
                    }
                }
            });

            let agent = Arc::new(connection);

            initialize_session(agent.clone(), &cwd).await?;
            let session = agent.new_session(agent_client_protocol::NewSessionRequest::new(&cwd));
            let session_id = session
                .await
                .map_err(|err| anyhow!("failed to create acp session: {err}"))?
                .session_id;

            let mut pending_prompt: Option<
                oneshot::Receiver<
                    agent_client_protocol::Result<agent_client_protocol::PromptResponse>,
                >,
            > = None;

            loop {
                if let Some(receiver) = &mut pending_prompt {
                    tokio::select! {
                        result = receiver => {
                            pending_prompt = None;
                            match result {
                                Ok(Ok(response)) => {
                                    event_emitter.emit(TaskEventKind::Finished {
                                        stop_reason: stop_reason_label(response.stop_reason),
                                    });
                                }
                                Ok(Err(err)) => {
                                    event_emitter.emit(TaskEventKind::Error {
                                        message: format!("prompt failed: {err}"),
                                    });
                                }
                                Err(err) => {
                                    event_emitter.emit(TaskEventKind::Error {
                                        message: format!("prompt worker channel failed: {err}"),
                                    });
                                }
                            }
                        }
                        command = command_rx.recv() => {
                            if !handle_command_while_running(
                                command,
                                agent.clone(),
                                session_id.clone(),
                                &event_emitter,
                            ).await? {
                                break;
                            }
                        }
                    }
                } else {
                    let Some(command) = command_rx.recv().await else {
                        break;
                    };

                    match command {
                        AcpWorkerCommand::Prompt(text) => {
                            let prompt_request = agent_client_protocol::PromptRequest::new(
                                session_id.clone(),
                                vec![ContentBlock::Text(TextContent::new(text))],
                            );
                            let (tx, rx) = oneshot::channel();
                            let agent = agent.clone();
                            tokio::task::spawn_local(async move {
                                let response = agent.prompt(prompt_request).await;
                                let _ = tx.send(response);
                            });

                            pending_prompt = Some(rx);
                            event_emitter.emit(TaskEventKind::StartedPrompt);
                        }
                        AcpWorkerCommand::Cancel => {
                            event_emitter.emit(TaskEventKind::Cancelled);
                        }
                        AcpWorkerCommand::Shutdown => {
                            break;
                        }
                    }
                }
            }

            Ok(())
        })
        .await
}

fn build_prompt_backend(runtime_cfg: &AcpRuntimeConfig) -> Rc<PromptBackendImpl> {
    match runtime_cfg.backend {
        AcpBackend::Codex => Rc::new(PromptBackendImpl::Codex(CodexBackend::new(
            runtime_cfg.codex_binary_path.clone(),
            runtime_cfg.codex_model.clone(),
            Duration::from_millis(runtime_cfg.codex_startup_timeout_ms),
        ))),
    }
}

async fn initialize_session(agent: Arc<ClientSideConnection>, cwd: &PathBuf) -> Result<()> {
    let initialize = InitializeRequest::new(ProtocolVersion::LATEST)
        .client_capabilities(ClientCapabilities::new())
        .client_info(Implementation::new(
            "coding-agent-bot",
            env!("CARGO_PKG_VERSION"),
        ));

    agent.initialize(initialize).await.map_err(|err| {
        anyhow!(
            "failed to initialize in-process acp client at {}: {err}",
            cwd.display()
        )
    })?;

    Ok(())
}

async fn handle_command_while_running(
    command: Option<AcpWorkerCommand>,
    agent: Arc<ClientSideConnection>,
    session_id: agent_client_protocol::SessionId,
    event_emitter: &EventEmitter,
) -> Result<bool> {
    let Some(command) = command else {
        return Ok(false);
    };

    match command {
        AcpWorkerCommand::Prompt(_) => {
            event_emitter.emit(TaskEventKind::Error {
                message: "a prompt is already running".to_string(),
            });
        }
        AcpWorkerCommand::Cancel => {
            agent
                .cancel(CancelNotification::new(session_id))
                .await
                .map_err(|err| anyhow!("failed to cancel prompt: {err}"))?;
            event_emitter.emit(TaskEventKind::Cancelled);
        }
        AcpWorkerCommand::Shutdown => {
            let _ = agent.cancel(CancelNotification::new(session_id)).await;
            return Ok(false);
        }
    }

    Ok(true)
}

struct AcpClient {
    event_emitter: EventEmitter,
}

#[async_trait::async_trait(?Send)]
impl Client for AcpClient {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> agent_client_protocol::Result<RequestPermissionResponse> {
        let allow = true;
        let selected = select_permission_option(&args.options, allow)
            .or_else(|| args.options.first().map(|option| option.option_id.clone()))
            .ok_or_else(Error::invalid_params)?;

        let outcome = RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(selected));

        Ok(RequestPermissionResponse::new(outcome))
    }

    async fn session_notification(
        &self,
        args: SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        match args.update {
            SessionUpdate::AgentMessageChunk(chunk) | SessionUpdate::AgentThoughtChunk(chunk) => {
                if let Some(text) = extract_text(chunk.content) {
                    self.event_emitter.emit(TaskEventKind::OutputChunk { text });
                }
            }
            SessionUpdate::ToolCall(call) => {
                let text = format!("[tool] {} ({})", call.title, tool_status_label(call.status));
                self.event_emitter.emit(TaskEventKind::ToolUpdate { text });
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let status_text = update
                    .fields
                    .status
                    .map(tool_status_label)
                    .unwrap_or("updated");
                let text = format!("[tool] {} {status_text}", update.tool_call_id.0);
                self.event_emitter.emit(TaskEventKind::ToolUpdate { text });
            }
            _ => {}
        }

        Ok(())
    }
}

fn select_permission_option(
    options: &[PermissionOption],
    allow: bool,
) -> Option<agent_client_protocol::PermissionOptionId> {
    let preferred = if allow {
        [
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::AllowAlways,
        ]
    } else {
        [
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        ]
    };

    preferred.into_iter().find_map(|kind| {
        options
            .iter()
            .find(|option| option.kind == kind)
            .map(|option| option.option_id.clone())
    })
}

fn extract_text(content: ContentBlock) -> Option<String> {
    match content {
        ContentBlock::Text(text) => Some(text.text),
        _ => None,
    }
}

fn tool_status_label(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "unknown",
    }
}

fn stop_reason_label(stop_reason: StopReason) -> String {
    match stop_reason {
        StopReason::EndTurn => "end_turn".to_string(),
        StopReason::MaxTokens => "max_tokens".to_string(),
        StopReason::MaxTurnRequests => "max_turn_requests".to_string(),
        StopReason::Refusal => "refusal".to_string(),
        StopReason::Cancelled => "cancelled".to_string(),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests;
