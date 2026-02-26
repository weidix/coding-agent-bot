# coding-agent-bot

A Telegram-first coding assistant bot with an integrated in-process ACP runtime.

## Overview

The bot supports:

- Running ACP tasks in the background via an integrated runtime
- Telegram interactions with inline buttons
- Streaming-like task output by editing the same Telegram message
- Multi-task concurrency (multiple Codex tasks at once)
- Access control with user/chat whitelist and folder whitelist
- Config-driven startup (no CLI arguments required)

## Requirements

- Rust stable toolchain
- Cargo

## Getting Started

```bash
cp config/bot.toml config/local.toml
# edit config/local.toml with your Telegram token and whitelist
CODING_AGENT_BOT_CONFIG=config/local.toml cargo run
```

## Development

```bash
cargo fmt
cargo check
```

## Configuration

Default config path:

- `config/bot.toml`

Override with:

- `CODING_AGENT_BOT_CONFIG=/path/to/config.toml`

Key sections:

- `[telegram]` bot token and stream edit behavior
- `[acp]` runtime controls (`stream_chunk_delay_ms`, `io_channel_buffer_size`, `max_running_tasks`)
- `[codex]` codex app-server settings (`binary_path`, `startup_timeout_ms`)
- codex app-server is started on demand, with an automatically selected free local port; when connection is lost, the bot restarts the app-server and retries once
- backend and model are selected during interaction (`/new <cwd> backend=codex [model=xxx]`)
- `[whitelist]` user/chat allow-list and allowed folders

## Library Usage

This crate can be imported as a library:

```rust
use coding_agent_bot::{run, AppConfig};
```

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE).
