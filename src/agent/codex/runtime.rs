use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::CodexAgentConfig;

const CODEX_APP_SERVER_SUBCOMMAND: &str = "app-server";
const CODEX_APP_SERVER_LISTEN_FLAG: &str = "--listen";
const CODEX_APP_SERVER_STDIO_URI: &str = "stdio://";
const CODEX_DEFAULT_BINARY: &str = "codex";

#[derive(Debug, Clone)]
pub(crate) struct CodexRuntime {
    outbound_tx: mpsc::UnboundedSender<OutboundMessage>,
    pending_requests: Arc<StdMutex<HashMap<u64, oneshot::Sender<acp::Result<Value>>>>>,
    turn_waiters: Arc<StdMutex<HashMap<String, oneshot::Sender<TurnCompletion>>>>,
    completed_turns: Arc<StdMutex<HashMap<String, TurnCompletion>>>,
    next_request_id: Arc<AtomicU64>,
    _child: Arc<AsyncMutex<Child>>,
    _reader_task: Arc<tokio::task::JoinHandle<()>>,
    _writer_task: Arc<tokio::task::JoinHandle<()>>,
}

impl CodexRuntime {
    pub(crate) async fn spawn(config: &CodexAgentConfig) -> acp::Result<Self> {
        let spec = command_spec(config.binary_path.clone());
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::inherit());
        command.kill_on_drop(true);

        let mut child = command.spawn().map_err(|err| {
            acp::Error::internal_error().data(format!(
                "failed to spawn codex app-server using `{}`: {err}",
                spec.program.display()
            ))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            acp::Error::internal_error().data("codex app-server stdin was not captured")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            acp::Error::internal_error().data("codex app-server stdout was not captured")
        })?;

        let pending_requests = Arc::new(StdMutex::new(HashMap::new()));
        let turn_waiters = Arc::new(StdMutex::new(HashMap::new()));
        let completed_turns = Arc::new(StdMutex::new(HashMap::new()));
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();

        let writer_task = {
            let pending = pending_requests.clone();
            tokio::spawn(writer_loop(stdin, outbound_rx, pending))
        };

        let reader_task = {
            let pending = pending_requests.clone();
            let waiters = turn_waiters.clone();
            let completed = completed_turns.clone();
            tokio::spawn(reader_loop(stdout, pending, waiters, completed))
        };

        Ok(Self {
            outbound_tx,
            pending_requests,
            turn_waiters,
            completed_turns,
            next_request_id: Arc::new(AtomicU64::new(1)),
            _child: Arc::new(AsyncMutex::new(child)),
            _reader_task: Arc::new(reader_task),
            _writer_task: Arc::new(writer_task),
        })
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> acp::Result<Value> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: request_id,
            method: method.to_string(),
            params,
        };
        let (tx, rx) = oneshot::channel();
        self.pending_requests
            .lock()
            .expect("pending_requests lock poisoned")
            .insert(request_id, tx);
        self.outbound_tx
            .send(OutboundMessage::Request(request))
            .map_err(|_| {
                self.pending_requests
                    .lock()
                    .expect("pending_requests lock poisoned")
                    .remove(&request_id);
                acp::Error::internal_error().data("failed to send request to codex app-server")
            })?;
        rx.await.map_err(|_| {
            acp::Error::internal_error().data("failed to receive response from codex app-server")
        })?
    }

    pub(crate) fn notify(&self, method: &str, params: Value) -> acp::Result<()> {
        self.outbound_tx
            .send(OutboundMessage::Notification(JsonRpcNotification {
                jsonrpc: "2.0",
                method: method.to_string(),
                params,
            }))
            .map_err(|_| acp::Error::internal_error().data("failed to send notification"))
    }

    pub(crate) async fn wait_turn_completion(&self, turn_id: &str) -> acp::Result<TurnCompletion> {
        if let Some(done) = self
            .completed_turns
            .lock()
            .expect("completed_turns lock poisoned")
            .remove(turn_id)
        {
            return Ok(done);
        }

        let (tx, rx) = oneshot::channel();
        self.turn_waiters
            .lock()
            .expect("turn_waiters lock poisoned")
            .insert(turn_id.to_string(), tx);

        match rx.await {
            Ok(completion) => Ok(completion),
            Err(_) => {
                self.turn_waiters
                    .lock()
                    .expect("turn_waiters lock poisoned")
                    .remove(turn_id);
                Err(acp::Error::internal_error().data("failed waiting for turn completion"))
            }
        }
    }
}

#[derive(Debug)]
struct CommandSpec {
    program: PathBuf,
    args: Vec<String>,
}

fn command_spec(binary_path: Option<PathBuf>) -> CommandSpec {
    let program = binary_path.unwrap_or_else(|| PathBuf::from(CODEX_DEFAULT_BINARY));
    let args = vec![
        CODEX_APP_SERVER_SUBCOMMAND.to_string(),
        CODEX_APP_SERVER_LISTEN_FLAG.to_string(),
        CODEX_APP_SERVER_STDIO_URI.to_string(),
    ];
    CommandSpec { program, args }
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    params: Value,
}

#[derive(Debug)]
enum OutboundMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
}

#[derive(Debug, Deserialize)]
struct IncomingMessage {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<IncomingError>,
}

#[derive(Debug, Deserialize)]
struct IncomingError {
    code: i32,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnCompletion {
    pub(crate) status: TurnCompletionStatus,
    pub(crate) error_message: Option<String>,
    pub(crate) turn_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnCompletionStatus {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Deserialize)]
struct TurnCompletedNotification {
    turn: TurnInfo,
}

#[derive(Debug, Deserialize)]
struct TurnInfo {
    id: String,
    status: String,
    #[serde(default)]
    error: Option<TurnError>,
}

#[derive(Debug, Deserialize)]
struct TurnError {
    message: String,
    #[serde(default, rename = "additionalDetails")]
    additional_details: Option<String>,
}

async fn writer_loop(
    mut stdin: ChildStdin,
    mut outbound_rx: mpsc::UnboundedReceiver<OutboundMessage>,
    pending_requests: Arc<StdMutex<HashMap<u64, oneshot::Sender<acp::Result<Value>>>>>,
) {
    while let Some(msg) = outbound_rx.recv().await {
        let bytes = match msg {
            OutboundMessage::Request(req) => match serde_json::to_vec(&req) {
                Ok(v) => v,
                Err(err) => {
                    fail_all_pending(
                        &pending_requests,
                        acp::Error::internal_error().data(format!(
                            "failed to serialize request for codex app-server: {err}"
                        )),
                    );
                    return;
                }
            },
            OutboundMessage::Notification(note) => match serde_json::to_vec(&note) {
                Ok(v) => v,
                Err(_) => continue,
            },
        };

        if stdin.write_all(&bytes).await.is_err() || stdin.write_all(b"\n").await.is_err() {
            fail_all_pending(
                &pending_requests,
                acp::Error::internal_error().data("failed writing to codex app-server stdin"),
            );
            return;
        }
        let _ = stdin.flush().await;
    }
}

async fn reader_loop(
    stdout: ChildStdout,
    pending_requests: Arc<StdMutex<HashMap<u64, oneshot::Sender<acp::Result<Value>>>>>,
    turn_waiters: Arc<StdMutex<HashMap<String, oneshot::Sender<TurnCompletion>>>>,
    completed_turns: Arc<StdMutex<HashMap<String, TurnCompletion>>>,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let incoming = match serde_json::from_str::<IncomingMessage>(&line) {
            Ok(msg) => msg,
            Err(_) => continue,
        };

        if let Some(id) = parse_id(incoming.id.as_ref()) {
            let sender = pending_requests
                .lock()
                .expect("pending_requests lock poisoned")
                .remove(&id);
            if let Some(sender) = sender {
                let result = if let Some(err) = incoming.error {
                    Err(acp::Error::new(err.code, err.message).data(err.data))
                } else {
                    Ok(incoming.result.unwrap_or(Value::Null))
                };
                let _ = sender.send(result);
            }
            continue;
        }

        if incoming.method.as_deref() == Some("turn/completed")
            && let Some(params) = incoming.params
            && let Some(completion) = parse_turn_completion(params)
        {
            let waiter = turn_waiters
                .lock()
                .expect("turn_waiters lock poisoned")
                .remove(&completion.turn_id);
            if let Some(waiter) = waiter {
                let _ = waiter.send(completion);
            } else {
                completed_turns
                    .lock()
                    .expect("completed_turns lock poisoned")
                    .insert(completion.turn_id.clone(), completion);
            }
        }
    }

    fail_all_pending(
        &pending_requests,
        acp::Error::internal_error().data("codex app-server closed unexpectedly"),
    );
    fail_all_turn_waiters(
        &turn_waiters,
        TurnCompletion {
            turn_id: String::new(),
            status: TurnCompletionStatus::Failed,
            error_message: Some("codex app-server closed unexpectedly".to_string()),
        },
    );
}

fn parse_id(raw: Option<&Value>) -> Option<u64> {
    let raw = raw?;
    match raw {
        Value::Number(num) => num.as_u64(),
        Value::String(text) => text.parse::<u64>().ok(),
        _ => None,
    }
}

fn parse_turn_completion(params: Value) -> Option<TurnCompletion> {
    let parsed = serde_json::from_value::<TurnCompletedNotification>(params).ok()?;
    let status = match parsed.turn.status.as_str() {
        "completed" => TurnCompletionStatus::Completed,
        "interrupted" => TurnCompletionStatus::Interrupted,
        "failed" => TurnCompletionStatus::Failed,
        _ => return None,
    };
    let error_message = parsed.turn.error.as_ref().map(|err| {
        let details = err.additional_details.clone().unwrap_or_default();
        if details.is_empty() {
            err.message.clone()
        } else {
            format!("{}: {details}", err.message)
        }
    });
    Some(TurnCompletion {
        turn_id: parsed.turn.id,
        status,
        error_message,
    })
}

fn fail_all_pending(
    pending_requests: &Arc<StdMutex<HashMap<u64, oneshot::Sender<acp::Result<Value>>>>>,
    err: acp::Error,
) {
    let mut pending = pending_requests
        .lock()
        .expect("pending_requests lock poisoned");
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(err.clone()));
    }
}

fn fail_all_turn_waiters(
    turn_waiters: &Arc<StdMutex<HashMap<String, oneshot::Sender<TurnCompletion>>>>,
    fallback: TurnCompletion,
) {
    let mut waiters = turn_waiters.lock().expect("turn_waiters lock poisoned");
    for (turn_id, sender) in waiters.drain() {
        let mut done = fallback.clone();
        done.turn_id = turn_id;
        let _ = sender.send(done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_spec_defaults_to_codex_binary() {
        let spec = command_spec(None);
        assert_eq!(spec.program, PathBuf::from("codex"));
        assert_eq!(
            spec.args,
            vec!["app-server", "--listen", "stdio://"]
                .into_iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn command_spec_respects_binary_override() {
        let spec = command_spec(Some(PathBuf::from("./tools/codex")));
        assert_eq!(spec.program, PathBuf::from("./tools/codex"));
        assert!(!spec.program.to_string_lossy().contains("codex-acp"));
    }

    #[test]
    fn parse_turn_completion_supports_completed_and_failed() {
        let completed = parse_turn_completion(serde_json::json!({
            "threadId": "thread-1",
            "turn": { "id": "turn-1", "status": "completed", "error": null }
        }))
        .expect("completed turn should parse");
        assert_eq!(completed.status, TurnCompletionStatus::Completed);

        let failed = parse_turn_completion(serde_json::json!({
            "threadId": "thread-1",
            "turn": {
                "id": "turn-2",
                "status": "failed",
                "error": { "message": "boom", "additionalDetails": "network" }
            }
        }))
        .expect("failed turn should parse");
        assert_eq!(failed.status, TurnCompletionStatus::Failed);
        assert_eq!(failed.error_message.as_deref(), Some("boom: network"));
    }
}
