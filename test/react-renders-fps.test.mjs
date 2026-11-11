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
  let frame;
  let canceled = false;
  const originalCommit = () => {};
  const context = vm.createContext({
    window: { __REACT_DEVTOOLS_GLOBAL_HOOK__: { onCommitFiberRoot: originalCommit } },
    performance: { now: () => 200000 },
    requestAnimationFrame: (callback) => { frame = callback; return 1; },
    cancelAnimationFrame: () => { canceled = true; },
  });
  vm.runInContext(script('RENDERS_INIT'), context);
  return {
    frame: (time) => frame(time),
    stop() {
      const result = JSON.parse(vm.runInContext(script('RENDERS_STOP'), context));
      assert.equal(canceled, true);
      assert.equal(context.window.__REACT_DEVTOOLS_GLOBAL_HOOK__.onCommitFiberRoot, originalCommit);
      assert.equal(context.window.__AB_RENDERS_ACTIVE__, undefined);
      return result;
    },
  };
}

test('long recordings stop without spreading every frame into a function call', () => {
  const recording = recorder();
  for (let i = 1; i <= 200001; i++) recording.frame(i * 20);
  assert.deepEqual(recording.stop().fps, { avg: 50, min: 50, max: 50, drops: 0 });
});

test('FPS statistics preserve mixed frame intervals and empty recordings', () => {
  const recording = recorder();
  for (const now of [10, 30, 70, 80]) recording.frame(now);
  assert.deepEqual(recording.stop().fps, { avg: 58, min: 25, max: 100, drops: 1 });
  assert.deepEqual(recorder().stop().fps, { avg: 0, min: 0, max: 0, drops: 0 });
});
