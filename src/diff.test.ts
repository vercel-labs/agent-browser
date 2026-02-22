import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { diffSnapshots, diffScreenshots } from './diff.js';
import { chromium, type Browser, type BrowserContext, type Page } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

describe('diffSnapshots', () => {
  it('should report no changes for identical inputs', () => {
    const text = 'heading "Hello"\nbutton "Submit" [ref=e1]';
    const result = diffSnapshots(text, text);
    expect(result.changed).toBe(false);
    expect(result.additions).toBe(0);
    expect(result.removals).toBe(0);
    expect(result.unchanged).toBe(2);
  });

  it('should report no changes for empty inputs', () => {
    const result = diffSnapshots('', '');
    expect(result.changed).toBe(false);
    expect(result.additions).toBe(0);
    expect(result.removals).toBe(0);
    expect(result.unchanged).toBe(1);
  });

  it('should detect a single-line addition', () => {
    const before = 'heading "Hello"';
    const after = 'heading "Hello"\nbutton "New"';
    const result = diffSnapshots(before, after);
    expect(result.changed).toBe(true);
    expect(result.additions).toBe(1);
    expect(result.removals).toBe(0);
    expect(result.unchanged).toBe(1);
    expect(result.diff).toContain('+ button "New"');
  });

  it('should detect a single-line removal', () => {
    const before = 'heading "Hello"\nbutton "Gone"';
    const after = 'heading "Hello"';
    const result = diffSnapshots(before, after);
    expect(result.changed).toBe(true);
    expect(result.additions).toBe(0);
    expect(result.removals).toBe(1);
    expect(result.unchanged).toBe(1);
    expect(result.diff).toContain('- button "Gone"');
  });

  it('should detect completely different inputs', () => {
    const before = 'line A\nline B';
    const after = 'line C\nline D';
    const result = diffSnapshots(before, after);
    expect(result.changed).toBe(true);
    expect(result.additions).toBe(2);
    expect(result.removals).toBe(2);
    expect(result.unchanged).toBe(0);
  });

  it('should handle mixed additions, removals, and unchanged lines', () => {
    const before = [
      'heading "Title"',
      'button "Submit" [ref=e2]',
      'text "old value"',
      'footer "Copyright"',
    ].join('\n');
    const after = [
      'heading "Title"',
      'button "Submit" [ref=e2] [disabled]',
      'text "new value"',
      'link "Help" [ref=e5]',
      'footer "Copyright"',
    ].join('\n');
    const result = diffSnapshots(before, after);
    expect(result.changed).toBe(true);
    expect(result.additions).toBeGreaterThan(0);
    expect(result.removals).toBeGreaterThan(0);
    expect(result.unchanged).toBeGreaterThan(0);
    expect(result.diff).toContain('+ ');
    expect(result.diff).toContain('- ');
  });

  it('should use + prefix for insertions and - prefix for deletions', () => {
    const before = 'alpha';
    const after = 'beta';
    const result = diffSnapshots(before, after);
    const lines = result.diff.split('\n');
    const deletions = lines.filter((l) => l.startsWith('- '));
    const insertions = lines.filter((l) => l.startsWith('+ '));
    expect(deletions.length).toBe(1);
    expect(insertions.length).toBe(1);
    expect(deletions[0]).toBe('- alpha');
    expect(insertions[0]).toBe('+ beta');
  });

  it('should use two-space prefix for unchanged lines', () => {
    const text = 'unchanged line';
    const result = diffSnapshots(text, text);
    expect(result.diff).toBe('  unchanged line');
  });

  it('should handle multiline to empty', () => {
    const before = 'line 1\nline 2\nline 3';
    const after = '';
    const result = diffSnapshots(before, after);
    expect(result.changed).toBe(true);
    expect(result.removals).toBeGreaterThanOrEqual(3);
  });

  it('should handle empty to multiline', () => {
    const before = '';
    const after = 'line 1\nline 2\nline 3';
    const result = diffSnapshots(before, after);
    expect(result.changed).toBe(true);
    expect(result.additions).toBeGreaterThanOrEqual(3);
  });
});

const canLaunchBrowser = await (async () => {
  try {
    const b = await chromium.launch({ headless: true });
    await b.close();
    return true;
  } catch {
    return false;
  }
})();

describe.skipIf(!canLaunchBrowser)('diffScreenshots', () => {
  let browser: Browser;
  let context: BrowserContext;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch({ headless: true });
    context = await browser.newContext({ viewport: { width: 200, height: 200 } });
    page = await context.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  async function screenshotOfColor(color: string): Promise<Buffer> {
    await page.setContent(`<div style="width:200px;height:200px;background:${color}"></div>`);
    return await page.screenshot({ type: 'png' });
  }

  it('should report match for identical images', async () => {
    const img = await screenshotOfColor('red');
    const result = await diffScreenshots(context, img, img, {});
    expect(result.match).toBe(true);
    expect(result.differentPixels).toBe(0);
    expect(result.mismatchPercentage).toBe(0);
    expect(result.dimensionMismatch).toBeUndefined();
    if (result.diffPath) fs.unlinkSync(result.diffPath);
  });

  it('should detect differences between distinct images', async () => {
    const imgA = await screenshotOfColor('red');
    const imgB = await screenshotOfColor('blue');
    const result = await diffScreenshots(context, imgA, imgB, {});
    expect(result.match).toBe(false);
    expect(result.differentPixels).toBeGreaterThan(0);
    expect(result.mismatchPercentage).toBeGreaterThan(0);
    if (result.diffPath) fs.unlinkSync(result.diffPath);
  });

  it('should detect dimension mismatch', async () => {
    const imgA = await screenshotOfColor('white');
    await page.setViewportSize({ width: 100, height: 100 });
    const imgB = await screenshotOfColor('white');
    await page.setViewportSize({ width: 200, height: 200 });
    const result = await diffScreenshots(context, imgA, imgB, {});
    expect(result.dimensionMismatch).toBe(true);
    expect(result.mismatchPercentage).toBe(100);
    if (result.diffPath) fs.unlinkSync(result.diffPath);
  });

  it('should write diff image to custom outputPath', async () => {
    const imgA = await screenshotOfColor('green');
    const imgB = await screenshotOfColor('yellow');
    const outputPath = path.join(os.tmpdir(), `diff-test-${Date.now()}.png`);
    const result = await diffScreenshots(context, imgA, imgB, { outputPath });
    expect(result.diffPath).toBe(outputPath);
    expect(fs.existsSync(outputPath)).toBe(true);
    const stat = fs.statSync(outputPath);
    expect(stat.size).toBeGreaterThan(0);
    fs.unlinkSync(outputPath);
  });
});
