use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

pub const CONFIG_PATH_ENV: &str = "CODING_AGENT_BOT_CONFIG";
pub const DEFAULT_CONFIG_PATH: &str = "config/bot.toml";

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub acp: AcpConfig,
    #[serde(default)]
    pub codex: CodexConfig,
    #[serde(default)]
    pub whitelist: WhitelistConfig,
}

impl AppConfig {
    pub fn load_default() -> Result<Self> {
        let path = env::var(CONFIG_PATH_ENV).unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
        Self::load_from_path(path)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        let raw = fs::read_to_string(path_ref)
            .with_context(|| format!("failed to read config file {}", path_ref.display()))?;
        let mut cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config file {}", path_ref.display()))?;

        if cfg.telegram.enabled && cfg.telegram.bot_token.trim().is_empty() {
            return Err(anyhow!(
                "telegram.bot_token must be set when telegram.enabled is true"
            ));
        }

        if cfg.acp.max_running_tasks == 0 {
            return Err(anyhow!("acp.max_running_tasks must be greater than 0"));
        }

        if cfg.acp.io_channel_buffer_size == 0 {
            return Err(anyhow!("acp.io_channel_buffer_size must be greater than 0"));
        }

        if cfg.codex.startup_timeout_ms == 0 {
            return Err(anyhow!(
                "codex.startup_timeout_ms must be greater than 0, got {}",
                cfg.codex.startup_timeout_ms
            ));
        }

        cfg.whitelist.allowed_folders = cfg
            .whitelist
            .allowed_folders
            .iter()
            .map(PathBuf::from)
            .collect();

        Ok(cfg)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default = "default_stream_edit_interval_ms")]
    pub stream_edit_interval_ms: u64,
    #[serde(default = "default_message_max_chars")]
    pub message_max_chars: usize,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            bot_token: String::new(),
            stream_edit_interval_ms: default_stream_edit_interval_ms(),
            message_max_chars: default_message_max_chars(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AcpConfig {
    #[serde(default = "default_stream_chunk_delay_ms")]
    pub stream_chunk_delay_ms: u64,
    #[serde(default = "default_io_channel_buffer_size")]
    pub io_channel_buffer_size: usize,
    #[serde(default = "default_max_running_tasks")]
    pub max_running_tasks: usize,
}

impl Default for AcpConfig {
    fn default() -> Self {
        Self {
            stream_chunk_delay_ms: default_stream_chunk_delay_ms(),
            io_channel_buffer_size: default_io_channel_buffer_size(),
            max_running_tasks: default_max_running_tasks(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodexConfig {
    #[serde(default)]
    pub binary_path: Option<PathBuf>,
    #[serde(default = "default_codex_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            startup_timeout_ms: default_codex_startup_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WhitelistConfig {
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,
    #[serde(default)]
    pub allowed_folders: Vec<PathBuf>,
    #[serde(default)]
    pub allowed_folders_file: Option<PathBuf>,
}

const fn default_true() -> bool {
    true
}

const fn default_codex_startup_timeout_ms() -> u64 {
    5_000
}

const fn default_stream_chunk_delay_ms() -> u64 {
    25
}

const fn default_io_channel_buffer_size() -> usize {
    64 * 1024
}

const fn default_max_running_tasks() -> usize {
    8
}

const fn default_stream_edit_interval_ms() -> u64 {
    800
}

const fn default_message_max_chars() -> usize {
    3500
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let raw = r#"
[telegram]
enabled = true
bot_token = "test-token"
"#;

        let cfg: AppConfig = toml::from_str(raw).expect("config should parse");
        assert_eq!(cfg.telegram.bot_token, "test-token");
        assert!(cfg.codex.binary_path.is_none());
        assert_eq!(cfg.codex.startup_timeout_ms, 5_000);
        assert_eq!(cfg.acp.stream_chunk_delay_ms, 25);
        assert_eq!(cfg.acp.io_channel_buffer_size, 64 * 1024);
        assert_eq!(cfg.acp.max_running_tasks, 8);
    }

    #[test]
    fn parse_whitelist_entries() {
        let raw = r#"
[telegram]
enabled = true
bot_token = "abc"

[whitelist]
allowed_user_ids = [1,2]
allowed_chat_ids = [100]
allowed_folders = ["./a", "./b"]
allowed_folders_file = "./allow.txt"
"#;

        let cfg: AppConfig = toml::from_str(raw).expect("config should parse");
        assert_eq!(cfg.whitelist.allowed_user_ids, vec![1, 2]);
        assert_eq!(cfg.whitelist.allowed_chat_ids, vec![100]);
        assert_eq!(cfg.whitelist.allowed_folders.len(), 2);
        assert_eq!(
            cfg.whitelist
                .allowed_folders_file
                .as_ref()
                .expect("file path")
                .to_string_lossy(),
            "./allow.txt"
        );
    }

    #[test]
    fn parse_codex_binary_path() {
        let raw = r#"
[telegram]
enabled = true
bot_token = "test-token"

[codex]
binary_path = "./tools/codex"
"#;

        let cfg: AppConfig = toml::from_str(raw).expect("config should parse");
        assert!(
            cfg.codex
                .binary_path
                .as_ref()
                .expect("codex binary path")
                .ends_with("tools/codex")
        );
    }
}
