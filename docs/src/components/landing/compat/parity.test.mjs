import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { accessSync, constants } from 'node:fs';
import { delimiter, join } from 'node:path';
import { test } from 'node:test';

const url = process.env.DOCS_TEST_URL;
assert.ok(url, 'Run through pnpm --filter docs test:routes, or set DOCS_TEST_URL.');
let session = `landing-parity-${randomUUID()}`;
const env = { ...process.env };
for (const key of ['AGENT_BROWSER_CDP', 'AGENT_BROWSER_AUTO_CONNECT', 'AGENT_BROWSER_PROVIDER', 'AGENT_BROWSER_PROFILE', 'AGENT_BROWSER_SESSION_NAME']) delete env[key];
// Prefer an explicitly selected or already installed CLI. CI selects a pinned release.
const installed = (process.env.PATH || '').split(delimiter).map(directory => join(directory, 'agent-browser')).find(path => {
  try { accessSync(path, constants.X_OK); return true; } catch { return false; }
});
const executable = process.env.DOCS_BROWSER_CLI || installed || execFileSync('pnpm', [
  'dlx', '--package=agent-browser@0.39.4', '--allow-build=agent-browser',
  '--reporter=silent', '-c', 'command -v agent-browser',
], { env, encoding: 'utf8', timeout: 60_000 }).trim().split('\n').at(-1);

function browser(...args) {
  const output = execFileSync(executable, ['--session', session, '--json', ...args], {
    env, encoding: 'utf8', timeout: 60_000,
  });
  const result = JSON.parse(output.trim().split('\n').at(-1));
  assert.equal(result.success, true, result.error);
  return result.data;
}

const metrics = `(() => {
  const style = element => {
    const css = getComputedStyle(element);
    return { size: css.fontSize, line: css.lineHeight, weight: css.fontWeight, spacing: css.letterSpacing };
  };
  const headings = [...document.querySelectorAll('main .landing-heading')];
  const actions = [...document.querySelectorAll('main .landing-action')];
  const tweetHeading = document.querySelector('[data-element="AgentTweetWall"] h2');
  const grid = tweetHeading.parentElement;
  const columns = getComputedStyle(grid).gridTemplateColumns.split(' ');
  const tabs = document.querySelector('[aria-label="Choose an agent tool"]');
  const tabBox = tabs.getBoundingClientRect();
  const switcherBox = tabs.parentElement.getBoundingClientRect();
  return {
    headings: headings.map(style),
    actions: actions.map(element => ({
      ...style(element), radius: getComputedStyle(element).borderRadius,
      height: element.getBoundingClientRect().height,
      padding: getComputedStyle(element).paddingInline,
    })),
    gridColumns: columns.length,
    switcher: {
      height: switcherBox.height,
      topPadding: tabBox.top - switcherBox.top,
      bottomPadding: switcherBox.bottom - tabBox.bottom,
    },
    tweetSpan: getComputedStyle(tweetHeading).gridColumnEnd,
    overflow: document.body.scrollWidth > innerWidth,
    background: getComputedStyle(document.body).backgroundColor,
    brands: [...new Set([...document.querySelectorAll('main span')]
      .map(element => getComputedStyle(element).maskImage)
      .filter(mask => mask.startsWith('url(')))],
  };
})()`;

test('homepage layout and interactions stay consistent across viewports and themes', { timeout: 180_000 }, async () => {
  try {
    browser('open', url);
    browser('eval', 'document.fonts.ready.then(() => true)');
    const backgrounds = [];
    for (const theme of ['light', 'dark']) {
      browser('set', 'media', theme);
      const ready = browser('eval', `new Promise(resolve => {
        const deadline = Date.now() + 10000;
        function check() {
          const expected = ${JSON.stringify(theme === 'dark')};
          if (document.documentElement.classList.contains('dark-theme') === expected &&
              document.documentElement.classList.contains('light-theme') === !expected) resolve(true);
          else if (Date.now() > deadline) resolve(false);
          else setTimeout(check, 25);
        }
        check();
      })`).result;
      assert.equal(ready, true, `${theme}: theme provider did not settle`);
      for (const width of [390, 1280]) {
        browser('set', 'viewport', String(width), '900');
        const actual = browser('eval', metrics).result;
        const expected = width === 390
          ? [{ size: '32px', line: '40px', weight: '450', spacing: '-1.28px' }]
          : [
              { size: '56px', line: '56px', weight: '450', spacing: '-3.36px' },
              { size: '64px', line: '64px', weight: '450', spacing: '-3.84px' },
            ];
        assert.equal(actual.headings.length, 3);
        actual.headings.forEach((heading, index) =>
          assert.deepEqual(heading, expected[width === 1280 && index === 2 ? 1 : 0], `${theme} ${width}: heading ${index}`));
        assert.equal(actual.actions.length, 3);
        actual.actions.forEach((action, index) => {
          assert.equal(action.radius, '9999px', `${theme} ${width}: pill radius`);
          assert.equal(action.size, '16px');
          assert.equal(action.line, '24px');
          assert.equal(action.weight, '500');
          assert.equal(action.height, 40);
          assert.equal(action.padding, index === 2 ? '12px' : '4px 8px');
        });
        assert.equal(actual.switcher.height, actual.actions[0].height, `${theme} ${width}: switcher matches the hero button height`);
        assert.equal(actual.switcher.topPadding, actual.switcher.bottomPadding, `${theme} ${width}: tabs are vertically centered`);
        assert.equal(actual.gridColumns, 12);
        assert.equal(actual.tweetSpan, width === 390 ? '-1' : '8');
        assert.equal(actual.overflow, false);
        assert.ok(actual.brands.length >= 3);
        for (const mask of actual.brands) {
          const asset = /url\(["']?(.*?)["']?\)/.exec(mask)?.[1];
          assert.ok(asset);
          const response = await fetch(asset);
          assert.equal(response.status, 200, asset);
          assert.match(await response.text(), /<svg/);
        }
        if (width === 1280) backgrounds.push(actual.background);
      }
      browser('hover', 'main [aria-label="Tools for agents"] .landing-action');
      const hover = browser('eval', `new Promise(resolve => {
        const deadline = Date.now() + 2000;
        function check() {
          const background = getComputedStyle(document.querySelector('main .landing-action')).backgroundColor;
          if (background === ${JSON.stringify(theme === 'dark' ? 'rgb(204, 204, 204)' : 'rgb(56, 56, 56)')} || Date.now() > deadline) resolve(background);
          else setTimeout(check, 25);
        }
        check();
      })`).result;
      assert.equal(hover, theme === 'dark' ? 'rgb(204, 204, 204)' : 'rgb(56, 56, 56)');
      browser('hover', 'main h1');
    }
    assert.notEqual(backgrounds[0], backgrounds[1]);
    browser('find', 'role', 'tab', 'click', '--name', 'Desktop');
    const state = browser('eval', `({ heading: document.querySelector('main h1').textContent, selected: document.querySelector('[role="tab"][aria-selected="true"]').textContent })`).result;
    assert.equal(state.selected, 'Desktop');
    assert.equal(state.heading, 'Desktop automation built for agents');
    assert.equal(browser('eval', 'document.querySelectorAll("main .ss-stage").length').result, 4);
    const animated = browser('eval', `new Promise(resolve => {
      const world = document.querySelector('main .ab-world');
      const initial = world.dataset.time;
      const deadline = Date.now() + 5000;
      function check() {
        if (world.dataset.time !== initial) resolve(true);
        else if (Date.now() > deadline) resolve(false);
        else setTimeout(check, 25);
      }
      check();
    })`).result;
    assert.equal(animated, true, 'hero animation must keep advancing');
  } finally {
    browser('close');
  }
});

test('use-case scenes cycle from their stills in sequence, return to them, and replay on hover', { timeout: 90_000 }, async () => {
  session = `landing-scenes-${randomUUID()}`;
  try {
    browser('open', url);
    browser('set', 'viewport', '1280', '900');
    browser('eval', 'document.fonts.ready.then(() => true)');
    browser('eval', `(() => {
      window.__sceneOrder = [];
      const root = document.querySelector('[data-element="AgentUseCases"]');
      window.__readPosters = () => [...root.querySelectorAll('.ab-feature-scene')].map(scene => {
        const marquee = scene.querySelector('.ab-shot-marquee');
        return {
          kind: scene.dataset.kind,
          fields: [...scene.querySelectorAll('.ss-field__chars')].map(field => field.textContent),
          focused: [...scene.querySelectorAll('.ss-field')].map(field => field.hasAttribute('data-focused')),
          signedIn: !!scene.querySelector('.ab-sign-in-done[data-on]'),
          lines: [...scene.querySelectorAll('.ab-perf-line[data-on]')].map(line => line.textContent),
          scroll: scene.querySelector('.ab-perf-lines')?.style.transform,
          marquee: marquee ? { path: marquee.querySelector('path').getAttribute('d'), left: marquee.style.left, top: marquee.style.top } : null,
          captures: scene.querySelectorAll('.ab-shot-copy[data-carry="held"]').length,
        };
      });
      window.__scenePosters = window.__readPosters();
      window.__resumedPosters = [];
      new MutationObserver(records => {
        for (const record of records) {
          const kind = record.target.closest('.ab-feature-scene')?.dataset.kind;
          const mark = record.target.getAttribute('data-mark');
          if (kind && mark) window.__sceneOrder.push({ kind, mark });
          if (mark === 'resume') window.__resumedPosters.push(window.__readPosters().find(scene => scene.kind === kind));
        }
      }).observe(root, { subtree: true, attributes: true, attributeFilter: ['data-mark'] });
      root.scrollIntoView({ block: 'start' });
    })()`);
    let order = [];
    const deadline = Date.now() + 45000;
    while (Date.now() < deadline) {
      order = browser('eval', 'window.__sceneOrder').result;
      if (order.some(event => event.kind === 'verify' && event.mark === 'done')) break;
      await new Promise(resolve => setTimeout(resolve, 500));
    }
    const kinds = order.map(event => event.kind).filter((kind, index, all) => index === 0 || kind !== all[index - 1]);
    assert.deepEqual(kinds, ['sign-in', 'performance', 'verify']);
    assert.equal(order.at(-1)?.mark, 'done');
    const frames = browser('eval', '({ initial: window.__scenePosters, resumed: window.__resumedPosters, final: window.__readPosters() })').result;
    assert.deepEqual(frames.resumed, frames.initial, 'each scene must resume its existing still');
    assert.deepEqual(frames.final, frames.initial, 'each full cycle must freeze at its original still');
    const held = browser('eval', `new Promise(resolve => {
      const before = JSON.stringify(window.__sceneOrder);
      setTimeout(() => resolve(before === JSON.stringify(window.__sceneOrder)), 1000);
    })`).result;
    assert.equal(held, true, 'completed scenes must not loop');

    browser('hover', '.ab-feature-scene[data-kind="sign-in"]');
    const replayed = browser('eval', `new Promise(resolve => {
      const deadline = Date.now() + 3000;
      function check() {
        const count = window.__sceneOrder.filter(event => event.kind === 'sign-in' && event.mark === 'resume').length;
        if (count > 1 || Date.now() > deadline) resolve(count);
        else setTimeout(check, 25);
      }
      check();
    })`).result;
    assert.ok(replayed > 1, 'hover should replay the completed illustration');

    browser('set', 'media', 'light', 'reduced-motion');
    const still = browser('eval', `new Promise(resolve => {
      const deadline = Date.now() + 3000;
      function check() {
        const stages = [...document.querySelectorAll('.ab-feature-scene .ss-stage')];
        const ready = stages.length === 3 && stages.every(stage => stage.hasAttribute('data-still'));
        if (ready || Date.now() > deadline) resolve(ready);
        else setTimeout(check, 25);
      }
      check();
    })`).result;
    assert.equal(still, true, 'reduced motion must retain static illustrations');
  } finally {
    browser('close');
  }
});
