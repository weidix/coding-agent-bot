#[tokio::main]
async fn main() -> anyhow::Result<()> {
    coding_agent_bot::run_from_default_config().await
}
