# 🐕 mostro-watchdog

[![CI](https://github.com/MostroP2P/mostro-watchdog/workflows/CI/badge.svg)](https://github.com/MostroP2P/mostro-watchdog/actions/workflows/ci.yml)
[![Release](https://github.com/MostroP2P/mostro-watchdog/workflows/Release/badge.svg)](https://github.com/MostroP2P/mostro-watchdog/actions/workflows/release.yml)
[![Latest Release](https://img.shields.io/github/v/release/MostroP2P/mostro-watchdog)](https://github.com/MostroP2P/mostro-watchdog/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

<p align="center">
  <img src="mascot.png" alt="mostro-watchdog mascot" width="300" />
</p>

Real-time Telegram notification bot for [Mostro](https://mostro.network) administrators. Monitors Nostr dispute events (kind 38386) and sends instant alerts to a Telegram group or channel.

## Why?

When a user opens a dispute on Mostro, administrators need to respond quickly. Users in disputes are worried — fast response times build trust and improve the experience.

**mostro-watchdog** bridges Nostr and Telegram so admins get notified the instant a dispute is created, without needing to monitor Mostrix or Nostr clients constantly.

## How it works

```text
Mostro daemon → Nostr (kind 38386) → mostro-watchdog → Telegram alert
```

1. Mostro daemon publishes a dispute event (kind 38386) to Nostr relays
2. mostro-watchdog subscribes to these events filtered by your Mostro's pubkey
3. When a new dispute is detected (status: `initiated`), it sends a formatted alert to your Telegram group/channel
4. Admins see the alert and can take the dispute via Mostrix or their preferred admin client

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (1.75+)
- A Telegram bot token (from [@BotFather](https://t.me/BotFather))
- Your Mostro daemon's Nostr public key

#### Native Build Dependencies

Some Rust crates require native libraries and build tools. Install them before building:

**Ubuntu / Debian:**

```bash
sudo apt update
sudo apt install -y cmake pkg-config libssl-dev
```

**macOS (Homebrew):**

```bash
brew install cmake openssl pkg-config
export OPENSSL_DIR=$(brew --prefix openssl)
```

> **Tip:** Add the `export OPENSSL_DIR=...` line to your `~/.zshrc` (or `~/.bashrc`) so you don't have to set it every time.

### Install

#### Option 1: Download Pre-compiled Binary (Recommended)

Download the latest binary for your platform from the [releases page](https://github.com/MostroP2P/mostro-watchdog/releases/latest):

```bash
# Linux x86_64
curl -LO https://github.com/MostroP2P/mostro-watchdog/releases/latest/download/mostro-watchdog-linux-x86_64
chmod +x mostro-watchdog-linux-x86_64
sudo mv mostro-watchdog-linux-x86_64 /usr/local/bin/mostro-watchdog

# Linux ARM64 (Raspberry Pi, etc.)
curl -LO https://github.com/MostroP2P/mostro-watchdog/releases/latest/download/mostro-watchdog-linux-aarch64
chmod +x mostro-watchdog-linux-aarch64
sudo mv mostro-watchdog-linux-aarch64 /usr/local/bin/mostro-watchdog

# macOS x86_64
curl -LO https://github.com/MostroP2P/mostro-watchdog/releases/latest/download/mostro-watchdog-macos-x86_64
chmod +x mostro-watchdog-macos-x86_64
sudo mv mostro-watchdog-macos-x86_64 /usr/local/bin/mostro-watchdog

# macOS ARM64 (Apple Silicon)
curl -LO https://github.com/MostroP2P/mostro-watchdog/releases/latest/download/mostro-watchdog-macos-aarch64
chmod +x mostro-watchdog-macos-aarch64
sudo mv mostro-watchdog-macos-aarch64 /usr/local/bin/mostro-watchdog
```

**Verify the download** (recommended):
```bash
# Download checksums
curl -LO https://github.com/MostroP2P/mostro-watchdog/releases/latest/download/manifest.txt

# Verify your binary
# Linux/WSL
sha256sum -c manifest.txt --ignore-missing

# macOS
shasum -a 256 -c manifest.txt
```

#### Option 2: Build from Source

```bash
git clone https://github.com/MostroP2P/mostro-watchdog.git
cd mostro-watchdog
cargo build --release

# Binary will be at ./target/release/mostro-watchdog
```

### Configure

```bash
# Copy the example config
cp config.example.toml config.toml

# Edit with your values
nano config.toml
```

You'll need to set:
- `mostro.pubkey` — Your Mostro daemon's Nostr public key
- `nostr.relays` — The relays your Mostro daemon uses
- `telegram.bot_token` — Token from @BotFather
- `telegram.chat_id` — The Telegram group/channel ID for alerts

### Run

```bash
# Default (looks for ./config.toml, then ~/.config/mostro-watchdog/config.toml)
./target/release/mostro-watchdog

# Custom config path
./target/release/mostro-watchdog --config /path/to/config.toml

# Positional argument also works
./target/release/mostro-watchdog /path/to/config.toml

# With debug logging
RUST_LOG=mostro_watchdog=debug ./target/release/mostro-watchdog

# Help & version
./target/release/mostro-watchdog --help
./target/release/mostro-watchdog --version
```

The config file is searched in this order:
1. `./config.toml` (current directory)
2. `~/.config/mostro-watchdog/config.toml`

Or specify it explicitly with `--config` / `-c`.

### Setting up the Telegram bot

1. Open Telegram and message [@BotFather](https://t.me/BotFather)
2. Send `/newbot` and follow the instructions to create a new bot
3. Copy the bot token to your `config.toml`
4. Create a private group/channel for your admin team
5. Add the bot to the group/channel
6. Get the chat ID (see config.example.toml for instructions)

## Alert Format

When a dispute is detected, you'll receive a message like:

```text
🚨 NEW DISPUTE

📋 Dispute ID: abc123def456
👤 Initiated by: buyer
⏰ Time: 2026-02-11 18:30:00 UTC

⚡ Please take this dispute in Mostrix or your admin client.
```

## Configuration Reference

| Field | Description |
|-------|-------------|
| `mostro.pubkey` | Mostro daemon's Nostr public key (hex or npub) |
| `nostr.relays` | Array of Nostr relay WebSocket URLs |
| `telegram.bot_token` | Telegram bot API token |
| `telegram.chat_id` | Telegram chat/group/channel ID for alerts |

## Roadmap

- [x] Alert on dispute status changes (in-progress, resolved, etc.)
- [ ] Health check / heartbeat notifications
- [ ] Docker image
- [ ] Pre-built binaries for Linux, macOS, Windows

## Contributing

Contributions are welcome! Please open an issue first to discuss what you'd like to change.

## License

[MIT](LICENSE)
