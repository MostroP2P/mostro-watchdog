# Mostro Health Monitoring

The watchdog alerts when the Mostro instance stops **working**, not only when it
stops running. It reads signed Nostr events: it checks that they keep arriving and
checks what they say.

This exists because of the 2026-10-14 incident. A production instance completed no
outgoing payment for about 21 hours. Its LND (neutrino backend) had stalled at block
971349, so every buyer payout was rejected. Meanwhile hold invoices kept being
created, incoming payments kept arriving, the info event kept being published and
disputes kept flowing. Nothing the watchdog watched at the time changed. See
[mostro-watchdog#46](https://github.com/MostroP2P/mostro-watchdog/issues/46).

Telegram only gets **state changes**:

- an alert when a condition starts,
- a reminder every `renotify_interval` while a critical one lasts,
- a "resolved" message with its duration when it clears.

There is never a periodic "all good" message (#16). Silence in the channel means
nothing changed. It doesn't prove the instance is healthy: that is what the
watchdog's own heartbeat and peer watchdogs are for (below).

## Phase 0: the info event

Works with any mostrod, no changes needed. The watchdog reads Mostro's info event
(kind 38385) every `check_interval` and:

- alerts when the newest one is older than `info_silence_threshold`: mostrod
  probably stopped;
- reports when `maintenance_mode` turns `"true"` and when it turns back.

The info event doesn't carry its interval, so set `info_silence_threshold` to three
times the instance's `publish_mostro_info_interval`. The default, 10800 s, suits an
interval of an hour. With mostrod's default of 300 s, use 900.

This would not have caught the 2026-10-14 incident: mostrod was alive. That needs
phase 1.

## Phase 1: the health event

Turn it on with `mostro_health_enabled = true` once the instance runs a mostrod that
publishes the health event (kind 38389, specified in the protocol's
[`health_event.md`](https://github.com/MostroP2P/protocol/pull/84); publisher:
[mostro#1063](https://github.com/MostroP2P/mostro/issues/1063)). It is off by
default: with no event at all, Mostro would be reported as silent.

### What the watchdog trusts

It follows the spec's "What a reader owes":

- It verifies the signature, the pubkey, the kind, `z` = `health` and `d` = the
  pubkey, and that every tag is well formed. A repeated tag, or a check name
  repeated, rejects the event.
- `interval` must be within 30–3600 and `expiration` exactly
  `created_at + 3 × interval`.
- An event dated more than 300 s in the future is ignored.
- It keeps the revision with the highest `seq`. The highest `seq` accepted per
  pubkey is stored in `disputes.db`, so after a restart a relay serving an older,
  unexpired revision is still not believed.
- A missing, expired or stale event (older than three intervals) means
  **unknown, never ok**.

### The encrypted details

The public tags carry a coarse state per check, the block height and the network.
Liquidity, payout counts, failure reasons and HTLC expiry travel encrypted (NIP-44)
to one key: set that key's pubkey as the health recipient in mostrod. The watchdog
decrypts with the key in `WATCHDOG_HEALTH_DECRYPT_KEY` (`decrypt_key_env`), or with
its own `WATCHDOG_NOSTR_PRIVATE_KEY` when that variable is unset, and logs the pubkey
to set at startup.

Without a key, or when the details are for another key, can't be decrypted, use an
unknown version, contradict the public tags, (on a Lightning instance) lack the
`lnd` or `payouts` object, have no `generated_at`, or were generated more than one
`interval` before the event that carries them (a payload republished from an earlier
round), the watchdog checks the public tags only, and logs why once.

### Rules

An alert fires only after `consecutive_samples` bad readings in a row (default 3, one
per `check_interval`), and after the rule's minimum duration. It is resolved after as
many good readings in a row. A reading the watchdog can't make (stale event, no
details, no reference height) counts for neither.

| Alert | When (defaults) | Severity | Needs |
|---|---|---|---|
| Mostro info event silent | info event older than `info_silence_threshold` | Critical | phase 0 |
| Mostro in maintenance mode | `maintenance_mode` is `true` | Warning | phase 0 |
| Mostro is silent | health event older than 3 × `interval`, past `expiration`, or never seen | Critical | phase 1 |
| Mostro reports itself down | `status` = `down` | Critical | phase 1 |
| Mostro reports itself degraded | `status` = `degraded` for 10 min (not counting maintenance) | Warning | phase 1 |
| LND not synced to chain | `lnd_synced_to_chain` = `false` for 10 min | Critical | phase 1 |
| LND behind the chain | 3 blocks or more behind the reference for 15 min | Critical | phase 1 |
| Buyer payouts stuck | an order settled-but-unpaid for 2 h and no successful payout for 2 h | Critical | details |
| Buyer payouts failing | more than 50 % of at least 3 payout attempts failed in the event's window, spread over 2 orders or more or with an `insufficient_balance` failure (one order retried against one bad invoice is the buyer's problem, not the node's) | Critical | details |
| HTLC close to expiry | nearest pending HTLC expires in fewer than 72 blocks | Critical | details |
| Cashu mint failing | the `cashu_mint` check is `fail` (Cashu instances) | Critical | phase 1 |
| Cashu mint degraded | the `cashu_mint` check is `warn` (Cashu instances) | Warning | phase 1 |
| Orders waiting for their payout | oldest settled-but-unpaid order older than 30 min, or a stale one left (settled over a week ago, never attempted: nothing in mostrod will pay it, so it warns until the operator clears it and never counts as stuck payouts) | Warning | details |
| Inactive Lightning channels | a channel inactive for 30 min | Warning | details |
| Peer watchdog silent | a peer's heartbeat older than 3 × its interval, or never seen | Warning | `peer_watchdogs` |

The thresholds are in `[health.thresholds]`. The failure rate uses the window the
event reports (`window_secs`, one hour in version 1), not a window of the watchdog's
own.

When LND admits it isn't synced, the block-lag and unsynced rules date from LND's own
`unsynced_since`, so an incident that started before the watchdog did is alerted
once, all at once. When LND claims to be synced but lags, which is the dangerous
case, the lag must last 15 minutes as seen by the watchdog.

### The block-height cross-check

The daemon can't detect a stale chain by itself: on 2026-10-14 its own height was the
problem. The watchdog compares `lnd_block_height` with:

1. the explorers in `block_height_sources` for the instance's `lnd_network` (the
   highest answer wins; defaults for mainnet, testnet, testnet4 and signet);
2. the median `lnd_block_height` of the instances listed in `trusted_peers` on the
   same network, from their fresh health events, when at least three are fresh. Only
   instances you trust: anyone can publish a validly signed health event with a
   made-up height, so discovered instances never count. Empty by default, which
   means explorers only; explorers queried directly are the preferred reference.

The higher of the two is the reference. Heights only compare within one network: on a
network with neither (regtest, simnet) the cross-check is skipped, never made against
another chain.

### Telling "relays down" from "Mostro silent"

A relay outage looks exactly like a silent Mostro. Every round the watchdog
publishes its own heartbeat and reads it back (without a heartbeat, it checks that
any relay is connected). When no relay returns it, the watchdog holds the silence
alerts, which are unknown until a relay answers, and logs the outage. Its own relays
are its infrastructure, so the outage never reaches Telegram, only the log.

When Mostro comes back after being silent and its details report a relay outage of
its own in the last hour, the "resolved" message says how long its relays were
unreachable.

## The watchdog's heartbeat

With a key, `publish_heartbeat = true` (the default) and either
`mostro_health_enabled = true` or some `peer_watchdogs`, the watchdog publishes an
addressable kind-38389 event with `z` = `watcher` every `check_interval`. Upgrading
alone never starts publishing: on phase 0 only, nothing is published.

```json
{
  "kind": 38389,
  "tags": [
    ["d", "<watchdog pubkey>"],
    ["y", "mostro-watchdog"],
    ["z", "watcher"],
    ["watching", "<Mostro pubkey>"],
    ["interval", "60"],
    ["seq", "1792000000500"],
    ["expiration", "1792000180"]
  ],
  "content": ""
}
```

Its `seq` and `created_at` keep rising across restarts and clock steps (both are
stored in `disputes.db`). If the clock went back by more than 300 s, the watchdog
logs an error: readers will drop the heartbeat as coming from the future until the
clock catches up.

The `watching` tag (shown above) links the watchdog's key to the instance in public,
so it is off by default; turn it on with `heartbeat_watching_tag = true`.

### Watching the watchdog

Run **two watchdogs on different machines**, outside the failure domain of mostrod and
LND. Give each one the other's pubkey in `peer_watchdogs`: each alerts when the other
goes silent.

- Each watchdog needs **its own** `WATCHDOG_NOSTR_PRIVATE_KEY`: it signs the
  heartbeat, and heartbeats from one key share one addressable event, so a live
  watchdog would hide a dead one.
- To let both read the encrypted details, which mostrod addresses to a single key,
  give them the **same** `WATCHDOG_HEALTH_DECRYPT_KEY` and set its pubkey as the health
  recipient in mostrod.

## Configuration

All keys go in `[health]`, next to the watchdog's own process monitoring
(`HEALTH_CHECK.md`):

```toml
[health]
# Phase 0
info_silence_threshold = 10800   # 3 × publish_mostro_info_interval; 0 = off
alert_on_maintenance = true

# Phase 1
mostro_health_enabled = false    # turn on once mostrod publishes kind 38389
check_interval = 60              # seconds between rounds; also the heartbeat interval
consecutive_samples = 3
renotify_interval = 3600         # critical reminders; 0 = never
trusted_peers = []               # instances trusted for the height median
publish_heartbeat = true         # once phase 1 or peer_watchdogs is on
heartbeat_watching_tag = false
peer_watchdogs = []              # npub or hex
# alerts_chat_id = -100123       # defaults to [telegram].chat_id
# private_key_env = "WATCHDOG_NOSTR_PRIVATE_KEY"    # signs the heartbeat; one per watchdog
# decrypt_key_env = "WATCHDOG_HEALTH_DECRYPT_KEY"   # decrypts the details; may be shared

[health.block_height_sources]
mainnet = [
  "https://mempool.space/api/blocks/tip/height",
  "https://blockstream.info/api/blocks/tip/height",
]

[health.thresholds]
block_lag_blocks = 3
block_lag_for_secs = 900
unsynced_for_secs = 600
no_payout_success_secs = 7200
payout_failure_rate = 0.5
payout_failure_min_attempts = 3
htlc_expiry_blocks = 72
settled_unpaid_warn_secs = 1800
inactive_channels_warn_secs = 1800
degraded_for_secs = 600
```

A `[health.block_height_sources]` table replaces the defaults: list every network
you need.

`WATCHDOG_NOSTR_PRIVATE_KEY` is the same key Serbero alerts and solver notifications
use. Both keys are optional here: without a signing key no heartbeat is published,
and without any key only the public tags are read.

## Storage

Open incidents, the highest `seq` accepted per pubkey and the heartbeat's `seq` and
`created_at` live in `disputes.db`, next to the dispute messages. A restart neither
repeats an alert nor forgets one that is still open.

## Limits

- One Mostro instance per watchdog, like the rest of the watchdog.
- Explorer answers are trusted as the chain tip. Two explorers, plus the peer median
  when available, make a single wrong answer unlikely to matter. A wrong high answer
  still could: the highest reference wins, so that a stale instance is never
  excused.
- Alerts go to one Telegram chat. There is no escalation beyond Telegram yet.
