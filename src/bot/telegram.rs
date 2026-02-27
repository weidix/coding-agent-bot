use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use teloxide::prelude::{Request, Requester};
use teloxide::{Bot, types::Me};

use crate::{AppConfig, TelegramConfig};

/// Telegram runtime options derived from configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramRuntimeConfig {
    pub stream_edit_interval: Duration,
    pub message_max_chars: usize,
    pub allowed_users: HashSet<String>,
}

/// Telegram bootstrap object.
///
/// This owns the low-level Telegram API client and validated runtime settings.
#[derive(Clone)]
pub struct TelegramRuntime {
    bot: Bot,
    config: TelegramRuntimeConfig,
}

impl TelegramRuntime {
    pub fn from_app_config(config: &AppConfig) -> Result<Option<Self>> {
        Self::from_telegram_config(&config.telegram)
    }

    pub fn from_telegram_config(config: &TelegramConfig) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }

        let token = config.bot_token.trim();
        if token.is_empty() {
            return Err(anyhow!(
                "telegram.bot_token must be set when telegram.enabled is true"
            ));
        }

        if config.message_max_chars == 0 {
            return Err(anyhow!(
                "telegram.message_max_chars must be greater than 0, got {}",
                config.message_max_chars
            ));
        }

        let bot = Bot::new(token.to_owned());
        let allowed_users = config
            .allowed_users
            .iter()
            .filter_map(|entry| normalize_username(entry))
            .collect();
        let runtime_config = TelegramRuntimeConfig {
            stream_edit_interval: Duration::from_millis(config.stream_edit_interval_ms),
            message_max_chars: config.message_max_chars,
            allowed_users,
        };

        Ok(Some(Self {
            bot,
            config: runtime_config,
        }))
    }

    #[must_use]
    pub fn bot(&self) -> Bot {
        self.bot.clone()
    }

    #[must_use]
    pub fn config(&self) -> &TelegramRuntimeConfig {
        &self.config
    }

    /// Checks Telegram reachability and token validity via `getMe`.
    pub async fn verify_connection(&self) -> Result<Me> {
        self.bot
            .get_me()
            .send()
            .await
            .context("failed to call Telegram getMe")
    }

    /// Placeholder runtime entrypoint.
    ///
    /// Actual update handling and ACP bot behavior will be implemented later.
    pub async fn run(self) -> Result<()> {
        Ok(())
    }

    #[must_use]
    pub fn is_user_allowed(&self, username: Option<&str>) -> bool {
        if self.config.allowed_users.is_empty() {
            return true;
        }

        username
            .and_then(normalize_username)
            .is_some_and(|name| self.config.allowed_users.contains(&name))
    }
}

fn normalize_username(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed.trim_start_matches('@').trim();
    if normalized.is_empty() {
        return None;
    }

    Some(normalized.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> TelegramConfig {
        TelegramConfig {
            enabled: true,
            bot_token: "test-token".to_string(),
            stream_edit_interval_ms: 1_200,
            message_max_chars: 2048,
            allowed_users: vec![],
        }
    }

    #[test]
    fn disabled_telegram_returns_none_runtime() {
        let mut config = sample_config();
        config.enabled = false;

        let runtime = TelegramRuntime::from_telegram_config(&config).expect("config should parse");
        assert!(runtime.is_none());
    }

    #[test]
    fn enabled_telegram_requires_non_empty_bot_token() {
        let mut config = sample_config();
        config.bot_token = "   ".to_string();

        let result = TelegramRuntime::from_telegram_config(&config);
        assert!(result.is_err());
        let err = result.err().expect("token must fail");
        assert!(
            err.to_string()
                .contains("telegram.bot_token must be set when telegram.enabled is true")
        );
    }

    #[test]
    fn enabled_telegram_requires_positive_message_max_chars() {
        let mut config = sample_config();
        config.message_max_chars = 0;

        let result = TelegramRuntime::from_telegram_config(&config);
        assert!(result.is_err());
        let err = result.err().expect("message_max_chars must fail");
        assert!(
            err.to_string()
                .contains("telegram.message_max_chars must be greater than 0")
        );
    }

    #[test]
    fn runtime_normalizes_allowed_users_from_config() {
        let mut config = sample_config();
        config.allowed_users = vec![
            "@Alice".to_string(),
            " bob ".to_string(),
            "   ".to_string(),
            "@".to_string(),
            "@alice".to_string(),
        ];

        let runtime = TelegramRuntime::from_telegram_config(&config)
            .expect("config should parse")
            .expect("runtime must be enabled");
        assert_eq!(
            runtime.config.stream_edit_interval,
            Duration::from_millis(1_200)
        );
        assert_eq!(runtime.config.message_max_chars, 2048);
        assert_eq!(runtime.config.allowed_users.len(), 2);
        assert!(runtime.config.allowed_users.contains("alice"));
        assert!(runtime.config.allowed_users.contains("bob"));
    }

    #[test]
    fn allow_all_when_allowed_users_not_configured() {
        let config = sample_config();
        let runtime = TelegramRuntime::from_telegram_config(&config)
            .expect("config should parse")
            .expect("runtime must be enabled");

        assert!(runtime.is_user_allowed(None));
        assert!(runtime.is_user_allowed(Some("@anyone")));
    }

    #[test]
    fn allow_only_configured_users_when_allowlist_exists() {
        let mut config = sample_config();
        config.allowed_users = vec!["@alice".to_string(), "bob".to_string()];
        let runtime = TelegramRuntime::from_telegram_config(&config)
            .expect("config should parse")
            .expect("runtime must be enabled");

        assert!(runtime.is_user_allowed(Some("@ALICE")));
        assert!(runtime.is_user_allowed(Some(" Bob ")));
        assert!(!runtime.is_user_allowed(Some("@charlie")));
        assert!(!runtime.is_user_allowed(None));
    }
}
