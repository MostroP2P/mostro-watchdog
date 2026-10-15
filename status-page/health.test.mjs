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
  parseHealthEvent, isFresh, displayState, keepNewest, ago, better, superseded, freshTip, remember, forgetOld,
  KNOWN_MAX_AGE, TIP_MAX_AGE, parseInstanceParam, mergeAccepted, mergeKnown, parseRelayParam, explorersFor,
  syncAccepted, withLock,
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

test('one rule orders revisions: higher seq, then lower id', () => {
  const r = (seq, id) => ({ seq, id });

  assert.equal(better(r(8, 'b'), r(7, 'a')), true);
  assert.equal(better(r(7, 'a'), r(7, 'b')), true);
  assert.equal(better(r(7, 'b'), r(7, 'a')), false);
  assert.equal(better(r(7, 'a'), r(7, 'a')), false, 'never beats itself');
  assert.equal(better(r(1, 'z'), undefined), true);
  assert.equal(better(undefined, r(1, 'z')), false);
});

test('a revision another tab beat is refused, the stored one restored', () => {
  const at = (seq, id) => ({ ...parseHealthEvent(instance({ seq: [String(seq)] }), NOW, yes).report, id });
  const stored = { seq: 9, id: 'e'.repeat(64) };

  // Another tab persisted seq 9: this tab must not show seq 8.
  const store = new Map();
  assert.equal(keepNewest(store, at(8, 'e'.repeat(64)), stored), false);
  assert.equal(store.size, 0);
  // The stored revision itself is taken when it arrives.
  assert.equal(keepNewest(store, at(9, 'e'.repeat(64)), stored), true);
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

test('integers past 2^53 − 1 are refused instead of rounded', () => {
  const unsafe = '9007199254740993';

  assert.equal(parseHealthEvent(instance({ lnd_block_height: [unsafe] }), NOW, yes).reason, 'bad lnd_block_height');
  assert.equal(parseHealthEvent(instance({ seq: [unsafe] }), NOW, yes).reason, 'bad seq');
  const unsafeTime = instance();
  unsafeTime.created_at = 2 ** 60;
  assert.equal(parseHealthEvent(unsafeTime, NOW, yes).reason, 'bad created_at');
  const bigSince = instance();
  bigSince.tags.find((t) => t[0] === 'check')[3] = unsafe;
  assert.equal(parseHealthEvent(bigSince, NOW, yes).reason, 'bad check');
});

test('a tie on seq goes to the lower id, whatever the arrival order or storage', () => {
  const at = (id) => ({ ...parseHealthEvent(instance(), NOW, yes).report, id });
  const low = at('1'.repeat(64));
  const high = at('9'.repeat(64));

  for (const order of [[low, high], [high, low]]) {
    const store = new Map();
    for (const report of order) keepNewest(store, report, undefined);
    assert.equal(store.get(PK).id, low.id);
  }

  // Storage holding the high id doesn't stop the low id.
  const store = new Map();
  keepNewest(store, high, { seq: high.seq, id: high.id });
  assert.equal(keepNewest(store, low, { seq: high.seq, id: high.id }), true);
  assert.equal(store.get(PK).id, low.id);
});

test('an instance parameter with no valid key is an error, not every instance', () => {
  const toHex = (k) => (/^[0-9a-f]{64}$/.test(k) ? k : null);

  assert.deepEqual(parseInstanceParam(null, toHex), { given: false, keys: [], invalid: [] });
  assert.deepEqual(parseInstanceParam('nope,bad', toHex), { given: true, keys: [], invalid: ['nope', 'bad'] });
  // A typo next to a valid key is named, not silently dropped.
  assert.deepEqual(parseInstanceParam(`${PK},typo,${PK}`, toHex), { given: true, keys: [PK], invalid: ['typo'] });
});

test('an optional tag present without a value rejects the event', () => {
  for (const name of ['lnd_block_height', 'lnd_synced_to_chain', 'lnd_network']) {
    const event = instance({ [name]: null });
    event.tags.push([name]);
    assert.equal(parseHealthEvent(event, NOW, yes).reason, `bad ${name}`, name);
  }
  const withP = instance({}, { content: 'abc' });
  withP.tags.push(['p']);
  assert.equal(parseHealthEvent(withP, NOW, yes).reason, 'bad p');
});

test('watchers are remembered with their software', () => {
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
      ['interval', '60'],
      ['seq', '1'],
      ['expiration', String(NOW + 180)],
    ],
  };
  const { report } = parseHealthEvent(event, NOW, yes);

  assert.deepEqual(remember({}, report, NOW)[PK], { name: 'mostro-watchdog', lastSeen: NOW });
});

test('merging storage keeps the tie-break: the lower id survives an equal seq', () => {
  const A = 'a'.repeat(64);
  const stored = { [A]: { seq: 7, id: '9'.repeat(64) } };
  const mine = { [A]: { seq: 7, id: '1'.repeat(64) } };

  assert.deepEqual(mergeAccepted(stored, mine)[A], { seq: 7, id: '1'.repeat(64) });
  assert.deepEqual(mergeAccepted(mine, stored)[A], { seq: 7, id: '1'.repeat(64) });
});

test('a revision shown here that another tab beat is marked as waiting', () => {
  const at = (seq) => ({ ...parseHealthEvent(instance({ seq: [String(seq)] }), NOW, yes).report });
  const shown = new Map([[PK, at(8)]]);

  assert.deepEqual(superseded(shown, { [PK]: { seq: 9, id: 'x' } }), [PK]);
  assert.deepEqual(superseded(shown, { [PK]: { seq: 8, id: shown.get(PK).id } }), []);
  assert.deepEqual(superseded(shown, {}), []);
});

test('two tabs merging their accepted seq never lower each other', () => {
  const A = 'a'.repeat(64);
  const B = 'b'.repeat(64);
  const stored = { [A]: { seq: 9, id: 'x' }, [B]: { seq: 3, id: 'y' } };
  // This tab loaded earlier: stale for A, newer for B.
  const mine = { [A]: { seq: 5, id: 'old' }, [B]: { seq: 4, id: 'z' } };

  assert.deepEqual(mergeAccepted(stored, mine), {
    [A]: { seq: 9, id: 'x' },
    [B]: { seq: 4, id: 'z' },
  });
  // Garbage from storage is dropped.
  assert.deepEqual(mergeAccepted({ nope: { seq: 1, id: 'q' }, [A]: { seq: 'x' } }, {}), {});
});

test('two tabs merging known publishers keep the latest sighting of each', () => {
  const A = 'a'.repeat(64);
  const B = 'b'.repeat(64);
  const stored = { [A]: { name: 'A', lastSeen: NOW }, [B]: { name: 'B', lastSeen: NOW - 100 } };
  const mine = { [A]: { name: 'A', lastSeen: NOW - 50 }, [B]: { name: 'B', lastSeen: NOW } };

  const merged = mergeKnown(stored, mine, NOW);

  assert.equal(merged[A].lastSeen, NOW);
  assert.equal(merged[B].lastSeen, NOW);
});

test('relay URLs need a ws or wss scheme and a host, and bad ones are named', () => {
  const defaults = ['wss://relay.example'];

  assert.deepEqual(parseRelayParam(null, defaults), { relays: defaults, invalid: [], usedDefaults: true });
  assert.deepEqual(
    parseRelayParam('wss://,https://x.org,nonsense,wss://relay.mostro.network', defaults),
    { relays: ['wss://relay.mostro.network/'], invalid: ['wss://', 'https://x.org', 'nonsense'], usedDefaults: false },
  );
  assert.deepEqual(parseRelayParam('wss://', defaults), { relays: defaults, invalid: ['wss://'], usedDefaults: true });
  assert.deepEqual(parseRelayParam('ws://localhost:10547', defaults).relays, ['ws://localhost:10547/']);
});

test('a network name never reaches an object prototype', () => {
  for (const name of ['toString', 'constructor', '__proto__', 'hasOwnProperty']) {
    assert.deepEqual(explorersFor(name), [], name);
    assert.equal(parseHealthEvent(instance({ lnd_network: [name] }), NOW, yes).reason, 'bad lnd_network', name);
  }
  assert.equal(explorersFor('mainnet').length, 2);
  assert.deepEqual(explorersFor('regtest'), []);
  assert.equal(parseHealthEvent(instance({ lnd_network: ['regtest'] }), NOW, yes).valid, true);
});

test('an empty instance parameter is an error, not every instance', () => {
  const toHex = (k) => (/^[0-9a-f]{64}$/.test(k) ? k : null);
  const params = (query) => new URLSearchParams(query);

  for (const query of ['instance=', 'instance=,,,', 'instance= , ']) {
    const p = params(query);
    const parsed = parseInstanceParam(p.get('instance'), toHex, p.has('instance'));
    assert.equal(parsed.given, true, query);
    assert.deepEqual(parsed.keys, [], query);
  }
  const none = params('relays=wss://x.org');
  assert.equal(parseInstanceParam(none.get('instance'), toHex, none.has('instance')).given, false);
});

/** A localStorage stand-in: one value, counting writes. */
function fakeStorage(initial = {}) {
  let raw = JSON.stringify(initial);
  return {
    writes: 0,
    read: () => JSON.parse(raw),
    write(value) {
      this.writes += 1;
      raw = JSON.stringify(value);
    },
    get value() {
      return JSON.parse(raw);
    },
  };
}

/** A Web Locks stand-in that runs requests one after another. */
function fakeLocks() {
  let queue = Promise.resolve();
  const held = [];
  return {
    held,
    request(name, critical) {
      const run = queue.then(async () => {
        held.push(name);
        try {
          return await critical();
        } finally {
          held.pop();
        }
      });
      queue = run.catch(() => {});
      return run;
    },
  };
}

test('read-merge-write under the lock keeps every tab\'s best revision', async () => {
  const A = 'a'.repeat(64);
  const B = 'b'.repeat(64);
  const storage = fakeStorage({ [A]: { seq: 3, id: 'x' } });
  const locks = fakeLocks();
  // Two tabs loaded stale copies and write at the same time.
  const tab1 = { [A]: { seq: 9, id: 'x' } };
  const tab2 = { [A]: { seq: 4, id: 'x' }, [B]: { seq: 7, id: 'y' } };

  await Promise.all([
    withLock(locks, 'lock', () => syncAccepted(storage.read, (v) => storage.write(v), tab1)),
    withLock(locks, 'lock', () => syncAccepted(storage.read, (v) => storage.write(v), tab2)),
  ]);

  assert.deepEqual(storage.value, { [A]: { seq: 9, id: 'x' }, [B]: { seq: 7, id: 'y' } });
});

test('an unchanged merge writes nothing, so tabs never wake each other in a loop', () => {
  const A = 'a'.repeat(64);
  const storage = fakeStorage({ [A]: { seq: 9, id: 'x' } });

  syncAccepted(storage.read, (v) => storage.write(v), { [A]: { seq: 5, id: 'x' } });
  syncAccepted(storage.read, (v) => storage.write(v), {});
  assert.equal(storage.writes, 0);

  syncAccepted(storage.read, (v) => storage.write(v), { [A]: { seq: 10, id: 'x' } });
  assert.equal(storage.writes, 1);
});

test('without the lock API the critical section still runs', async () => {
  let ran = false;
  const result = await withLock(undefined, 'lock', () => {
    ran = true;
    return 42;
  });

  assert.equal(ran, true);
  assert.equal(result, 42);
  const locks = fakeLocks();
  await withLock(locks, 'mostro-status:accepted', () => assert.deepEqual(locks.held, ['mostro-status:accepted']));
});
