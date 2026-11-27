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

function recorder() {
  const context = vm.createContext({
    window: { __REACT_DEVTOOLS_GLOBAL_HOOK__: { onCommitFiberRoot() {} } },
    performance: { now: () => 0 },
    requestAnimationFrame: () => 1,
    cancelAnimationFrame() {},
  });
  vm.runInContext(script('RENDERS_INIT'), context);
  return {
    commit(fiber) {
      context.window.__REACT_DEVTOOLS_GLOBAL_HOOK__.onCommitFiberRoot(1, { current: fiber });
    },
    stop: () => JSON.parse(vm.runInContext(script('RENDERS_STOP'), context)),
  };
}

function update(tag, previousState, currentState) {
  const recording = recorder();
  const props = {};
  const alternate = { memoizedState: previousState, memoizedProps: props };
  recording.commit({ tag, type: function Counter() {}, flags: 1, alternate,
    memoizedProps: props, memoizedState: currentState, child: null, sibling: null });
  return recording.stop().components[0].changes;
}

test('class state changes report their fields instead of a parent render', () => {
  assert.deepEqual(update(1, { count: 1, removed: true }, { count: 2, added: 5 }), [
    { type: 'state', name: 'count', prev: '1', next: '2' },
    { type: 'state', name: 'added', prev: 'undefined', next: '5' },
    { type: 'state', name: 'removed', prev: 'true', next: 'undefined' },
  ]);
});

test('class state keys named next or memoizedState are ordinary fields', () => {
  assert.deepEqual(update(1, { next: 1, memoizedState: 'a' }, { next: 2, memoizedState: 'b' }), [
    { type: 'state', name: 'next', prev: '1', next: '2' },
    { type: 'state', name: 'memoizedState', prev: '"a"', next: '"b"' },
  ]);
});

test('null state and added undefined fields retain class field names', () => {
  assert.deepEqual(update(1, null, { ready: true, optional: undefined }), [
    { type: 'state', name: 'ready', prev: 'undefined', next: 'true' },
    { type: 'state', name: 'optional', prev: 'undefined', next: 'undefined' },
  ]);
});

test('unchanged class fields do not produce a false state-change reason', () => {
  assert.deepEqual(update(1, { count: 1, value: NaN }, { count: 1, value: NaN }),
    [{ type: 'parent', name: 'unknown' }]);
});

test('function component hook changes keep their hook index', () => {
  const previous = { memoizedState: 1, next: { memoizedState: 'a', next: null } };
  const current = { memoizedState: 1, next: { memoizedState: 'b', next: null } };
  assert.deepEqual(update(0, previous, current), [
    { type: 'state', name: 'hook #1', prev: '"a"', next: '"b"' },
  ]);
});
