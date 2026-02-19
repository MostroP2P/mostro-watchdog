# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.1.0] - 2026-02-19

### Added
- Initial release of mostro-watchdog
- Nostr-based monitoring of Mostro dispute events (kind 38386)
- Real-time Telegram notifications for administrators
- Configurable notification settings
- Support for multiple relay connections
- Structured logging with tracing

### Features
- **Dispute Monitoring**: Automatically monitors Nostr relays for dispute events
- **Telegram Integration**: Sends formatted notifications to specified Telegram chats
- **Configuration**: TOML-based configuration with example file
- **Reliability**: Robust error handling and reconnection logic
- **Logging**: Comprehensive logging for debugging and monitoring

[v0.1.0]: https://github.com/MostroP2P/mostro-watchdog/releases/tag/v0.1.0