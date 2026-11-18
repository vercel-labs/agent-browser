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

function component(alternate = null) {
  return { tag: 0, type: function Counter() {}, flags: 1, alternate,
    memoizedProps: {}, memoizedState: null, child: null, sibling: null };
}

test('alternating fibers identify one component instance across updates', () => {
  const recording = recorder();
  const first = component();
  recording.commit(first);
  const alternate = component(first);
  first.alternate = alternate;
  recording.commit(alternate);
  recording.commit(first);
  const [result] = recording.stop().components;
  assert.equal(result.count, 3);
  assert.equal(result.mounts, 1);
  assert.equal(result.reRenders, 2);
  assert.equal(result.instanceCount, 1);
});

test('a recording that starts after mount counts an alternate pair once', () => {
  const recording = recorder();
  const first = component();
  const alternate = component(first);
  first.alternate = alternate;
  recording.commit(alternate);
  recording.commit(first);
  assert.equal(recording.stop().components[0].instanceCount, 1);
});

test('distinct mounts and remounts remain distinct instances with the same name', () => {
  const recording = recorder();
  const first = component();
  const second = component();
  first.sibling = second;
  recording.commit(first);
  recording.commit(component());
  const [result] = recording.stop().components;
  assert.equal(result.count, 3);
  assert.equal(result.mounts, 3);
  assert.equal(result.instanceCount, 3);
});
