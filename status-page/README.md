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
  highlighted. On a network without explorers (regtest) there is no comparison.

It also lists the **watchers**: monitoring tools such as mostro-watchdog that publish a
heartbeat (`z` = `watcher`), with what they watch and whether they are alive.

The page applies the spec's reader rules to every event: signature, `d` = author,
well-formed and unrepeated tags, `interval` in 30–3600, `expiration` exactly
`created_at + 3 × interval`, no more than 300 s in the future, and the highest `seq`
per pubkey. Events that fail are ignored (logged at debug level in the browser
console). As a stateless reader it can't remember the highest `seq` across reloads;
the freshness rule limits a replay to an event at most three intervals old.

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
| `relays` | `?relays=wss://relay.mostro.network,wss://nos.lol` | Relays to read from (default: `relay.mostro.network`, `nos.lol`, `relay.damus.io`) |
| `instance` | `?instance=npub1...,npub1...` | Show only these instances (npub or hex), and show them as unknown until a valid event arrives |
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
