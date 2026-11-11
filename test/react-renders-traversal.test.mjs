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

function component(name = 'Row') {
  return { tag: 0, type: { name }, flags: 1, alternate: null,
    memoizedProps: {}, memoizedState: null, child: null, sibling: null };
}

test('wide lists do not silently truncate after the call-stack limit', () => {
  const recording = recorder();
  let first = null;
  for (let i = 0; i < 30000; i++) {
    const fiber = component();
    fiber.sibling = first;
    first = fiber;
  }
  recording.commit(first);
  const [result] = recording.stop().components;
  assert.equal(result.count, 30000);
  assert.equal(result.mounts, 30000);
});

test('deep host trees still reach nested components and following siblings', () => {
  const recording = recorder();
  let root = component('Leaf');
  for (let i = 0; i < 30000; i++) {
    root = { tag: 5, type: 'div', child: root, sibling: null };
  }
  root.sibling = component('Following');
  recording.commit(root);
  assert.deepEqual(recording.stop().components.map(({ name }) => name), ['Leaf', 'Following']);
});

test('iterative traversal preserves parent, child, and sibling order', () => {
  const recording = recorder();
  const root = component('Parent');
  root.child = component('Child');
  root.child.child = component('Grandchild');
  root.child.sibling = component('ChildSibling');
  root.sibling = component('Sibling');
  recording.commit(root);
  assert.deepEqual(recording.stop().components.map(({ name }) => name),
    ['Parent', 'Child', 'Grandchild', 'ChildSibling', 'Sibling']);
});
