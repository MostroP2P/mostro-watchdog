# Dispute Status Alerts

This document describes the enhanced dispute monitoring capabilities implemented in issue #2.

## Overview

mostro-watchdog now monitors **all** dispute status changes, not just new disputes. This provides complete visibility into the dispute lifecycle for Mostro administrators.

## Dispute Status Types

The bot monitors these dispute status changes:

### 🚨 `initiated`
- **Description**: New dispute created, waiting for solver
- **Message**: Shows dispute ID, initiator (buyer/seller), and timestamp
- **Action needed**: Admin should take the dispute

### 🔄 `in-progress`  
- **Description**: Dispute taken by a solver/admin
- **Message**: Confirms dispute is being handled
- **Action needed**: None, informational

### 💰 `seller-refunded`
- **Description**: Dispute resolved by refunding the seller
- **Message**: Shows resolution and closure confirmation
- **Action needed**: None, dispute closed

### ✅ `settled`
- **Description**: Dispute resolved by paying the buyer
- **Message**: Shows payment resolution and closure
- **Action needed**: None, dispute closed

### 🔓 `released`
- **Description**: Dispute resolved when seller releases funds
- **Message**: Shows cooperative resolution
- **Action needed**: None, dispute closed

### 🤝 `cooperatively-canceled`
- **Description**: Both parties agreed to a cooperative cancel while the dispute was open: the seller is refunded and no solver was needed. Emitted by Mostro nodes on mostro-core 0.15.1 or later; older nodes reported this case as `seller-refunded`.
- **Message**: Shows the users' own resolution and the refund to the seller
- **Action needed**: None, dispute closed

### 📡 `other`
- **Description**: Unknown or future status types
- **Message**: Generic status update message
- **Action needed**: May require investigation

## Configuration

### Alert Types (Optional)

You can enable/disable specific alert types in your `config.toml`:

```toml
```toml
[alerts]
initiated = true        # New disputes (recommended: true)
in_progress = true      # Dispute taken (recommended: true)
seller_refunded = true  # Seller refunded (recommended: true) 
settled = true          # Payment to buyer (recommended: true)
released = true         # Released by seller (recommended: true)
cooperatively_canceled = true  # Cancelled by both parties (recommended: true)
other = true           # Unknown statuses (recommended: true)
```

### Backward Compatibility

The `[alerts]` section is **optional**. If not present, all alert types default to enabled, maintaining backward compatibility.

## Alert Format Examples

### New Dispute (initiated)
```text
🚨 NEW DISPUTE

📋 Dispute ID: `abc123def456`
👤 Initiated by: buyer
⏰ Time: 2026-02-20 15:30:00 UTC

⚡ Please take this dispute in Mostrix or your admin client.
```

### Dispute In Progress
```text
🔄 DISPUTE IN PROGRESS

📋 Dispute ID: `abc123def456`
👨‍⚖️ Status: Taken by solver
⏰ Time: 2026-02-20 15:35:00 UTC

ℹ️ Dispute is now being handled.
```

### Dispute Resolved (settled)
```text
✅ DISPUTE RESOLVED

📋 Dispute ID: `abc123def456`
💸 Resolution: Payment to buyer
⏰ Time: 2026-02-20 16:00:00 UTC

✔️ Dispute closed: buyer receives payment.
```

## Serbero alerts

[Serbero](https://github.com/MostroP2P/serbero) is Mostro's dispute assistant.
It takes a dispute as a read-only solver, talks to both parties and, when a
person is needed, hands the dispute off to the human solvers. While Serbero
mediates, Mostro shows the dispute as `in-progress`, so without Serbero alerts
the Telegram group never learns that a dispute was handed off and needs a solver.

With Serbero alerts on, the watchdog:

- shows Serbero's latest step on the dispute's message (an edit, which does not
  notify);
- sends a **new message**, which notifies, when Serbero hands a dispute off or
  cannot start mediating it. It replies to the dispute's message when there is
  one, and stands alone otherwise (for example when the watchdog started after
  the dispute's alert went out);
- sends another **new message** when a solver takes the dispute over from
  Serbero, so the group sees the handoff answered.

| Serbero says | Line on the dispute's message | New message |
|---|---|---|
| `mediating` | 🤖 Serbero: mediating | – |
| `guidance sent: <path>` | 🤖 Serbero: guided the parties to resolve it themselves (payment arrived) | – |
| `handed off: <reason>` | 🙋 Serbero: handed off (conflicting claims) — a solver must take it over | 🙋 SERBERO HANDED OFF A DISPUTE |
| `mediation could not start` | 🙋 Serbero: mediation could not start — a solver must take it over | 🙋 SERBERO COULD NOT START MEDIATION |

The line stays on the message through later status changes. Once a solver
takes the dispute over, "a solver must take it over" becomes "a solver took it
over" (and `mediating` reads `mediated`). Once the dispute is resolved without
a takeover, "a solver must take it over" is dropped, so the final message stays
true.

### Handoff alert

```text
🙋 SERBERO HANDED OFF A DISPUTE

📋 Dispute ID: `abc123def456`
💬 Reason: conflicting claims
⏰ Time: 2026-10-01 15:30:00 UTC

⚡ A solver must take it over in Mostrix (Ctrl+T on Disputes Pending).
```

Reasons are Serbero's handoff reasons in plain words: `conflicting claims`,
`fraud signal`, `human requested`, `round limit`, `unresponsive`, and so on.

### Takeover alert

Sent once per dispute when a solver takes over a dispute Serbero held, whether
after a handoff or while Serbero was still mediating:

```text
👨‍⚖️ SOLVER TOOK OVER FROM SERBERO

📋 Dispute ID: `abc123def456`
⏰ Time: 2026-10-01 15:42:00 UTC

ℹ️ A solver is now handling the dispute.
```

Mostro's dispute events do not name the solver, so the watchdog spots a
takeover as a second, later `in-progress` event for a dispute Serbero reported
on (Serbero, a read-only solver, cannot take a dispute twice). It is not
announced when:

- it happens while the watchdog is stopped: the watchdog only sees dispute
  events published while it runs, like every other dispute alert;
- it is seen before any update from Serbero about that dispute.

The takeover also updates Serbero's line on the dispute's message, even with
the `in_progress` alert turned off.

### Setup

1. Generate a Nostr key for the watchdog, used for nothing else. Any Nostr key
   tool works, or `openssl rand -hex 32`.
2. Put the key in an environment variable (nsec or hex) and add a `[serbero]`
   section to `config.toml`:

   ```bash
   export WATCHDOG_NOSTR_PRIVATE_KEY=...   # never in config.toml
   ```

   ```toml
   [serbero]
   # private_key_env = "WATCHDOG_NOSTR_PRIVATE_KEY"   # the default
   # pubkey = "npub1..."   # Serbero's key; read from the Mostro node when omitted
   ```

3. Start the watchdog. It logs its own public key:

   ```text
   🤖 Serbero alerts enabled. Watchdog Nostr pubkey: npub1... (hex: 3bf0...). Add the hex key to Serbero's [[observers]].
   ```

4. Add that hex key to Serbero's config as an **observer** and restart Serbero:

   ```toml
   [[observers]]
   pubkey = "<watchdog hex pubkey>"
   ```

The watchdog fails to start when `[serbero]` is present but the environment
variable is missing or does not hold a valid key. Error messages name the
variable, never its value.

When `pubkey` is omitted, the watchdog reads Serbero's key from the `serbero`
tag of the Mostro node's info event (kind 38385) and checks it again every 10
minutes (or every `nip65_refresh_interval`, if shorter). While no key is known
it retries sooner, from 30 seconds on. If the node announces no Serbero, the
watchdog logs a warning and keeps running without Serbero alerts. A key that
worked is dropped only after two info events in a row name no Serbero, since
one may be a stale copy on a slow relay.

**Relays.** The watchdog hears Serbero only on its own relays: the bootstrap
`[nostr] relays`, then the Mostro node's NIP-65 relays once it switches to
them. Serbero must publish to at least one of these relays, or its messages
never reach the watchdog (nothing warns about it).

To turn either kind of alert off:

```toml
[alerts]
serbero_handoff = false    # no new message on handoffs or takeovers
serbero_progress = false   # no Serbero line on dispute messages
```

### Privacy

Serbero sends observers only the first line of each update, for example
`Dispute <id> · handed off: conflicting_claims`, never what the parties wrote.
If the watchdog's key is registered as a solver by mistake, Serbero sends it
full briefs and transcripts: the watchdog still reads only the first line,
drops the rest without storing, logging or forwarding it, and logs a warning
asking for the key to be moved to `[[observers]]`.

### Delivery

- Serbero's messages are Mostro protocol v2 `send-dm` messages (NIP-44,
  kind 14) signed by Serbero; anything not signed by the trusted Serbero key is
  ignored.
- Each update is relayed at most once per dispute, across relay redeliveries
  and restarts (recorded in `disputes.db`).
- On every start, every 10 minutes (or every `nip65_refresh_interval`, if
  shorter) and after each switch of relays, the watchdog fetches the last 24
  hours of Serbero's messages, so updates sent while it was down are relayed.
- A handoff alert that Telegram rejected is retried by an early fetch, after
  30 seconds and then after doubling delays while failures last.
- A handoff for a dispute the watchdog already saw resolved sends no alert.
- Updates move a dispute only forward (mediating, then guidance, then
  handoff): Serbero dates a retried message when it sends it, so a late
  `mediating` never replaces a handoff.

## Benefits

1. **Complete visibility**: Track disputes from creation to resolution
2. **Reduced response time**: Immediate notifications for all status changes
3. **Better coordination**: Team knows when disputes are being handled
4. **Audit trail**: Full history of dispute progression
5. **Customizable**: Enable only the alerts you need

## Migration from v0.1.x

Existing configurations continue to work unchanged. The new status monitoring is automatic, and you can optionally add the `[alerts]` section for fine-grained control.

## Technical Details

- Monitors Nostr events (kind 38386) for all status values
- Only the live subscription (`since` the launch, or the relay swap) posts
  new messages. Statuses fetched by the solver catch-up, which has no lower
  time bound, only edit a dispute's existing message when they are newer than
  the recorded status, and never fall back to a new message
- Parses `s` tag for status, `d` tag for dispute ID, `initiator` tag for who created dispute
- Uses different emoji and messaging for each status type
- Maintains backward compatibility with existing configurations