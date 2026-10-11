# Dispute Status Alerts

This document describes the enhanced dispute monitoring capabilities implemented in issue #2.

## Overview

mostro-watchdog monitors **all** dispute status changes, not just new disputes,
and keeps **one message per dispute**: sent when the dispute opens and edited
in place with a timeline of everything that happened to it. See
[Alert Format](#alert-format).

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

A status turned off still goes on the dispute's timeline and still edits the
dispute's message, without the edit notification; it only never **sends** a
new message. So with `initiated = false`, a dispute's message first appears
at its next enabled status, with the opening already on its timeline.

### Edit notifications

Telegram does not notify of an edited message: no sound, no banner, no unread
badge. With one message per dispute, only a dispute's first status would ever
ring a phone. So after each live edit of a dispute's message the watchdog
sends a short reply to it naming the step, for example:

```text
🔔 Dispute 96629381-bcb8-4d4f-8c66-e8f86f3e86ea
🙋 Serbero handed off · conflicting claims
```

and deletes that reply `edit_notification_lifetime` seconds later (default:
60, at most 300). The phones ring, the preview says what happened, and the
channel keeps one message per dispute. Telegram clears the notification from
the phone when the message is deleted, so the reply stays long enough to be
seen before it goes; `0` deletes it at once.

```toml
[alerts]
edit_notifications = true        # default
edit_notification_lifetime = 60  # seconds
```

A caught-up step, a status turned off and a step that arrives before the
dispute has a message never notify. In a group, the bot deletes its own
messages without any extra permission; in a channel it needs the admin right
to delete messages. A reply the bot could not delete stays in the channel and
is logged as a warning.

### Solver names

Mostro names the solver in some dispute events. The timeline shows a solver by
the name configured for their pubkey (hex), or by a shortened pubkey:

```toml
[alerts.solver_names]
"000000e2fdb5000000000000000000000000000000000000000000000000a7f1" = "grunch"
```

### Backward Compatibility

The `[alerts]` section is **optional**. If not present, all alert types default to enabled, maintaining backward compatibility.

## Alert Format

Each dispute has one message, edited in place. A header line shows where the
dispute stands; the timeline below it lists every step the watchdog knows
about, in the order they happened (by event time, whatever order they reached
the watchdog). Times are UTC; the date shows once at the bottom, and again on
a step that happened on another day.

```text
⚖️ DISPUTE 96629381-bcb8-4d4f-8c66-e8f86f3e86ea
Status: ✅ RESOLVED · released by seller

🚨 09:41:02 Opened by buyer
🤖 09:41:03 Taken by Serbero
🤖 09:41:05 Serbero mediating
🙋 09:48:53 Serbero handed off · facts gathered
🔓 10:07:21 Released by seller · resolved by the parties

All times UTC · 2026-10-10
```

A dispute a human took over from Serbero, resolved by that solver:

```text
⚖️ DISPUTE f8af1141-b3a2-45e7-8391-017debdf3ef5
Status: ✅ RESOLVED · seller refunded by grunch

🚨 18:10:39 Opened by seller
🤖 18:10:40 Taken by Serbero
🤖 18:10:42 Serbero mediating
🙋 18:16:34 Serbero handed off · facts gathered
👨‍⚖️ 18:48:47 Taken over by grunch
💰 19:10:42 Seller refunded · resolved by grunch

All times UTC · 2026-10-09
```

Header lines:

| Where the dispute stands | Header |
|---|---|
| Opened, nobody took it | 🚨 OPEN · needs a solver |
| Serbero took it | 🤖 WITH SERBERO · mediating (or · guided the parties) |
| Serbero handed it off or could not start | 🙋 NEEDS A SOLVER · handed off · ‹reason› |
| A solver took it (over) | 👨‍⚖️ WITH A SOLVER · ‹name› |
| Resolved | ✅ RESOLVED · released by seller / canceled cooperatively / settled, buyer paid, by ‹name› / seller refunded by ‹name› |
| Cooperative cancel on an older node (`canceled`) | 🗑 CANCELED · cooperatively |
| A status this version does not know | 📡 ‹status› |

Mostro's `in-progress` event does not say who took the dispute: the first take
of a dispute Serbero reports on is shown as Serbero's, and a later one as a
solver taking it over.

A timeline longer than 12 steps keeps its first step and its last eleven, with
a line saying how many were left out. A message deleted in Telegram is sent
again, with the whole timeline, at the dispute's next live status. A message
sent by a version before the timeline gets its stored status as first step.

## Serbero alerts

[Serbero](https://github.com/MostroP2P/serbero) is Mostro's dispute assistant.
It takes a dispute as a read-only solver, talks to both parties and, when a
person is needed, hands the dispute off to the human solvers. While Serbero
mediates, Mostro shows the dispute as `in-progress`, so without Serbero alerts
the Telegram group never learns that a dispute was handed off and needs a solver.

With Serbero alerts on, the watchdog adds each of Serbero's steps to the
dispute's timeline and moves its header, an edit of the dispute's one message
followed by the [edit notification](#edit-notifications). It never sends a
lasting message for them: the channel holds one message per dispute.

| Serbero says | Step on the timeline | Header while it holds |
|---|---|---|
| `mediating` | 🤖 Serbero mediating | 🤖 WITH SERBERO · mediating |
| `guidance sent: <path>` | 🤖 Serbero guided the parties · payment arrived | 🤖 WITH SERBERO · guided the parties |
| `handed off: <reason>` | 🙋 Serbero handed off · conflicting claims | 🙋 NEEDS A SOLVER · handed off · conflicting claims |
| `mediation could not start` | 🙋 Serbero could not start mediation | 🙋 NEEDS A SOLVER · mediation could not start |

The steps stay on the timeline through later status changes. The header stops
asking for a solver once one takes the dispute over or the dispute is
resolved, so the final message stays true. A step that reaches the watchdog
late (a catch-up, a relay delay) takes its place on the timeline by its own
time, and a late notice of an earlier mediation stage never moves the header
back.

Reasons are Serbero's handoff reasons in plain words: `conflicting claims`,
`fraud signal`, `human requested`, `round limit`, `unresponsive`, and so on.

### Takeover

Mostro's dispute events do not name the solver, so a solver taking a dispute
over from Serbero shows as a later `in-progress` event for the dispute. It
joins the timeline as `👨‍⚖️ Taken over by ‹name›` and the header stops asking
for a solver (`👨‍⚖️ WITH A SOLVER · ‹name›`), even with the `in_progress`
alert turned off.

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

To keep Serbero's steps off the dispute's timeline:

```toml
[alerts]
serbero_progress = false   # no Serbero steps on the dispute's timeline
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
- An update the watchdog could not show (Telegram rejected the edit of the
  dispute's message) or could not record (a database error) is retried by an
  early fetch, after 30 seconds and then after doubling delays while failures
  last. The step joins the timeline once however many times it is retried.
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
  time bound, only add to the timeline and edit a dispute's existing message,
  and never fall back to a new message
- Every step is stored once in `disputes.db` (`dispute_timeline`, keyed by
  dispute, step, detail and event time), so redeliveries, re-fetches and
  restarts never duplicate one; the message is rendered from the whole
  timeline on every change, and steps in the same second keep the lifecycle
  order (opened, taken, Serbero, resolved)
- With `serbero_progress = false`, Serbero's steps are not stored, so they
  never show on the timeline, not even on a later redraw
- Parses `s` tag for status, `d` tag for dispute ID, `initiator` tag for who created dispute, `solver` tag for who resolved it
- Maintains backward compatibility with existing configurations