use anyhow::{Result, anyhow};
use teloxide::Bot;

use crate::acp::{AcpBackend, AcpRuntimeConfig};
use crate::config::AppConfig;
use crate::task_manager::TaskManager;
use crate::telegram::{TelegramRuntime, run_telegram};

pub async fn run_from_default_config() -> Result<()> {
    let config = AppConfig::load_default()?;
    run(config).await
}

pub async fn run(config: AppConfig) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .without_time()
        .init();

    let runtime_cfg = AcpRuntimeConfig {
        backend: AcpBackend::default(),
        codex_binary_path: config.codex.binary_path.clone(),
        codex_model: None,
        codex_startup_timeout_ms: config.codex.startup_timeout_ms,
        stream_chunk_delay_ms: config.acp.stream_chunk_delay_ms,
        io_channel_buffer_size: config.acp.io_channel_buffer_size,
    };

    let manager = TaskManager::new(runtime_cfg, config.acp.max_running_tasks);

    if config.telegram.enabled {
        let telegram_runtime = TelegramRuntime::new(manager.clone(), config.telegram.clone());
        let bot = Bot::new(config.telegram.bot_token.clone());
        tokio::spawn(async move {
            run_telegram(telegram_runtime, bot).await;
            Ok::<(), anyhow::Error>(())
        })
        .await??;
        return Ok(());
    }

    Err(anyhow!("telegram is disabled in config"))
}
