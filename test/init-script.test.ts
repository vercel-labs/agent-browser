import { describe, it, expect, afterEach, beforeEach } from 'vitest';
import { BrowserManager } from '../src/browser.js';
import { writeFileSync, unlinkSync, mkdtempSync } from 'fs';
import path from 'path';
import os from 'os';

describe('Init Script Injection', () => {
  let browser: BrowserManager;
  let tmpDir: string;
  const originalEnv = process.env.AGENT_BROWSER_INIT_SCRIPT;

  beforeEach(() => {
    tmpDir = mkdtempSync(path.join(os.tmpdir(), 'ab-init-test-'));
  });

  afterEach(async () => {
    if (originalEnv !== undefined) {
      process.env.AGENT_BROWSER_INIT_SCRIPT = originalEnv;
    } else {
      delete process.env.AGENT_BROWSER_INIT_SCRIPT;
    }
    if (browser?.isLaunched()) {
      await browser.close();
    }
  });

  it('should inject inline init script into new pages', async () => {
    process.env.AGENT_BROWSER_INIT_SCRIPT = 'window.__AB_INIT_TEST__ = "injected";';

    browser = new BrowserManager();
    await browser.launch({ headless: true });

    const page = browser.getPage();
    await page.goto('about:blank');

    const value = await page.evaluate(() => (window as any).__AB_INIT_TEST__);
    expect(value).toBe('injected');
  });

  it('should inject init script from a file path', async () => {
    const scriptPath = path.join(tmpDir, 'init.js');
    writeFileSync(scriptPath, 'window.__AB_FILE_INIT__ = "from-file";');
    process.env.AGENT_BROWSER_INIT_SCRIPT = scriptPath;

    browser = new BrowserManager();
    await browser.launch({ headless: true });

    const page = browser.getPage();
    await page.goto('about:blank');

    const value = await page.evaluate(() => (window as any).__AB_FILE_INIT__);
    expect(value).toBe('from-file');

    unlinkSync(scriptPath);
  });

  it('should not inject script when env var is not set', async () => {
    delete process.env.AGENT_BROWSER_INIT_SCRIPT;

    browser = new BrowserManager();
    await browser.launch({ headless: true });

    const page = browser.getPage();
    await page.goto('about:blank');

    const value = await page.evaluate(() => (window as any).__AB_INIT_TEST__);
    expect(value).toBeUndefined();
  });
});
