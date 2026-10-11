# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Serbero alerts: with a `[serbero]` section, the watchdog reads the mediation
  updates Serbero, Mostro's dispute assistant, sends its observers. It shows
  Serbero's progress on each dispute's message. Only the first line of each
  update is read. See
  [DISPUTE_STATUS_ALERTS.md](DISPUTE_STATUS_ALERTS.md#serbero-alerts).
- `[alerts]` option `serbero_progress`.
- One message per dispute with a timeline: the dispute's message is edited in
  place and lists everything that happened to it in the order it happened
  (opened, taken, Serbero's steps, takeover, resolution), under a header
  saying where it stands. Steps that arrive late or out of order take their
  place by event time. See
  [DISPUTE_STATUS_ALERTS.md](DISPUTE_STATUS_ALERTS.md#alert-format) (#41).
- `[alerts]` option `solver_names`.
- Edit notifications: Telegram does not notify of an edited message, so each
  live edit of a dispute's message is followed by a short reply naming the
  step, deleted a minute later. `[alerts]` options `edit_notifications`
  and `edit_notification_lifetime`. See
  [DISPUTE_STATUS_ALERTS.md](DISPUTE_STATUS_ALERTS.md#edit-notifications).
- Solver notifications: with a `[solver_notifications]` section, a solver who
  links their key with `/link` and Mostrix gets a private Telegram message
  when a party writes to them in a dispute chat. No private key leaves
  Mostrix and the watchdog never reads the chat. New commands `/link`,
  `/unlink` and `/status`. See [SOLVER_NOTIFICATIONS.md](SOLVER_NOTIFICATIONS.md).

### Removed
- The separate Serbero messages (`🙋 SERBERO HANDED OFF A DISPUTE`,
  `🙋 SERBERO COULD NOT START MEDIATION`, `👨‍⚖️ SOLVER TOOK OVER FROM SERBERO`)
  and the `serbero_handoff` and `takeover_message` options. The dispute's one
  message already shows every step; a config that still sets the options
  loads unchanged.

### Changed
- `disputes.db` gains a `message_text` column and two tables
  (`serbero_states`, `serbero_headers`); existing databases are migrated on
  start.
- `disputes.db` gains the `solver_*` tables for solver notifications.
- `disputes.db` gains the `dispute_timeline` table; existing databases are
  migrated on start, and a message sent before the timeline gets its stored
  status as first step.
- A status turned off in `[alerts]` still goes on the dispute's timeline and
  edits the message; it only never sends a new one.
- A cooperative cancel reported as `canceled` by older nodes closes the
  dispute's timeline instead of deleting its message.

### Fixed
- The disputes channel only shows what the live dispute subscription
  delivers. The solver catch-up, which reads the latest status of every
  watched dispute however old, could post months-old statuses as new alerts
  when a solver started watching a dispute the channel had no message for.
  A caught-up status now only adds to the dispute's timeline and edits its
  existing message (#40).
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