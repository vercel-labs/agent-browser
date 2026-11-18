import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(
  new URL('../cli/src/native/react/scripts.rs', import.meta.url),
  'utf8',
);
const script = (name) => {
  const match = source.match(new RegExp(`pub const ${name}: &str = r#"([\\s\\S]*?)"#;`));
  assert.ok(match, `Missing ${name} script`);
  return match[1];
};

for (const name of ['constructor', 'toString', 'hasOwnProperty', '__proto__']) {
  test(`render recording preserves component name ${name} and subsequent siblings`, () => {
    let forwarded = 0;
    const hook = { onCommitFiberRoot: () => forwarded++ };
    const context = vm.createContext({
      window: { __REACT_DEVTOOLS_GLOBAL_HOOK__: hook },
      performance: { now: () => 100 },
      requestAnimationFrame: () => 1,
      cancelAnimationFrame: () => {},
    });
    const fiber = (displayName, sibling = null) => ({
      tag: 0,
      type: { displayName },
      flags: 1,
      alternate: null,
      memoizedProps: {},
      memoizedState: null,
      child: null,
      sibling,
    });

    vm.runInContext(script('RENDERS_INIT'), context);
    hook.onCommitFiberRoot(1, { current: fiber(name, fiber('Sibling')) });
    const result = JSON.parse(vm.runInContext(script('RENDERS_STOP'), context));

    assert.equal(result.totalRenders, 2);
    assert.equal(result.totalMounts, 2);
    assert.equal(result.totalComponents, 2);
    assert.deepEqual(result.components.map((component) => component.name).sort(), [name, 'Sibling'].sort());
    assert.equal(forwarded, 1);
  });
}
