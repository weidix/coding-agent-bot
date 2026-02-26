use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;

static CODEX_SERVER_STATE: OnceLock<Mutex<CodexServerState>> = OnceLock::new();

#[derive(Default)]
struct CodexServerState {
    app_server_url: Option<String>,
    child: Option<Child>,
    last_pid: Option<u32>,
}

fn server_state() -> &'static Mutex<CodexServerState> {
    CODEX_SERVER_STATE.get_or_init(|| Mutex::new(CodexServerState::default()))
}

pub async fn ensure_codex_app_server_ready(
    codex_binary_path: Option<&Path>,
    startup_timeout: Duration,
) -> Result<String> {
    let mut state = server_state().lock().await;

    if let Some(url) = state.app_server_url.clone() {
        if probe_codex_server(&url).await.is_ok() {
            return Ok(url);
        }

        tracing::warn!("existing codex app-server at {url} is not reachable, starting a new one");
        terminate_existing_server(&mut state)?;
    }

    let app_server_url = spawn_codex_app_server(&mut state, codex_binary_path)?;
    wait_until_server_ready(&app_server_url, startup_timeout).await?;
    Ok(app_server_url)
}

pub async fn restart_codex_app_server(
    codex_binary_path: Option<&Path>,
    startup_timeout: Duration,
) -> Result<String> {
    let mut state = server_state().lock().await;
    terminate_existing_server(&mut state)?;

    let app_server_url = spawn_codex_app_server(&mut state, codex_binary_path)?;
    wait_until_server_ready(&app_server_url, startup_timeout).await?;
    Ok(app_server_url)
}

fn terminate_existing_server(state: &mut CodexServerState) -> Result<()> {
    state.app_server_url = None;

    let Some(mut child) = state.child.take() else {
        return Ok(());
    };

    match child.try_wait() {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(err) => {
            return Err(anyhow!("failed to inspect codex app-server process: {err}"));
        }
    }

    child
        .kill()
        .with_context(|| "failed to kill unresponsive codex app-server process")?;
    let _ = child.wait();
    Ok(())
}

fn spawn_codex_app_server(
    state: &mut CodexServerState,
    codex_binary_path: Option<&Path>,
) -> Result<String> {
    let port = find_free_local_port()?;
    let app_server_url = format!("ws://127.0.0.1:{port}");

    let executable = codex_binary_path.unwrap_or_else(|| Path::new("codex"));
    let command_text = command_display(executable, port);

    let mut command = Command::new(executable);
    command
        .arg("app-server")
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let child = command
        .spawn()
        .with_context(|| format!("failed to spawn codex app-server command: {command_text}"))?;

    state.last_pid = Some(child.id());
    state.child = Some(child);
    state.app_server_url = Some(app_server_url.clone());

    if let Some(pid) = state.last_pid {
        tracing::info!("started codex app-server process pid={pid}, url={app_server_url}");
    } else {
        tracing::info!("started codex app-server process url={app_server_url}");
    }

    Ok(app_server_url)
}

fn find_free_local_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .with_context(|| "failed to bind local socket for codex app-server")?;
    let port = listener
        .local_addr()
        .with_context(|| "failed to read local socket address for codex app-server")?
        .port();
    Ok(port)
}

async fn wait_until_server_ready(app_server_url: &str, startup_timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + startup_timeout;

    loop {
        let connect_error = match probe_codex_server(app_server_url).await {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };

        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "codex app-server at {app_server_url} did not become ready within {}ms; last error: {connect_error}",
                startup_timeout.as_millis()
            ));
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn probe_codex_server(app_server_url: &str) -> std::result::Result<(), String> {
    let (mut ws, _) = connect_async(app_server_url)
        .await
        .map_err(|err| err.to_string())?;
    ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
        reason: "ready-check".into(),
    }))
    .await
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn command_display(executable: &Path, port: u16) -> String {
    format!("{} app-server --port {port}", executable.display())
}

#[cfg(test)]
mod tests {
    use futures_util::SinkExt;
    use futures_util::StreamExt;
    use serde_json::Value;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    #[test]
    fn command_display_contains_port_argument() {
        let rendered = command_display(Path::new("codex"), 4567);
        assert_eq!(rendered, "codex app-server --port 4567");
    }

    #[test]
    #[ignore = "requires local socket bind capability"]
    fn find_free_local_port_returns_non_zero_port() {
        let port = find_free_local_port().expect("free port");
        assert!(port > 0);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires local websocket listener capability"]
    async fn probe_codex_server_succeeds_when_server_is_online() {
        let (url, _server_task) = spawn_fake_server().await;
        let result = probe_codex_server(&url).await;
        assert!(result.is_ok(), "unexpected error: {result:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires local websocket listener capability"]
    async fn probe_codex_server_fails_when_server_is_offline() {
        let url = unused_local_ws_url().await;
        let result = probe_codex_server(&url).await;
        assert!(result.is_err(), "offline server should fail");
    }

    async fn spawn_fake_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake server");
        let addr = listener.local_addr().expect("local addr");
        let url = format!("ws://{addr}");

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_async(stream).await.expect("accept websocket");

            while let Some(frame) = ws.next().await {
                let frame = frame.expect("frame");
                match frame {
                    Message::Text(text) => {
                        let _parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    }
                    Message::Close(_) => break,
                    Message::Ping(payload) => {
                        ws.send(Message::Pong(payload)).await.expect("pong");
                    }
                    _ => {}
                }
            }
        });

        (url, handle)
    }

    async fn unused_local_ws_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);
        format!("ws://{addr}")
    }
}
