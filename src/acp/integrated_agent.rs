use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use agent_client_protocol::{
    Agent, AgentCapabilities, AgentSideConnection, AuthenticateRequest, AuthenticateResponse,
    CancelNotification, Client, ContentBlock, ContentChunk, Error, Implementation,
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, ProtocolVersion, SessionId, SessionNotification, SessionUpdate, TextContent,
    ToolCall, ToolCallUpdate, ToolCallUpdateFields,
};
use tokio::sync::{Mutex, mpsc};

use super::backend::{BackendEvent, PromptBackend, PromptBackendImpl, PromptRunContext};
pub(super) struct IntegratedAcpAgent {
    sessions: Mutex<HashMap<SessionId, AgentSession>>,
    client: Mutex<Option<Rc<AgentSideConnection>>>,
    next_session_id: AtomicU64,
    next_tool_call_id: AtomicU64,
    stream_chunk_delay: std::time::Duration,
    backend: Rc<PromptBackendImpl>,
}

#[derive(Debug, Clone)]
struct AgentSession {
    cwd: PathBuf,
    cancel_requested: Arc<AtomicBool>,
}

impl IntegratedAcpAgent {
    pub(super) fn new(
        stream_chunk_delay: std::time::Duration,
        backend: Rc<PromptBackendImpl>,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            client: Mutex::new(None),
            next_session_id: AtomicU64::new(1),
            next_tool_call_id: AtomicU64::new(1),
            stream_chunk_delay,
            backend,
        }
    }

    pub(super) async fn attach_client(&self, client: Rc<AgentSideConnection>) {
        *self.client.lock().await = Some(client);
    }

    async fn notify(
        &self,
        session_id: SessionId,
        update: SessionUpdate,
    ) -> agent_client_protocol::Result<()> {
        let client = self
            .client
            .lock()
            .await
            .clone()
            .ok_or_else(|| Error::internal_error().data("agent transport is not attached"))?;

        client
            .session_notification(SessionNotification::new(session_id, update))
            .await
    }

    async fn find_session(
        &self,
        session_id: &SessionId,
    ) -> agent_client_protocol::Result<AgentSession> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(Error::invalid_params)
    }

    fn next_session_id(&self) -> SessionId {
        let id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        SessionId::new(format!("session-{id}"))
    }

    fn next_tool_call_id(&self) -> String {
        let id = self.next_tool_call_id.fetch_add(1, Ordering::Relaxed);
        format!("tool-{id}")
    }

    async fn emit_backend_event(
        &self,
        session_id: SessionId,
        event: BackendEvent,
    ) -> agent_client_protocol::Result<()> {
        match event {
            BackendEvent::ToolStart {
                tool_call_id,
                title,
                status,
            } => {
                self.notify(
                    session_id,
                    SessionUpdate::ToolCall(ToolCall::new(tool_call_id, title).status(status)),
                )
                .await
            }
            BackendEvent::MessageChunk(text) => {
                self.notify(
                    session_id,
                    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                        TextContent::new(text),
                    ))),
                )
                .await
            }
            BackendEvent::ToolUpdate {
                tool_call_id,
                status,
            } => {
                self.notify(
                    session_id,
                    SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                        tool_call_id,
                        ToolCallUpdateFields::new().status(status),
                    )),
                )
                .await
            }
        }
    }
}

#[async_trait::async_trait(?Send)]
impl Agent for IntegratedAcpAgent {
    async fn initialize(
        &self,
        _args: InitializeRequest,
    ) -> agent_client_protocol::Result<InitializeResponse> {
        Ok(InitializeResponse::new(ProtocolVersion::LATEST)
            .agent_capabilities(AgentCapabilities::new())
            .agent_info(Implementation::new(
                "integrated-codex-acp",
                env!("CARGO_PKG_VERSION"),
            )))
    }

    async fn authenticate(
        &self,
        _args: AuthenticateRequest,
    ) -> agent_client_protocol::Result<AuthenticateResponse> {
        Ok(AuthenticateResponse::new())
    }

    async fn new_session(
        &self,
        args: NewSessionRequest,
    ) -> agent_client_protocol::Result<NewSessionResponse> {
        let session_id = self.next_session_id();
        let cwd = resolve_session_cwd(&args.cwd);

        self.sessions.lock().await.insert(
            session_id.clone(),
            AgentSession {
                cwd,
                cancel_requested: Arc::new(AtomicBool::new(false)),
            },
        );

        Ok(NewSessionResponse::new(session_id))
    }

    async fn prompt(&self, args: PromptRequest) -> agent_client_protocol::Result<PromptResponse> {
        let session = self.find_session(&args.session_id).await?;
        session.cancel_requested.store(false, Ordering::Relaxed);

        let prompt_text = extract_prompt_text(&args.prompt);
        if prompt_text.trim().is_empty() {
            return Err(Error::invalid_params().data("prompt must include text content"));
        }

        let tool_call_id = self.next_tool_call_id();
        let context = PromptRunContext {
            session_key: args.session_id.0.to_string(),
            cwd: session.cwd.clone(),
            prompt_text,
            tool_call_id,
            cancel_requested: session.cancel_requested,
            stream_chunk_delay: self.stream_chunk_delay,
        };
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let backend = self.backend.clone();
        let backend_task =
            tokio::task::spawn_local(async move { backend.run_prompt(context, event_tx).await });
        tokio::pin!(backend_task);

        let stop_reason = loop {
            tokio::select! {
                backend_result = &mut backend_task => {
                    let reason = backend_result
                        .map_err(|err| Error::internal_error().data(format!("backend task join failed: {err}")))??;
                    while let Ok(event) = event_rx.try_recv() {
                        self.emit_backend_event(args.session_id.clone(), event).await?;
                    }
                    break reason;
                }
                maybe_event = event_rx.recv() => {
                    if let Some(event) = maybe_event {
                        self.emit_backend_event(args.session_id.clone(), event).await?;
                    }
                }
            }
        };

        Ok(PromptResponse::new(stop_reason))
    }

    async fn cancel(&self, args: CancelNotification) -> agent_client_protocol::Result<()> {
        if let Some(session) = self.sessions.lock().await.get(&args.session_id) {
            session.cancel_requested.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn extract_prompt_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn resolve_session_cwd(session_cwd: &Path) -> PathBuf {
    if session_cwd.is_absolute() {
        session_cwd.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(base_dir) => base_dir.join(session_cwd),
            Err(_) => session_cwd.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_prompt_text_reads_only_text_content() {
        let blocks = vec![
            ContentBlock::Text(TextContent::new("first line")),
            ContentBlock::Text(TextContent::new("second line")),
        ];

        let text = extract_prompt_text(&blocks);
        assert_eq!(text, "first line\nsecond line");
    }

    #[test]
    fn resolve_session_cwd_expands_relative_path() {
        let current = std::env::current_dir().expect("current dir");
        let resolved = resolve_session_cwd(Path::new("workspace"));
        assert_eq!(resolved, current.join("workspace"));
    }
}
