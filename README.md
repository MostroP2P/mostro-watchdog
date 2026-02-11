# 🐕 mostro-watchdog

<p align="center">
  <img src="mascot.png" alt="mostro-watchdog mascot" width="300" />
</p>

Real-time Telegram notification bot for [Mostro](https://mostro.network) administrators. Monitors Nostr dispute events (kind 38386) and sends instant alerts to a Telegram group or channel.

## Why?

When a user opens a dispute on Mostro, administrators need to respond quickly. Users in disputes are worried — fast response times build trust and improve the experience.

**mostro-watchdog** bridges Nostr and Telegram so admins get notified the instant a dispute is created, without needing to monitor Mostrix or Nostr clients constantly.

## How it works

```
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

### Install

```bash
# From source
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
# Default (looks for config.toml in current directory)
./target/release/mostro-watchdog

# Custom config path
./target/release/mostro-watchdog /path/to/config.toml

# With debug logging
RUST_LOG=mostro_watchdog=debug ./target/release/mostro-watchdog
```

### Setting up the Telegram bot

1. Open Telegram and message [@BotFather](https://t.me/BotFather)
2. Send `/newbot` and follow the instructions to create a new bot
3. Copy the bot token to your `config.toml`
4. Create a private group/channel for your admin team
5. Add the bot to the group/channel
6. Get the chat ID (see config.example.toml for instructions)

## Alert Format

When a dispute is detected, you'll receive a message like:

```
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

- [ ] Alert on dispute status changes (in-progress, resolved, etc.)
- [ ] Alert on other important Mostro events (order cancellations, expired invoices)
- [ ] Configurable alert templates
- [ ] Multiple Telegram channels for different event types
- [ ] Health check / heartbeat notifications
- [ ] Docker image
- [ ] Pre-built binaries for Linux, macOS, Windows

## Contributing

Contributions are welcome! Please open an issue first to discuss what you'd like to change.

## License

[MIT](LICENSE)
