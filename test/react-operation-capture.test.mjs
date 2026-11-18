import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../cli/src/native/react/scripts.rs', import.meta.url), 'utf8');
function script(name) {
  const match = source.match(new RegExp('pub const ' + name + ': &str = r#"([\\s\\S]*?)"#;'));
  assert.ok(match, `Missing production script ${name}`);
  return match[1];
}

function fixture(flush) {
  const listeners = new Set();
  const timers = [];
  const hook = {
    emit(event, payload) {
      if (event === 'operations') for (const listener of [...listeners]) listener(payload);
    },
    sub(event, callback) {
      assert.equal(event, 'operations');
      listeners.add(callback);
      return () => listeners.delete(callback);
    },
    rendererInterfaces: new Map(),
    renderers: new Map([[1, { rendererPackageName: 'react-dom' }], [2, { rendererPackageName: 'react-dom' }]]),
    getFiberRoots: () => new Set([{}]),
  };
  hook.rendererInterfaces.set(1, {
    flushInitialOperations: () => flush(hook),
    hasElementWithId: () => false,
  });
  hook.rendererInterfaces.set(2, {});
  const originalEmit = hook.emit;
  const context = vm.createContext({
    window: { __REACT_DEVTOOLS_GLOBAL_HOOK__: hook },
    setTimeout: (callback) => { timers.push(callback); return timers.length; },
  });
  return {
    hook, originalEmit, listeners,
    finish: () => { assert.ok(timers.length); timers.shift()(); },
    read: (name) => vm.runInContext(script(name).replace('{{PICK_RI}}', script('PICK_REACT_RENDERER')), context),
  };
}

function operations(scriptName, renderer, id, name) {
  const chars = Array.from(name, (char) => char.codePointAt(0));
  const header = [renderer, 1, chars.length + 1, chars.length, ...chars];
  return scriptName === 'TREE_SNAPSHOT'
    ? [...header, 1, id, 5, 1, 0, 1, 0, 0]
    : [...header, 8, id, 1, 1, 1, -1];
}

for (const name of ['TREE_SNAPSHOT', 'SUSPENSE_WALK']) {
  test(`${name} releases capture when the renderer flush throws`, async () => {
    const page = fixture(() => { throw new Error('renderer failed'); });
    await assert.rejects(page.read(name), /renderer failed/);
    assert.equal(page.hook.emit, page.originalEmit);
    assert.equal(page.listeners.size, 0);
  });

  test(`${name} only captures the selected renderer`, async () => {
    const page = fixture((hook) => {
      hook.emit('operations', operations(name, 2, 202, 'Other'));
      hook.emit('operations', operations(name, 1, 101, 'Selected'));
    });
    const pending = page.read(name);
    page.finish();
    const result = JSON.parse(await pending);
    assert.deepEqual(result.map(({ id, name }) => ({ id, name })), [{ id: 101, name: 'Selected' }]);
    assert.equal(page.hook.emit, page.originalEmit);
    assert.equal(page.listeners.size, 0);
  });

  test(`${name} overlapping reads do not remove another reader's capture`, async () => {
    const page = fixture(() => {});
    const first = page.read(name);
    const second = page.read(name);
    page.finish();
    assert.deepEqual(JSON.parse(await first), []);
    page.hook.emit('operations', operations(name, 1, 101, 'Later'));
    page.finish();
    const result = JSON.parse(await second);
    assert.deepEqual(result.map(({ id, name }) => ({ id, name })), [{ id: 101, name: 'Later' }]);
    assert.equal(page.hook.emit, page.originalEmit);
    assert.equal(page.listeners.size, 0);
  });

  test(`${name} preserves existing operation subscribers`, async () => {
    const payload = operations(name, 1, 101, 'Selected');
    const page = fixture((hook) => hook.emit('operations', payload));
    const delivered = [];
    page.listeners.add((event) => delivered.push(event));
    const pending = page.read(name);
    page.finish();
    assert.equal(JSON.parse(await pending)[0].name, 'Selected');
    assert.deepEqual(delivered, [payload]);
    assert.equal(page.listeners.size, 1);
    assert.equal(page.hook.emit, page.originalEmit);
  });
}
