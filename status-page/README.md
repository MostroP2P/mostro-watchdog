# Mostro Status Page

A static page that shows the live health of Mostro instances, read straight from
their signed Nostr health events (kind 38389, protocol `health_event.md`). It has no
backend and needs no key: it only reads public tags.

For each instance it shows:

- **ok**, **degraded** or **down**, as the instance reports it, while its last valid
  event is fresh;
- **unknown** when there is no valid event, or the last one expired or is older than
  three intervals. Never ok. The card says why;
- when it was last seen, its escrow mode, maintenance mode, version and checks;
- for Lightning instances, LND's block height next to a public explorer's tip for the
  same network (mempool.space, blockstream.info), with a lag of 3 blocks or more
  highlighted. An instance that reports **ok** while 3 or more blocks behind for 15
  minutes (the watchdog's `block_lag_for_secs`) is shown as **degraded**, and the card
  says why: `status = ok` is not proof. The lag is counted from when this page first
  saw it, so a reload starts the count again. On a network without explorers
  (regtest) there is no comparison.

It also lists the **watchers**: monitoring tools such as mostro-watchdog that publish a
heartbeat (`z` = `watcher`), with what they watch and whether they are alive.

The page applies the spec's reader rules to every event: signature, `d` = author,
well-formed and unrepeated tags, `interval` in 30–3600, `expiration` exactly
`created_at + 3 × interval`, no more than 300 s in the future, and the highest `seq`
per pubkey. Events that fail are ignored (logged at debug level in the browser
console).

The spec treats a page as a stateless reader. As best-effort hardening, the page
remembers in the browser's `localStorage`, when it is available:

- the highest `seq` (and event id) accepted per publisher, so a reload doesn't take
  an older revision a relay still serves;
- the instances and watchers it has seen (name or software and last seen, forgotten
  after 30 days), so one whose events are no longer served shows as **unknown** with
  "last seen" instead of vanishing.

Reading, merging and writing the stored revisions happens under a Web Locks lock
(`navigator.locks`) when the browser has the API, so two tabs can't interleave; without
it, the merge still never lowers what another tab stored, and a tab that hears of a
change writes the winning revisions back. One rule orders the revisions of a publisher
everywhere: a higher `seq` wins, and on
an equal `seq` the lower event id. Each incoming event is judged against what any tab
has stored by then, writes merge with the stored data by the same rule, and when
another tab stores a revision that beats the one shown here, this tab shows the
publisher as waiting until that revision arrives. Tabs and reloads therefore always
agree. (The watchdog also lets its persisted event id win a tie; the spec treats a page
as a stateless reader, and lowest id gives every tab the same answer.)
Without storage (private mode, some `file://` setups) it works as before.
A relay that cannot be connected when the page opens (offline, or a slow first
connection) is tried again with a doubling delay, from 15 s up to 5 minutes between
tries, for as long as the page is open; nostr-tools alone would drop it for good. Explorer
tips are used for 30 minutes after they were fetched; when every explorer has failed
for longer, the card says "no reference" instead of comparing with a stale tip.

## Hosting

It is one HTML file. nostr-tools comes from jsDelivr, pinned to an exact version
with a Subresource Integrity hash. Anything that serves static files works:

- **GitHub Pages**: publish the `status-page/` directory (or copy `index.html` to the
  root of a Pages repository).
- **Any web server**: copy `index.html` to the document root.
- **Locally**: `python3 -m http.server --directory status-page 8080` and open
  <http://localhost:8080/>.

## Options

All options are URL query parameters:

| Parameter | Example | Effect |
|---|---|---|
| `relays` | `?relays=wss://relay.mostro.network,wss://nos.lol` | Relays to read from, and to discover instances and their relay lists on (default: `relay.mostro.network`, `nos.lol`, `relay.damus.io`). Without `instance`, only these relays are searched. Entries that aren't `ws:`/`wss:` URLs with a host are named in a warning and skipped; with none valid, the defaults are used |
| `instance` | `?instance=npub1...,npub1...` | Show only these instances (npub or hex), and show them as unknown until a valid event arrives. Their NIP-65 relay lists (kind 10002) are read from the `relays` relays (again each time one of them connects, for the instances whose list has not been found yet), and their health events are also read from the relays those lists say they write to (at most 8, and never one on localhost or a private network: a list is the publisher's, and must not open connections inside the visitor's network). Invalid keys are named in a warning; if none is valid, nothing is shown |
| `watchers` | `?watchers=0` | Hide the watchers |

## Development

The reader rules live between the `BEGIN health-lib` and `END health-lib` markers of
`index.html`, as pure functions. The tests read them from there:

```sh
node --test status-page/health.test.mjs
```

To try the page against synthetic events, run a local relay (for example
`nak serve`), publish kind-38389 events to it with `nak event`, and open the page with
`?relays=ws://localhost:10547`.
