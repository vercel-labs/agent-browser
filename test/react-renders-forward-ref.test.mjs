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

function profile(type, tag = 11, extra = {}) {
  const recording = recorder();
  recording.commit({ tag, type, flags: 1, alternate: null,
    memoizedProps: {}, memoizedState: null, child: null, sibling: null, ...extra });
  return recording.stop().components;
}

function forwardRef(render) {
  return { $$typeof: Symbol.for('react.forward_ref'), render };
}

test('named forwardRef render functions appear in profiles', () => {
  const [result] = profile(forwardRef(function Input() {}));
  assert.ok(result, 'forwardRef component is missing');
  assert.equal(result.name, 'Input');
  assert.equal(result.count, 1);
  assert.equal(result.mounts, 1);
});

test('explicit wrapper and render display names retain their priority', () => {
  const render = function Input() {};
  render.displayName = 'InputRender';
  const wrapper = forwardRef(render);
  assert.equal(profile(wrapper)[0]?.name, 'InputRender');
  wrapper.displayName = 'AccessibleInput';
  assert.equal(profile(wrapper)[0]?.name, 'AccessibleInput');
});

test('forwardRef parents are identified in unchanged-child render reasons', () => {
  const props = {};
  const [result] = profile(function Child() {}, 0, {
    memoizedProps: props,
    alternate: { memoizedProps: props, memoizedState: null },
    return: { tag: 11, type: forwardRef(function Input() {}), alternate: {} },
  });
  assert.deepEqual(result.changes, [{ type: 'parent', name: 'Input' }]);
});

test('ordinary component names and unnamed wrappers keep their behavior', () => {
  assert.equal(profile(function Counter() {}, 0)[0].name, 'Counter');
  const unnamed = function () {};
  Object.defineProperty(unnamed, 'name', { value: '' });
  assert.deepEqual(profile(forwardRef(unnamed)), []);
});
