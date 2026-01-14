/**
 * Integration test for @sparticuz/chromium compatibility
 * This tests the executablePath option with a serverless-optimized Chromium build
 *
 * Note: @sparticuz/chromium only works on Linux (designed for AWS Lambda).
 * This test will skip on non-Linux platforms.
 */
import { describe, it, expect, afterAll } from 'vitest';
import { BrowserManager } from '../src/browser.js';
import * as os from 'os';

const isLinux = os.platform() === 'linux';

// Only run if @sparticuz/chromium is available AND we're on Linux
const canRunTest = await (async () => {
  if (!isLinux) {
    console.log('Skipping @sparticuz/chromium test: only runs on Linux');
    return false;
  }
  try {
    await import('@sparticuz/chromium');
    return true;
  } catch {
    console.log('Skipping @sparticuz/chromium test: package not installed');
    return false;
  }
})();

describe.skipIf(!canRunTest)('Serverless Chromium Integration', () => {
  let browser: BrowserManager;
  let chromiumPath: string;

  it('should get executable path from @sparticuz/chromium', async () => {
    const chromium = await import('@sparticuz/chromium');
    chromiumPath = await chromium.default.executablePath();
    expect(chromiumPath).toBeTruthy();
    expect(typeof chromiumPath).toBe('string');
    console.log('Chromium executable path:', chromiumPath);
  });

  it('should launch browser with custom executablePath', async () => {
    const chromium = await import('@sparticuz/chromium');
    chromiumPath = await chromium.default.executablePath();

    browser = new BrowserManager();
    await browser.launch({
      headless: true,
      executablePath: chromiumPath,
    });

    expect(browser.isLaunched()).toBe(true);
  });

  it('should navigate to a page', async () => {
    const page = browser.getPage();
    await page.goto('https://example.com');
    expect(page.url()).toBe('https://example.com/');
  });

  it('should get page title', async () => {
    const page = browser.getPage();
    const title = await page.title();
    expect(title).toBe('Example Domain');
  });

  it('should take snapshot with refs', async () => {
    const { tree, refs } = await browser.getSnapshot();
    expect(tree).toContain('Example Domain');
    expect(typeof refs).toBe('object');
    expect(Object.keys(refs).length).toBeGreaterThan(0);
  });

  it('should take screenshot', async () => {
    const page = browser.getPage();
    const buffer = await page.screenshot();
    expect(buffer).toBeInstanceOf(Buffer);
    expect(buffer.length).toBeGreaterThan(0);
  });

  afterAll(async () => {
    if (browser?.isLaunched()) {
      await browser.close();
    }
  });
});
