#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = coding_agent_bot::AppConfig::load_default()?;

    if let Some(runtime) =
        coding_agent_bot::bot::telegram::TelegramRuntime::from_app_config(&config)?
    {
        let _ = runtime.verify_connection().await?;
    }

    Ok(())
}
