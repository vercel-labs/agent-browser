import { copyFile, mkdir, readFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parse } from '@puppeteer/replay';

import { createRequire } from 'node:module';

const fixtureRoot = new URL('../cli/src/native/codegen/test-fixtures/', import.meta.url);
const playwrightCli = createRequire(import.meta.url).resolve('@playwright/test/cli');

test('the shared Recorder fixture passes the supported parser', async () => {
  const source = await readFile(new URL('flow.json', fixtureRoot), 'utf8');
  const flow = parse(JSON.parse(source));
  assert.equal(flow.title, 'hostile "flow"\nname');
  assert.equal(flow.steps.length, 6);
  // The accessible-name selector is the primary capture path, and the name
  // here carries quotes that the runner has to escape.
  const hover = flow.steps.find(step => step.type === 'hover');
  assert.deepEqual(hover.selectors[0], ['aria/Save "now"']);
});

test('the shared Playwright fixture compiles and collects', async () => {
  // The spec imports `@playwright/test`, which resolves from the directory the
  // file sits in. Run the fixture inside this package so it finds it.
  const workspace = new URL('generated/', import.meta.url);
  await mkdir(workspace, { recursive: true });
  const fixture = new URL('flow.spec.ts', workspace);
  await copyFile(new URL('flow.spec.ts', fixtureRoot), fixture);
  const child = spawn(
    process.execPath,
    [playwrightCli, 'test', '--list', 'flow.spec.ts'],
    { cwd: new URL('.', import.meta.url), stdio: ['ignore', 'pipe', 'pipe'] },
  );
  let output = '';
  child.stdout.on('data', chunk => { output += chunk; });
  child.stderr.on('data', chunk => { output += chunk; });
  const exitCode = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', resolve);
  });
  assert.equal(exitCode, 0, output);
  assert.match(output, /hostile/);
});
