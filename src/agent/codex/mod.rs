use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol as acp;
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time;

use crate::config::CodexConfig;

use self::mapping::{map_stop_reason, parse_thread_id, parse_turn_id, prompt_blocks_to_turn_input};
use self::runtime::CodexRuntime;
use super::Agent;

mod mapping;
mod runtime;

#[derive(Debug, Clone)]
pub struct CodexAgentConfig {
    pub binary_path: Option<PathBuf>,
    pub startup_timeout: Duration,
}

impl From<&CodexConfig> for CodexAgentConfig {
    fn from(value: &CodexConfig) -> Self {
        Self {
            binary_path: value.binary_path.clone(),
            startup_timeout: Duration::from_millis(value.startup_timeout_ms),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodexAgent {
    config: CodexAgentConfig,
    runtime: Arc<AsyncMutex<Option<CodexRuntime>>>,
    sessions: Arc<AsyncMutex<HashMap<acp::SessionId, String>>>,
    active_turns: Arc<AsyncMutex<HashMap<acp::SessionId, String>>>,
}

impl CodexAgent {
    #[must_use]
    pub fn new(config: CodexAgentConfig) -> Self {
        Self {
            config,
            runtime: Arc::new(AsyncMutex::new(None)),
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            active_turns: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn from_codex_config(config: &CodexConfig) -> Self {
        Self::new(config.into())
    }

    async fn ensure_runtime(&self) -> acp::Result<CodexRuntime> {
        let mut guard = self.runtime.lock().await;
        if let Some(runtime) = guard.as_ref() {
            return Ok(runtime.clone());
        }

        let runtime = CodexRuntime::spawn(&self.config).await?;
        let init_params = json!({
            "clientInfo": {
                "name": "coding-agent-bot",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        let initialized = time::timeout(
            self.config.startup_timeout,
            runtime.request("initialize", init_params),
        )
        .await
        .map_err(|_| acp::Error::internal_error().data("codex initialize timed out"))?;
        initialized?;

        *guard = Some(runtime.clone());
        Ok(runtime)
    }

    async fn thread_id_by_session(&self, session_id: &acp::SessionId) -> acp::Result<String> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| acp::Error::resource_not_found(Some(session_id.0.to_string())))
    }
}

#[async_trait(?Send)]
impl Agent for CodexAgent {
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        let _ = self.ensure_runtime().await?;
        Ok(acp::InitializeResponse::new(args.protocol_version)
            .agent_capabilities(
                acp::AgentCapabilities::new()
                    .load_session(true)
                    .prompt_capabilities(acp::PromptCapabilities::new().embedded_context(true)),
            )
            .agent_info(
                acp::Implementation::new("codex", env!("CARGO_PKG_VERSION")).title("Codex"),
            ))
    }

    async fn authenticate(
        &self,
        _args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        Ok(acp::AuthenticateResponse::new())
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        let runtime = self.ensure_runtime().await?;
        let params = json!({
            "cwd": args.cwd,
            "approvalPolicy": "never",
        });
        let result = runtime.request("thread/start", params).await?;
        let thread_id = parse_thread_id(&result)?;
        let session_id = acp::SessionId::new(thread_id.clone());
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), thread_id);
        Ok(acp::NewSessionResponse::new(session_id))
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        let runtime = self.ensure_runtime().await?;
        let params = json!({
            "threadId": args.session_id,
            "cwd": args.cwd,
        });
        let result = runtime.request("thread/resume", params).await?;
        let thread_id = parse_thread_id(&result)?;
        self.sessions
            .lock()
            .await
            .insert(args.session_id, thread_id);
        Ok(acp::LoadSessionResponse::new())
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let runtime = self.ensure_runtime().await?;
        let thread_id = self.thread_id_by_session(&args.session_id).await?;
        let input = prompt_blocks_to_turn_input(&args.prompt)?;
        let params = json!({
            "threadId": thread_id,
            "input": input,
        });
        let result = runtime.request("turn/start", params).await?;
        let turn_id = parse_turn_id(&result)?;

        {
            let mut active_turns = self.active_turns.lock().await;
            if active_turns.contains_key(&args.session_id) {
                return Err(
                    acp::Error::invalid_params().data("a turn is already running for this session")
                );
            }
            active_turns.insert(args.session_id.clone(), turn_id.clone());
        }

        let completion = runtime.wait_turn_completion(&turn_id).await;
        self.active_turns.lock().await.remove(&args.session_id);
        let completion = completion?;
        map_stop_reason(completion)
    }

    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()> {
        let runtime = self.ensure_runtime().await?;
        let thread_id = self.thread_id_by_session(&args.session_id).await?;
        let turn_id = self
            .active_turns
            .lock()
            .await
            .get(&args.session_id)
            .cloned();
        let Some(turn_id) = turn_id else {
            return Ok(());
        };
        runtime.notify(
            "turn/interrupt",
            json!({
                "threadId": thread_id,
                "turnId": turn_id,
            }),
        )
    }
}
