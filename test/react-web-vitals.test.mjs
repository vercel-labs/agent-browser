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

function vitals(navigation, interactionCount) {
  const observers = new Map();
  const performance = { getEntriesByType: () => navigation ? [navigation] : [] };
  if (interactionCount !== undefined) performance.interactionCount = interactionCount;
  const context = vm.createContext({
    window: {}, performance, console: { timeStamp() {} },
    PerformanceObserver: class {
      constructor(callback) { this.callback = callback; }
      observe(options) { observers.set(options.type, { callback: this.callback, options }); }
    },
  });
  vm.runInContext(script('VITALS_INIT'), context);
  return {
    performance, observers,
    emit(type, entries) {
      const observer = observers.get(type);
      assert.ok(observer, `Missing observer for ${type}`);
      observer.callback({ getEntries: () => entries });
    },
    read: () => JSON.parse(vm.runInContext(script('VITALS_READ'), context)),
    install: () => vm.runInContext(script('VITALS_INIT'), context),
  };
}
const shift = (startTime, value, hadRecentInput = false) => ({ startTime, value, hadRecentInput });
const event = (interactionId, duration) => ({ interactionId, duration });
const closeTo = (actual, expected) => assert.ok(Math.abs(actual - expected) < 0.000001, `${actual} != ${expected}`);

test('CLS reports the largest session window, excluding recent input', () => {
  const page = vitals();
  page.emit('layout-shift', [shift(100, 0.1), shift(900, 0.1), shift(2000, 0.3)]);
  page.emit('layout-shift', [shift(2200, 5, true), shift(2500, 0.05)]);
  closeTo(page.read().cwv.cls, 0.35);
  assert.equal(page.read().cwv.clsEntries.length, 4);
});

test('CLS starts a new window at a one-second gap or a five-second span', () => {
  const gap = vitals();
  gap.emit('layout-shift', [shift(0, 0.2), shift(1000, 0.3)]);
  closeTo(gap.read().cwv.cls, 0.3);
  const span = vitals();
  span.emit('layout-shift', [0, 900, 1800, 2700, 3600, 4500, 5000].map((time) => shift(time, 0.1)));
  closeTo(span.read().cwv.cls, 0.6);
});

test('INP ignores event entries that are not interactions', () => {
  const page = vitals();
  page.emit('event', [event(0, 1000)]);
  assert.equal(page.read().cwv.inp, null);
  page.emit('event', [event(7, 120), event(7, 240), event(7, 100)]);
  assert.equal(page.read().cwv.inp, 240);
});

test('INP groups events by interaction and excludes one outlier per 50 interactions', () => {
  const page = vitals(undefined, 49);
  page.emit('event', [event(7, 240), event(7, 232), event(14, 160)]);
  assert.equal(page.read().cwv.inp, 240);
  page.performance.interactionCount = 50;
  page.emit('event', [event(21, 80)]);
  assert.equal(page.read().cwv.inp, 160);
});

test('INP estimates interaction count when the native count is unavailable', () => {
  const page = vitals();
  page.emit('event', Array.from({ length: 49 }, (_, index) =>
    event(100 + index * 7, index === 0 ? 240 : index === 1 ? 160 : 24)));
  assert.equal(page.read().cwv.inp, 240);
  page.emit('event', [event(100 + 49 * 7, 24)]);
  assert.equal(page.read().cwv.inp, 160);
});

test('INP includes a fast first input below the event timing threshold', () => {
  const page = vitals(undefined, 1);
  page.emit('first-input', [event(7, 8)]);
  assert.equal(page.read().cwv.inp, 8);
});

test('TTFB includes connection setup and accounts for prerender activation', () => {
  assert.equal(vitals({ requestStart: 100, responseStart: 250 }).read().ttfb, 250);
  assert.equal(vitals({ requestStart: 100, responseStart: 250, activationStart: 200 }).read().ttfb, 50);
  assert.equal(vitals({ requestStart: 100, responseStart: 250, activationStart: 300 }).read().ttfb, 0);
});

test('missing navigation and untouched paint metrics keep their output shape', () => {
  const page = vitals();
  assert.equal(page.read().ttfb, null);
  page.emit('paint', [{ name: 'first-contentful-paint', startTime: 100.126 }]);
  page.emit('largest-contentful-paint', [{ startTime: 250.126, size: 42, element: { tagName: 'IMG' } }]);
  assert.equal(page.read().cwv.fcp, 100.13);
  assert.deepEqual(page.read().cwv.lcp, { startTime: 250.13, size: 42, element: 'img', url: null });
  const count = page.observers.size;
  page.install();
  assert.equal(page.observers.size, count);
});
