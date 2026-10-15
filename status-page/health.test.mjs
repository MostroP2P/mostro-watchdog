// Tests of the status page's reader rules. The page is one HTML file, so
// the rules are read from between its `BEGIN health-lib` / `END health-lib`
// markers and imported as a module. Signatures are checked in the browser
// by nostr-tools; here `verify` is a stub.
//
// Run: node --test status-page/health.test.mjs

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
const lib = html.slice(html.indexOf('// BEGIN health-lib'), html.indexOf('// END health-lib'));
const {
  parseHealthEvent, isFresh, displayState, keepNewest, ago, acceptable, freshTip, remember, forgetOld,
  KNOWN_MAX_AGE, TIP_MAX_AGE,
} = await import(
  `data:text/javascript;base64,${Buffer.from(lib).toString('base64')}`
);

const NOW = 1792000000;
const PK = 'a'.repeat(64);
const yes = () => true;

function instance(overrides = {}, { createdAt = NOW, content = '' } = {}) {
  const tags = {
    d: [PK],
    y: ['mostro', 'Test Mostro'],
    z: ['health'],
    status: ['ok'],
    escrow_mode: ['lightning'],
    lnd_block_height: ['971485'],
    lnd_synced_to_chain: ['true'],
    lnd_network: ['mainnet'],
    maintenance_mode: ['false'],
    mostro_version: ['0.19.3'],
    interval: ['60'],
    seq: ['7'],
    expiration: [String(createdAt + 180)],
    ...overrides,
  };
  const list = Object.entries(tags)
    .filter(([, v]) => v !== null)
    .map(([k, v]) => [k, ...v]);
  for (const name of ['lnd_sync', 'payouts', 'relays', 'database']) {
    list.push(['check', name, 'ok', '1791900000']);
  }
  return { id: 'e'.repeat(64), pubkey: PK, kind: 38389, created_at: createdAt, tags: list, content };
}

test('the spec example parses into a report', () => {
  const result = parseHealthEvent(instance(), NOW, yes);

  assert.equal(result.valid, true);
  assert.equal(result.report.name, 'Test Mostro');
  assert.equal(result.report.blockHeight, 971485);
  assert.equal(result.report.network, 'mainnet');
  assert.equal(result.report.checks.length, 4);
});

test('a bad signature is rejected', () => {
  assert.equal(parseHealthEvent(instance(), NOW, () => false).reason, 'invalid signature');
});

test('d must be the author', () => {
  assert.equal(parseHealthEvent(instance({ d: ['health'] }), NOW, yes).reason, 'd is not the author');
});

test('interval, seq and expiration follow the spec', () => {
  assert.equal(parseHealthEvent(instance({ interval: ['29'], expiration: [String(NOW + 87)] }), NOW, yes).valid, false);
  assert.equal(parseHealthEvent(instance({ seq: ['07'] }), NOW, yes).reason, 'bad seq');
  assert.equal(parseHealthEvent(instance({ expiration: [String(NOW + 181)] }), NOW, yes).reason, 'bad expiration');
});

test('an event more than 300 s in the future is ignored', () => {
  const future = instance({}, { createdAt: NOW + 301 });

  assert.equal(parseHealthEvent(future, NOW, yes).reason, 'dated in the future');
  assert.equal(parseHealthEvent(instance({}, { createdAt: NOW + 300 }), NOW, yes).valid, true);
});

test('a repeated tag or check name rejects the event', () => {
  const repeated = instance();
  repeated.tags.push(['seq', '7']);
  const twice = instance();
  twice.tags.push(['check', 'relays', 'fail', '1791900000']);

  assert.equal(parseHealthEvent(repeated, NOW, yes).reason, 'repeated seq');
  assert.equal(parseHealthEvent(twice, NOW, yes).reason, 'bad check');
});

test('p is present if and only if there is content', () => {
  assert.equal(parseHealthEvent(instance({}, { content: 'abc' }), NOW, yes).valid, false);
  assert.equal(parseHealthEvent(instance({ p: ['b'.repeat(64)] }), NOW, yes).valid, false);
  assert.equal(parseHealthEvent(instance({ p: ['b'.repeat(64)] }, { content: 'abc' }), NOW, yes).valid, true);
});

test('unknown check names are skipped and unknown states read as unknown', () => {
  const event = instance();
  event.tags = event.tags.filter((t) => t[0] !== 'check');
  event.tags.push(['check', 'lnd_sync', 'weird', '1791900000'], ['check', 'quantum', 'fail', '1791900000']);

  const { report } = parseHealthEvent(event, NOW, yes);

  assert.deepEqual(report.checks, [{ name: 'lnd_sync', state: 'unknown', since: 1791900000 }]);
});

test('a missing or stale event is unknown, never ok', () => {
  const { report } = parseHealthEvent(instance(), NOW, yes);

  assert.equal(displayState(undefined, NOW), 'unknown');
  assert.equal(displayState(report, NOW + 179), 'ok');
  assert.equal(displayState(report, NOW + 180), 'unknown');
  assert.equal(isFresh(report, NOW + 180), false);
});

test('a watcher heartbeat needs no instance tags and may watch several instances', () => {
  const event = {
    id: 'f'.repeat(64),
    pubkey: PK,
    kind: 38389,
    created_at: NOW,
    content: '',
    tags: [
      ['d', PK],
      ['y', 'mostro-watchdog'],
      ['z', 'watcher'],
      ['watching', 'b'.repeat(64)],
      ['watching', 'c'.repeat(64)],
      ['interval', '60'],
      ['seq', '88213'],
      ['expiration', String(NOW + 180)],
    ],
  };

  const { valid, report } = parseHealthEvent(event, NOW, yes);
  assert.equal(valid, true);
  assert.equal(report.shape, 'watcher');
  assert.equal(report.watching.length, 2);
  assert.equal(displayState(report, NOW), 'ok');

  event.tags.push(['watching', 'b'.repeat(64)]);
  assert.equal(parseHealthEvent(event, NOW, yes).reason, 'bad watching');
});

test('the highest seq wins', () => {
  const store = new Map();
  const at = (seq) => parseHealthEvent(instance({ seq: [String(seq)] }), NOW, yes).report;

  assert.equal(keepNewest(store, at(5)), true);
  assert.equal(keepNewest(store, at(4)), false);
  assert.equal(keepNewest(store, at(5)), false);
  assert.equal(keepNewest(store, at(6)), true);
  assert.equal(store.get(PK).seq, 6);
});

test('durations read naturally', () => {
  assert.equal(ago(40), '40 s');
  assert.equal(ago(420), '7 min');
  assert.equal(ago(74460), '20 h 41 min');
});

test('a revision older than the one accepted on an earlier visit is refused', () => {
  const at = (seq, id = 'e'.repeat(64)) => ({ ...parseHealthEvent(instance({ seq: [String(seq)] }), NOW, yes).report, id });
  const remembered = { seq: 7, id: 'e'.repeat(64) };

  assert.equal(acceptable(at(6), remembered), false);
  assert.equal(acceptable(at(7), remembered), true, 'the same event restores the state');
  assert.equal(acceptable(at(7, 'f'.repeat(64)), remembered), false, 'same seq, other event');
  assert.equal(acceptable(at(8), remembered), true);
  assert.equal(acceptable(at(1), undefined), true);

  const store = new Map();
  assert.equal(keepNewest(store, at(6), remembered), false);
  assert.equal(store.size, 0);
  assert.equal(keepNewest(store, at(7), remembered), true);
});

test('an explorer tip is used only while it is fresh', () => {
  const tip = { height: 971485, source: 'mempool.space', at: NOW };

  assert.equal(freshTip(tip, NOW + TIP_MAX_AGE), tip);
  assert.equal(freshTip(tip, NOW + TIP_MAX_AGE + 1), null);
  assert.equal(freshTip(undefined, NOW), null);
});

test('instances seen before are remembered, and forgotten after a month', () => {
  const { report } = parseHealthEvent(instance(), NOW, yes);

  const known = remember({}, report, NOW);
  assert.deepEqual(known[PK], { name: 'Test Mostro', lastSeen: NOW });

  // An older revision doesn't move "last seen" back.
  const older = { ...report, createdAt: NOW - 600 };
  assert.equal(remember(known, older, NOW)[PK].lastSeen, NOW);

  assert.deepEqual(forgetOld(known, NOW + KNOWN_MAX_AGE + 1), {});
  // Garbage from storage is dropped, not trusted.
  assert.deepEqual(forgetOld({ nothex: { lastSeen: NOW }, [PK]: { lastSeen: 'x' } }, NOW), {});
});
