fn main() -> anyhow::Result<()> {
    coding_agent_bot::AppConfig::load_default()?;
    Ok(())
}
