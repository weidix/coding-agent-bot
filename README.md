# coding-agent-bot

This project now only keeps configuration loading and validation logic.

## Requirements

- Rust stable toolchain
- Cargo

## Usage

By default, configuration is loaded from:

- `config/bot.toml`

Override with:

- `CODING_AGENT_BOT_CONFIG=/path/to/config.toml`

Run a configuration check:

```bash
CODING_AGENT_BOT_CONFIG=config/bot.toml cargo run
```

If parsing and validation pass, the process exits successfully.

## Development

```bash
cargo fmt
cargo check
```

## Library Usage

```rust
use coding_agent_bot::AppConfig;
```

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE).
