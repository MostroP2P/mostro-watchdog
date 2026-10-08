# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Serbero alerts: with a `[serbero]` section, the watchdog reads the mediation
  updates Serbero, Mostro's dispute assistant, sends its observers. It shows
  Serbero's progress on each dispute's message and sends a new message when
  Serbero hands a dispute off or cannot start mediating it, so a human solver
  takes it over. Only the first line of each update is read. See
  [DISPUTE_STATUS_ALERTS.md](DISPUTE_STATUS_ALERTS.md#serbero-alerts).
- `[alerts]` options `serbero_handoff` and `serbero_progress`.
- Serbero takeover alert: a new message when a solver takes over a dispute
  Serbero held, and Serbero's line on the dispute's message reads "a solver
  took it over". Sent with `serbero_handoff`.

### Changed
- `disputes.db` gains a `message_text` column and four tables
  (`serbero_states`, `serbero_headers`, `dispute_statuses`,
  `serbero_takeovers`); existing databases are migrated on start.

### Fixed
- A config file with a TOML error no longer prints the whole file, Telegram bot
  token included, at startup: the error shows its message and line only.

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

[Unreleased]: https://github.com/MostroP2P/mostro-watchdog/compare/v0.3.0...HEAD
[v0.1.0]: https://github.com/MostroP2P/mostro-watchdog/releases/tag/v0.1.0