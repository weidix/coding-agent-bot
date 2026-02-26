use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agent_client_protocol::{Error, StopReason};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::backend::BackendEvent;

pub(super) struct CodexRpcConnection {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: i64,
}

impl CodexRpcConnection {
    pub(super) async fn connect(url: &str) -> agent_client_protocol::Result<Self> {
        let (ws, _) = connect_async(url).await.map_err(|err| {
            internal_error(format!("failed to connect codex app-server {url}: {err}"))
        })?;
        Ok(Self { ws, next_id: 1 })
    }

    pub(super) async fn initialize(&mut self) -> agent_client_protocol::Result<()> {
        let result = self
            .send_request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "coding-agent-bot",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {},
                }),
            )
            .await?;

        if result.get("userAgent").and_then(Value::as_str).is_none() {
            return Err(internal_error(
                "codex initialize response is missing userAgent",
            ));
        }

        Ok(())
    }

    pub(super) async fn start_thread(
        &mut self,
        cwd: PathBuf,
        model: Option<String>,
    ) -> agent_client_protocol::Result<String> {
        let mut params = json!({
            "cwd": cwd.to_string_lossy(),
            "approvalPolicy": "never",
            "sandbox": {
                "type": "workspaceWrite",
                "networkAccess": false,
                "writableRoots": [cwd.to_string_lossy()],
            }
        });
        if let Some(model) = model {
            params["model"] = Value::String(model);
        }

        let result = self.send_request("thread/start", params).await?;
        let thread_id = result
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| internal_error("thread/start response missing thread.id"))?;

        Ok(thread_id.to_string())
    }

    pub(super) async fn start_turn(
        &mut self,
        thread_id: &str,
        prompt_text: String,
        model: Option<String>,
    ) -> agent_client_protocol::Result<String> {
        let mut params = json!({
            "threadId": thread_id,
            "input": [
                {
                    "type": "text",
                    "text": prompt_text,
                }
            ]
        });
        if let Some(model) = model {
            params["model"] = Value::String(model);
        }

        let result = self.send_request("turn/start", params).await?;
        let turn_id = result
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| internal_error("turn/start response missing turn.id"))?;

        Ok(turn_id.to_string())
    }

    pub(super) async fn wait_turn_completion(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        cancel_requested: &Arc<AtomicBool>,
        event_tx: &mpsc::UnboundedSender<BackendEvent>,
        poll_interval: Duration,
    ) -> agent_client_protocol::Result<StopReason> {
        let mut interrupt_sent = false;
        let mut interrupt_request_id: Option<i64> = None;

        loop {
            tokio::select! {
                _ = tokio::time::sleep(poll_interval), if !interrupt_sent && cancel_requested.load(Ordering::Relaxed) => {
                    let request_id = self.send_request_without_wait("turn/interrupt", json!({
                        "threadId": thread_id,
                        "turnId": turn_id,
                    })).await?;
                    interrupt_sent = true;
                    interrupt_request_id = Some(request_id);
                }
                raw = self.read_next_json() => {
                    let message = raw?;
                    if let Some(method) = message.get("method").and_then(Value::as_str) {
                        if let Some(id) = message.get("id") {
                            self.respond_unsupported(id.clone()).await?;
                            continue;
                        }

                        let params = message.get("params").cloned().unwrap_or(Value::Null);
                        if let Some(stop_reason) =
                            self.handle_notification(method, &params, thread_id, turn_id, event_tx)?
                        {
                            return Ok(stop_reason);
                        }
                        continue;
                    }

                    if let Some(id_value) = message.get("id") {
                        if let Some(error) = message.get("error") {
                            return Err(internal_error(format!(
                                "codex app-server error response: {error}"
                            )));
                        }

                        if let Some(pending_interrupt_id) = interrupt_request_id
                            && id_value.as_i64() == Some(pending_interrupt_id)
                        {
                            continue;
                        }
                    }
                }
            }
        }
    }

    pub(super) async fn send_notification(
        &mut self,
        method: &str,
        params: Value,
    ) -> agent_client_protocol::Result<()> {
        self.send_json(json!({
            "method": method,
            "params": params,
        }))
        .await
    }

    fn handle_notification(
        &self,
        method: &str,
        params: &Value,
        thread_id: &str,
        turn_id: &str,
        event_tx: &mpsc::UnboundedSender<BackendEvent>,
    ) -> agent_client_protocol::Result<Option<StopReason>> {
        match method {
            "item/agentMessage/delta" => {
                let same_thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(|id| id == thread_id)
                    .unwrap_or(false);
                let same_turn = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .map(|id| id == turn_id)
                    .unwrap_or(false);
                if same_thread
                    && same_turn
                    && let Some(delta) = params.get("delta").and_then(Value::as_str)
                {
                    let _ = event_tx.send(BackendEvent::MessageChunk(delta.to_string()));
                }
            }
            "item/mcpToolCall/progress" => {
                let same_thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(|id| id == thread_id)
                    .unwrap_or(false);
                let same_turn = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .map(|id| id == turn_id)
                    .unwrap_or(false);
                if same_thread
                    && same_turn
                    && let Some(message) = params.get("message").and_then(Value::as_str)
                {
                    let _ =
                        event_tx.send(BackendEvent::MessageChunk(format!("\n[tool] {message}")));
                }
            }
            "turn/completed" => {
                let same_thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(|id| id == thread_id)
                    .unwrap_or(false);
                let same_turn = params
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .map(|id| id == turn_id)
                    .unwrap_or(false);
                if same_thread && same_turn {
                    let status = params
                        .get("turn")
                        .and_then(|turn| turn.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("failed");
                    let stop_reason = match status {
                        "completed" => StopReason::EndTurn,
                        "interrupted" => StopReason::Cancelled,
                        _ => StopReason::Refusal,
                    };
                    return Ok(Some(stop_reason));
                }
            }
            _ => {}
        }

        Ok(None)
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> agent_client_protocol::Result<Value> {
        let id = self.send_request_without_wait(method, params).await?;
        loop {
            let message = self.read_next_json().await?;

            if let Some(received_method) = message.get("method").and_then(Value::as_str) {
                if let Some(request_id) = message.get("id") {
                    self.respond_unsupported(request_id.clone()).await?;
                    continue;
                }

                if received_method == "error" {
                    let params = message.get("params").cloned().unwrap_or(Value::Null);
                    return Err(internal_error(format!(
                        "codex app-server notification error: {params}"
                    )));
                }
                continue;
            }

            let Some(received_id) = message.get("id").and_then(Value::as_i64) else {
                continue;
            };
            if received_id != id {
                continue;
            }

            if let Some(error) = message.get("error") {
                return Err(internal_error(format!(
                    "codex app-server request '{method}' failed: {error}"
                )));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn send_request_without_wait(
        &mut self,
        method: &str,
        params: Value,
    ) -> agent_client_protocol::Result<i64> {
        let id = self.next_id;
        self.next_id += 1;

        self.send_json(json!({
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        Ok(id)
    }

    async fn respond_unsupported(&mut self, id: Value) -> agent_client_protocol::Result<()> {
        self.send_json(json!({
            "id": id,
            "error": {
                "code": -32601,
                "message": "method is not supported by coding-agent-bot",
            }
        }))
        .await
    }

    async fn send_json(&mut self, value: Value) -> agent_client_protocol::Result<()> {
        self.ws
            .send(Message::Text(value.to_string().into()))
            .await
            .map_err(|err| {
                internal_error(format!("failed to send codex app-server message: {err}"))
            })
    }

    async fn read_next_json(&mut self) -> agent_client_protocol::Result<Value> {
        loop {
            let frame = self
                .ws
                .next()
                .await
                .ok_or_else(|| internal_error("codex app-server websocket closed"))?
                .map_err(|err| {
                    internal_error(format!("failed to read codex app-server message: {err}"))
                })?;

            match frame {
                Message::Text(text) => {
                    return serde_json::from_str::<Value>(&text).map_err(|err| {
                        internal_error(format!(
                            "failed to decode codex app-server json message: {err}"
                        ))
                    });
                }
                Message::Binary(data) => {
                    return serde_json::from_slice::<Value>(&data).map_err(|err| {
                        internal_error(format!(
                            "failed to decode codex app-server binary message: {err}"
                        ))
                    });
                }
                Message::Ping(payload) => {
                    self.ws.send(Message::Pong(payload)).await.map_err(|err| {
                        internal_error(format!("failed to send websocket pong: {err}"))
                    })?;
                }
                Message::Pong(_) => {}
                Message::Close(_) => {
                    return Err(internal_error("codex app-server websocket closed"));
                }
                _ => {}
            }
        }
    }
}

fn internal_error(message: impl Into<String>) -> Error {
    Error::internal_error().data(message.into())
}
