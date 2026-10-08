# Solver Notifications

A solver can link their Mostro solver key to the watchdog and get a private
Telegram message when a party writes to them in the chat of a dispute they
took. The watchdog stays a notification bot: it never holds a private key of
the solver or of a party, never reads a chat message, and never sends one.

```text
party writes in the dispute chat ─▶ Nostr (kind 14) ─▶ mostro-watchdog ─▶ private Telegram message to the solver
```

## How it works

Mostro's dispute chat is not addressed to the solver's pubkey. Each
conversation (solver ↔ buyer, solver ↔ seller) has its own keys derived from
an ECDH secret (`mostro_core::chat`): every message is a kind 14 event signed
by the conversation's `K_sign` and tagged to its `K_conv`, written by either
side. Without a private key nobody can tell that a message belongs to a
solver, so the watchdog needs the solver's client, Mostrix, to tell it which
conversations to watch.

1. **Link.** The solver sends `/link` to the bot in a private chat and gets a
   one-time code and the watchdog's key. They enter both in Mostrix, which
   sends the watchdog a `link` message signed with the solver's key. This
   proves the key is theirs and ties it to their Telegram chat.
2. **Watch.** When the solver takes a dispute (and, right after linking, for
   every dispute they already hold), Mostrix sends `watch` with the public
   `K_sign` of each conversation of that dispute.
3. **Notify.** The watchdog follows kind 14 events signed by those keys. When
   one arrives it waits `grace_period` seconds, then tells the solver which
   party wrote, with the dispute id, and asks them to open Mostrix (see the
   example in the [README](README.md#solver-notifications)). Messages from
   the same party within the wait are grouped into one notification, whose
   header reads `N new messages from the buyer`.
4. **Skip the solver's own messages.** Both sides of a conversation sign with
   the same `K_sign`, so the watchdog cannot tell who wrote a message. Before
   Mostrix publishes a message of the solver, it sends the watchdog a `sent`
   receipt with the event id; a message with a receipt is never notified. The
   wait gives a receipt that arrives late time to land.
5. **Stop.** Watching a dispute ends on `unwatch` (Mostrix sends it when the
   solver finalizes the dispute), when the dispute's kind-38386 status says it
   is resolved, or on `/unlink`.

The notification never quotes a message: the watchdog cannot decrypt it. The
dispute channel is unaffected; every solver notification is private.

## Telegram commands

Answered in private chats only, like every command.

| Command | What it does |
|---|---|
| `/link` | Replies with a one-time code (valid 10 minutes) and the watchdog's key to enter in Mostrix |
| `/unlink` | Unlinks every solver key linked to this chat and stops watching their disputes |
| `/status` | Lists the solver keys linked to this chat and how many disputes are watched |

Any solver can link a key; they only get notifications for the conversations
their own Mostrix asks the watchdog to watch. A chat links at most 5 keys.

Every watch and every receipt belongs to the key that sent it. A
conversation's `K_sign` is public (it signs every chat event), so a party or
anyone else may link a key of their own and name it: they then get their own
notifications for that conversation, which tell them nothing a relay does not
already show, and the solver's notifications are unaffected. A `sent` receipt
only silences a message for the key that sent it.

## Configuration

```toml
[solver_notifications]
# private_key_env = "WATCHDOG_NOSTR_PRIVATE_KEY"   # the default; shared with [serbero]
# grace_period = 20   # seconds to wait for a `sent` receipt before notifying
```

The watchdog's Nostr key comes from the environment variable, as for Serbero
alerts. With both sections, use the same variable so the watchdog has one key.
Without the section, `/link`, `/unlink` and `/status` answer that solver
notifications are off.

## Protocol (Mostrix → watchdog)

Every message is a Mostro protocol v2 `send-dm`: a `Message::Dm` with action
`send-dm` and a `text_message` payload, wrapped by
`mostro_core::transport::wrap_message_nip44` with the solver's key as the
identity, to the watchdog's pubkey (a NIP-44 kind 14 event). The trade keys
may be ephemeral: the watchdog trusts the identity proven inside the
ciphertext, never the event author.

The text is a JSON object with a version `v` and a `type`. Unknown versions,
types and fields are rejected.

### `link`

```json
{"v": 1, "type": "link", "code": "K7QM-2XPA"}
```

Ties the identity to the Telegram chat that asked for `code`. A code works
once and expires 10 minutes after `/link`. Codes use the letters and digits
`ABCDEFGHJKMNPQRSTUVWXYZ23456789` in two groups of four; the watchdog accepts
them in any case, with or without the dash.

### `watch`

```json
{
  "v": 1,
  "type": "watch",
  "dispute_id": "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a",
  "conversations": [
    {"party": "buyer", "sign_pubkey": "<64 hex: pub(K_sign) of the solver ↔ buyer chat>"},
    {"party": "seller", "sign_pubkey": "<64 hex: pub(K_sign) of the solver ↔ seller chat>"}
  ]
}
```

Accepted only from a linked identity. A `watch` from a key that is not linked
yet is applied once its `link` arrives (relays may deliver them out of
order). Replaces the conversations the watchdog follows for this dispute and
solver; a conversation already followed keeps the time it was first watched
from. A key watches at most 50 disputes at once. `party` is `buyer` or `seller`, each at
most once, with one or two conversations. Only chat events created at or
after the `watch` message are notified, so history the solver already read is
never reported.

### `sent`

```json
{"v": 1, "type": "sent", "event_id": "<64 hex: id of the solver's kind 14 chat event>"}
```

Accepted only from a linked identity, and only for that identity's own
notifications. Send it **before** publishing the chat event (the id is known
once the event is signed). Receipts are kept for two days, at most 2000 per
key.

### `unwatch`

```json
{"v": 1, "type": "unwatch", "dispute_id": "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a"}
```

Accepted only from a linked identity. Stops watching the dispute's
conversations for this solver.

### Ordering and redelivery

Relays may deliver messages late, twice or out of order. For each dispute and
solver, a `watch` or `unwatch` applies only if it is not older (by
`created_at`) than the last one applied. Chat events are notified at most
once.

## Delivery

- The watchdog follows its messages and the watched conversations on its own
  relays (the bootstrap `[nostr] relays`, then the Mostro node's NIP-65
  relays) and catches up on the last day at startup and every 10 minutes, so a
  `watch` sent while it was stopped still applies.
- Each round also fetches Mostro's latest kind-38386 status of every watched
  dispute, however old, so a dispute resolved while the watchdog was stopped
  stops being watched.
- A notification that Telegram refuses is retried a minute later; it is tried
  at most three times in total.

## Limits

- A solver message written from a client that sends no `sent` receipt (for
  example `mostro-cli`), or whose receipt is lost, is notified as if a party
  wrote it.
- A party message that arrives while the watchdog is stopped is notified when
  it catches up, if it is less than a day old.
- The watchdog learns when and how often each watched conversation is used,
  never what is said.
- A party can make the solver get at most one notification per conversation
  every `grace_period` seconds, however many messages they send.
- Notifications are sent from the same loop as the dispute alerts, so a slow
  Telegram API delays both, as it already does for Serbero alerts.
