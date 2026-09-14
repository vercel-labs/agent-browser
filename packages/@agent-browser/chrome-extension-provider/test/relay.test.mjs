// Real WebSocket contract tests against a mock extension, not Chrome acceptance tests.
import assert from 'node:assert/strict';
import { once } from 'node:events';
import http from 'node:http';
import test from 'node:test';
import { WebSocket } from 'ws';
import { BridgeDaemon } from '../dist/daemon/server.js';

const secret = 'relay-contract-control-token-32-bytes-long';
const extensionId = 'a'.repeat(32);
const origin = `chrome-extension://${extensionId}`;
const scope = { namespace: '', session: 'work' };
const tab = { tabId: 7, url: 'https://example.test/work', title: 'Work' };
function inbox(ws) {
  const messages = [], waiters = [];
  ws.on('message', raw => {
    const value = JSON.parse(raw.toString());
    const index = waiters.findIndex(item => item.match(value));
    if (index < 0) messages.push(value);
    else { const [item] = waiters.splice(index, 1); clearTimeout(item.timer); item.resolve(value); }
  });
  return match => {
    const index = messages.findIndex(match);
    if (index >= 0) return Promise.resolve(messages.splice(index, 1)[0]);
    return new Promise((resolve, reject) => {
      const item = { match, resolve, timer: setTimeout(() => reject(new Error('Protocol response timed out')), 3000) };
      waiters.push(item);
    });
  };
}
async function rejectSocket(url, options) {
  const ws = new WebSocket(url, options);
  ws.on('error', () => {});
  const response = await new Promise((resolve, reject) => {
    ws.on('unexpected-response', (_req, res) => resolve(res));
    ws.on('open', () => reject(new Error('Unexpected WebSocket authorization')));
  });
  assert.equal(response.statusCode, 403);
  ws.terminate();
}
async function fixture(t) {
  const daemon = new BridgeDaemon({ controlToken: secret });
  const port = await daemon.start(), base = `http://127.0.0.1:${port}`, sockets = [];
  t.after(async () => { sockets.forEach(ws => ws.terminate()); await daemon.stop(); });
  const request = async (path, method = 'GET', body, headers = {}) => {
    const response = await fetch(base + path, { method, headers: { authorization: `Bearer ${secret}`, ...headers },
      ...(body === undefined ? {} : { body: typeof body === 'string' ? body : JSON.stringify(body) }) });
    return { status: response.status, body: await response.json() };
  };
  const connect = async (url, options) => {
    const ws = new WebSocket(url, options), next = inbox(ws);
    sockets.push(ws);
    await once(ws, 'open');
    return { ws, next };
  };
  const setup = async (s = scope) => (await request('/setup', 'POST', s)).body;
  const pair = async (s = scope, profileId = 'test_profile_instance_123') => {
    const prepared = await setup(s);
    const url = `ws://127.0.0.1:${port}/extension?token=${prepared.pairingCode.split(':')[1]}`;
    const peer = await connect(url, { origin });
    peer.ws.send(JSON.stringify({ v: 1, kind: 'hello', profileId, extensionId, chromeVersion: '140.0.0.0' }));
    const paired = await peer.next(m => m.kind === 'paired');
    assert.deepEqual(paired.scope, s);
    assert.equal(paired.requestId, prepared.requestId);
    peer.url = url;
    peer.commands = [];
    peer.respond = () => ({ result: {} });
    peer.ws.on('message', raw => {
      const command = JSON.parse(raw.toString());
      if (command.kind !== 'cdp-command') return;
      peer.commands.push(command);
      const result = peer.respond(command);
      if (result !== undefined) peer.ws.send(JSON.stringify({ v: 1, kind: 'cdp-result', reqId: command.reqId, ...result }));
    });
    peer.emit = (method, params, sessionId, tabId = 7) => peer.ws.send(JSON.stringify({ v: 1, kind: 'cdp-event', method, params, sessionId, tabId }));
    peer.grant = async (value = tab) => {
      peer.ws.send(JSON.stringify({ v: 1, kind: 'grant', tab: value }));
      return peer.next(m => ['granted', 'error'].includes(m.kind));
    };
    return peer;
  };
  const reserve = async (s = scope) => request('/lease', 'POST', s);
  const client = async lease => {
    const value = await connect(lease.cdpUrl);
    let id = 0;
    value.call = (method, params = {}, sessionId) => {
      const requestId = ++id;
      value.ws.send(JSON.stringify({ id: requestId, method, params, sessionId }));
      return value.next(m => m.id === requestId);
    };
    return value;
  };
  const authorized = async () => {
    const peer = await pair();
    await peer.grant();
    const lease = (await reserve()).body, cdp = await client(lease);
    const targetId = (await cdp.call('Target.getTargets')).result.targetInfos[0].targetId;
    const sessionId = (await cdp.call('Target.attachToTarget', { targetId, flatten: true })).result.sessionId;
    return { peer, lease, cdp, targetId, sessionId };
  };
  return { base, port, daemon, request, setup, pair, reserve, client, authorized, connect };
}

test('control auth, exact Host, browser Origin, bounded body and valid scope', async t => {
  const f = await fixture(t);
  assert.equal((await f.request('/health')).status, 200);
  assert.equal((await f.request('/health', 'GET', undefined, { authorization: 'wrong' })).status, 401);
  assert.equal((await f.request('/health', 'GET', undefined, { origin: 'https://example.test' })).status, 403);
  const wrongHost = await new Promise(resolve => {
    http.get(`${f.base}/health`, { headers: { host: `localhost:${f.port}`, authorization: `Bearer ${secret}` } }, res => {
      res.resume(); resolve(res.statusCode);
    });
  });
  assert.equal(wrongHost, 403);
  assert.equal((await f.request('/setup', 'POST', { session: 'work' })).status, 400);
  assert.equal((await f.request('/setup', 'POST', 'x'.repeat(17000))).status, 413);
  assert.equal((await f.reserve()).status, 409);
});

test('pairing is one-time, Origin-bound, and debugger attach is deferred until CDP connects', async t => {
  const f = await fixture(t), prepared = await f.setup();
  const url = `ws://127.0.0.1:${f.port}/extension?token=${prepared.pairingCode.split(':')[1]}`;
  await rejectSocket(url, { origin: 'https://example.test' });
  const bad = await f.connect(url, { origin }), closed = once(bad.ws, 'close');
  bad.ws.send(JSON.stringify({ v: 1, kind: 'hello', profileId: 'test_profile_12345', extensionId: 'b'.repeat(32), chromeVersion: '140' }));
  await closed;
  await rejectSocket(url, { origin });
  const peer = await f.pair();
  await peer.grant();
  assert.equal(peer.commands.length, 0);
  const lease = (await f.reserve()).body;
  assert.equal(peer.commands.length, 0);
  assert.equal((await f.request('/status?namespace=&session=work')).body.state, 'reserved');
  await rejectSocket(peer.url, { origin });
  await rejectSocket(lease.cdpUrl, { origin: 'https://example.test' });
  const cdp = await f.client(lease);
  assert.ok((await cdp.call('Browser.getVersion')).result.product.startsWith('Chrome/'));
  assert.equal(peer.commands[0].method, 'Bridge.attach');
  await rejectSocket(lease.cdpUrl);
});

test('scopes and profiles stay isolated and one profile tab has only one grant', async t => {
  const f = await fixture(t), a = await f.pair(), other = { namespace: 'other', session: 'work' };
  const b = await f.pair(other), c = await f.pair({ namespace: 'third', session: 'work' }, 'different_profile_123');
  assert.equal((await a.grant()).kind, 'granted');
  assert.equal((await b.grant()).kind, 'error');
  assert.equal((await c.grant()).kind, 'granted');
  assert.equal((await f.reserve(other)).status, 409);
  const lease = (await f.reserve()).body, cdp = await f.client(lease);
  await cdp.call('Browser.getVersion');
  assert.equal((await f.request(`/lease/${lease.leaseId}`, 'DELETE')).status, 200);
  assert.equal((await f.request(`/lease/${lease.leaseId}`, 'DELETE')).status, 200);
  await a.next(m => m.kind === 'released');
  assert.ok(a.commands.some(m => m.method === 'Bridge.detach'));
  assert.ok(!a.commands.some(m => ['Page.close', 'Browser.close', 'Target.closeTarget'].includes(m.method)));
  assert.equal((await f.reserve()).status, 409);
  assert.equal((await b.grant()).kind, 'granted');
  await rejectSocket(lease.cdpUrl);
});

test('virtual target routing is scoped and Chrome errors preserve code, text and session', async t => {
  const f = await fixture(t), { peer, cdp, targetId, sessionId } = await f.authorized();
  assert.equal((await cdp.call('Target.getTargets')).result.targetInfos.length, 1);
  assert.equal((await cdp.call('Target.getTargetInfo', { targetId })).result.targetInfo.targetId, targetId);
  for (const method of ['Target.createTarget', 'Target.createBrowserContext', 'Target.closeTarget', 'Browser.close']) assert.ok((await cdp.call(method, { targetId })).error);
  assert.ok((await cdp.call('Target.attachToTarget', { targetId: 'other', flatten: true })).error);
  assert.ok((await cdp.call('Target.activateTarget', { targetId: 'other' })).error);
  assert.ok((await cdp.call('Runtime.evaluate', {}, 'unowned')).error);
  assert.ok((await cdp.call('Runtime.evaluate')).error);
  peer.respond = () => ({ error: { code: -32602, message: 'Chrome rejected the expression' } });
  const result = await cdp.call('Runtime.evaluate', { expression: 'bad' }, sessionId);
  assert.equal(result.sessionId, sessionId);
  assert.deepEqual(result.error, { code: -32602, message: 'Chrome rejected the expression' });
});

test('iframe/worker native sessions route recursively and detach removes descendants', async t => {
  const f = await fixture(t), { peer, cdp, sessionId } = await f.authorized();
  await cdp.call('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, sessionId);
  assert.deepEqual(peer.commands.at(-1).params.filter, [{ type: 'iframe', exclude: false }, { type: 'worker', exclude: false }, { exclude: true }]);
  peer.emit('Target.attachedToTarget', { sessionId: 'native-frame', targetInfo: { targetId: 'frame-id', type: 'iframe', url: 'https://frame.test/' }, waitingForDebugger: true });
  const frame = await cdp.next(m => m.method === 'Target.attachedToTarget'), childId = frame.params.sessionId;
  assert.notEqual(childId, 'native-frame');
  assert.equal(frame.sessionId, sessionId);
  assert.equal(frame.params.targetInfo.targetId, 'frame-id');
  await cdp.call('Runtime.evaluate', { expression: '1' }, childId);
  assert.equal(peer.commands.at(-1).sessionId, 'native-frame');
  assert.ok(peer.commands.some(m => m.method === 'Target.setAutoAttach' && m.sessionId === 'native-frame'));
  peer.emit('Target.attachedToTarget', { sessionId: 'native-worker', targetInfo: { targetId: 'worker-id', type: 'worker', url: 'https://frame.test/worker.js' } }, 'native-frame');
  const worker = await cdp.next(m => m.method === 'Target.attachedToTarget');
  assert.equal(worker.sessionId, childId);
  peer.emit('Runtime.consoleAPICalled', { type: 'log', args: [] }, 'native-worker');
  assert.equal((await cdp.next(m => m.method === 'Runtime.consoleAPICalled')).sessionId, worker.params.sessionId);
  peer.emit('Target.attachedToTarget', { sessionId: 'shared', targetInfo: { targetId: 'shared-id', type: 'shared_worker', url: 'https://frame.test/' } });
  peer.emit('Target.attachedToTarget', { sessionId: 'foreign', targetInfo: { targetId: 'foreign-id', type: 'iframe', url: 'https://evil.test/' } }, undefined, 8);
  assert.deepEqual((await cdp.call('Target.getTargets')).result.targetInfos.map(i => i.type), ['page', 'iframe', 'worker']);
  assert.ok((await cdp.call('Runtime.evaluate', {}, 'native-frame')).error);
  peer.emit('Target.detachedFromTarget', { sessionId: 'native-frame', targetId: 'frame-id' });
  await cdp.next(m => m.method === 'Target.detachedFromTarget' && m.params.sessionId === childId);
  assert.ok((await cdp.call('Runtime.evaluate', {}, childId)).error);
  assert.ok((await cdp.call('Runtime.evaluate', {}, worker.params.sessionId)).error);
  assert.equal((await cdp.call('Target.getTargets')).result.targetInfos.length, 1);
});

test('cookie URL/domain/partition and storage origin are checked before forwarding', async t => {
  const f = await fixture(t), { peer, cdp, sessionId } = await f.authorized();
  await cdp.call('Network.getCookies', {}, sessionId);
  assert.deepEqual(peer.commands.at(-1).params, { urls: [tab.url] });
  assert.deepEqual((await cdp.call('Network.setCookies', { cookies: [{ name: 'test', value: '1', url: tab.url }] }, sessionId)).result, {});
  const denied = [
    ['Network.getCookies', { urls: ['https://elsewhere.test/'] }], ['Network.getCookies', { urls: [] }],
    ['Network.getAllCookies', {}], ['Network.clearBrowserCookies', {}], ['Network.clearBrowserCache', {}],
    ['Network.setCookie', { name: 'test', value: '1', domain: '.example.test' }],
    ['Network.setCookie', { name: 'test', value: '1', domain: 'elsewhere.test' }],
    ['Network.setCookies', { cookies: [{ name: 'test', value: '1', url: tab.url }, { name: 'bad', value: '2', url: 'https://elsewhere.test' }] }],
    ['Network.deleteCookies', { name: 'test', url: 'https://elsewhere.test' }],
    ['Network.setCookie', { name: 'test', value: '1', partitionKey: { topLevelSite: 'https://elsewhere.test', hasCrossSiteAncestor: false } }],
    ['Storage.getCookies', {}], ['Storage.clearCookies', {}],
    ['Storage.clearDataForOrigin', { origin: 'https://elsewhere.test', storageTypes: 'local_storage' }],
    ['Storage.clearDataForOrigin', { origin: 'https://example.test', storageTypes: 'all' }],
    ['DOMStorage.getDOMStorageItems', { storageId: { securityOrigin: 'https://elsewhere.test', isLocalStorage: true } }],
  ];
  for (const [method, params] of denied) {
    const before = peer.commands.length;
    assert.ok((await cdp.call(method, params, sessionId)).error, method);
    assert.equal(peer.commands.length, before, method);
  }
  assert.deepEqual((await cdp.call('Network.setCookie', { name: 'test', value: '1', partitionKey: { topLevelSite: 'https://example.test', hasCrossSiteAncestor: false } }, sessionId)).result, {});
  assert.deepEqual((await cdp.call('Storage.clearDataForOrigin', { origin: 'https://example.test', storageTypes: 'local_storage' }, sessionId)).result, {});
  peer.ws.send(JSON.stringify({ v: 1, kind: 'tab-updated', tab: { ...tab, url: 'https://new.test/' } }));
  assert.ok((await cdp.call('Network.getCookies', { urls: [tab.url] }, sessionId)).error);
});

test('revoke, tab loss, extension loss and CDP loss invalidate old leases', async t => {
  for (const kind of ['revoked', 'tab-updated', 'extension-close', 'client-close']) await t.test(kind, async t => {
    const f = await fixture(t), { peer, cdp, lease } = await f.authorized();
    const closed = once(cdp.ws, 'close');
    if (kind === 'extension-close') peer.ws.terminate();
    else if (kind === 'client-close') cdp.ws.close();
    else peer.ws.send(JSON.stringify({ v: 1, kind, reason: 'user stopped', tab: { ...tab, tabId: 8 } }));
    await closed;
    // Synchronize on the relay's release before inspecting control state.
    if (kind === 'client-close') await peer.next(m => m.kind === 'released');
    assert.equal((await f.reserve()).status, 409);
    await rejectSocket(lease.cdpUrl);
  });
});

test('pending child commands fail on detach and late native replies cannot resurrect the session', async t => {
  const f = await fixture(t), { peer, cdp } = await f.authorized();
  peer.emit('Target.attachedToTarget', { sessionId: 'native-pending', targetInfo: { targetId: 'pending-frame', type: 'iframe', url: 'https://example.test/frame' } });
  const attached = await cdp.next(m => m.method === 'Target.attachedToTarget');
  peer.respond = command => command.method === 'Runtime.evaluate' ? undefined : { result: {} };
  const pending = cdp.call('Runtime.evaluate', { expression: 'new Promise(() => {})' }, attached.params.sessionId);
  const command = await peer.next(m => m.kind === 'cdp-command' && m.method === 'Runtime.evaluate');
  peer.emit('Target.detachedFromTarget', { sessionId: 'native-pending', targetId: 'pending-frame' });
  assert.match((await pending).error.message, /detached/);
  peer.ws.send(JSON.stringify({ v: 1, kind: 'cdp-result', reqId: command.reqId, result: { value: 'late' } }));
  assert.ok((await cdp.call('Runtime.evaluate', {}, attached.params.sessionId)).error);
});

test('pairings expire after five minutes and unused leases revoke after sixty seconds', async t => {
  t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: Date.now() });
  const f = await fixture(t), prepared = await f.setup();
  t.mock.timers.tick(300_001);
  await rejectSocket(`ws://127.0.0.1:${f.port}/extension?token=${prepared.pairingCode.split(':')[1]}`, { origin });
  const peer = await f.pair();
  await peer.grant();
  const lease = (await f.reserve()).body;
  t.mock.timers.tick(60_001);
  await peer.next(m => m.kind === 'released');
  assert.equal(peer.commands.some(m => m.method === 'Bridge.attach'), false);
  assert.equal((await f.reserve()).status, 409);
  await rejectSocket(lease.cdpUrl);
});

test('cookie replies exclude unrelated partitions and navigation commits update the scope immediately', async t => {
  const f = await fixture(t), { peer, cdp, sessionId } = await f.authorized();
  const cookie = { name: 'safe', value: '1', domain: 'example.test', path: '/', secure: true };
  peer.respond = command => command.method === 'Network.getCookies' ? { result: { cookies: [
    cookie,
    { ...cookie, name: 'other-host', domain: 'other.test' },
    { ...cookie, name: 'other-path', path: '/elsewhere' },
    { ...cookie, name: 'other-partition', partitionKey: { topLevelSite: 'https://other.test', hasCrossSiteAncestor: false } },
    { ...cookie, name: 'opaque-partition', partitionKeyOpaque: true },
  ] } } : { result: {} };
  assert.deepEqual((await cdp.call('Network.getCookies', {}, sessionId)).result.cookies.map(c => c.name), ['safe']);
  peer.emit('Page.frameNavigated', { frame: { id: 'top-frame', url: 'https://new.test/' } });
  await cdp.next(m => m.method === 'Page.frameNavigated');
  assert.ok((await cdp.call('Network.getCookies', { urls: [tab.url] }, sessionId)).error);
  assert.deepEqual((await cdp.call('Network.getCookies', {}, sessionId)).result.cookies, []);
});

test('heartbeat terminates an unresponsive client and revokes its grant', async t => {
  t.mock.timers.enable({ apis: ['setInterval'] });
  const f = await fixture(t), peer = await f.pair();
  await peer.grant();
  const lease = (await f.reserve()).body;
  const client = await f.connect(lease.cdpUrl, { autoPong: false });
  await peer.next(m => m.kind === 'cdp-command' && m.method === 'Bridge.attach');
  const ping = once(peer.ws, 'ping');
  const closed = once(client.ws, 'close');
  t.mock.timers.tick(15_000);
  await ping;
  await new Promise(resolve => setImmediate(resolve));
  await new Promise(resolve => setImmediate(resolve));
  t.mock.timers.tick(15_000);
  await closed;
  await peer.next(m => m.kind === 'released');
  assert.equal((await f.reserve()).status, 409);
  await rejectSocket(lease.cdpUrl);
});
