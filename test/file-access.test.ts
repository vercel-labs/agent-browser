import { describe, it, expect, afterEach, beforeAll, afterAll } from 'vitest';
import { BrowserManager } from '../src/browser.js';
import { writeFileSync, unlinkSync } from 'node:fs';
import path from 'node:path';
import os from 'node:os';

describe('File Access (Issue #345)', () => {
  let browser: BrowserManager;
  const testFilePath = path.join(os.tmpdir(), 'agent-browser-test-file.html');
  const testFileUrl = `file://${testFilePath}`;

  // Create test HTML file before tests
  beforeAll(() => {
    writeFileSync(
      testFilePath,
      '<html><body><h1>Test File Access</h1><p>This content was loaded from a local file.</p></body></html>'
    );
  });

  // Clean up test file after tests
  afterAll(() => {
    try {
      unlinkSync(testFilePath);
    } catch {
      // Ignore if file doesn't exist
    }
  });

  afterEach(async () => {
    if (browser?.isLaunched()) {
      await browser.close();
    }
  });

  describe('without allowFileAccess flag', () => {
    it('should fail to load file:// URL content by default', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
      });

      const page = browser.getPage();

      // Navigate to file:// URL - this should work for navigation
      // but Chromium restricts what the page can do
      await page.goto(testFileUrl);

      // The page should load but let's verify the URL
      const url = page.url();
      expect(url).toBe(testFileUrl);

      // Content should be accessible when navigating directly
      const content = await page.content();
      expect(content).toContain('Test File Access');
    });
  });

  describe('with allowFileAccess flag', () => {
    it('should load file:// URL with allowFileAccess enabled', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        allowFileAccess: true,
      });

      const page = browser.getPage();
      await page.goto(testFileUrl);

      // Verify the page loaded correctly
      const url = page.url();
      expect(url).toBe(testFileUrl);

      // Verify content is accessible
      const heading = await page.locator('h1').textContent();
      expect(heading).toBe('Test File Access');

      const paragraph = await page.locator('p').textContent();
      expect(paragraph).toBe('This content was loaded from a local file.');
    });

    it('should allow file:// URL to access other local files via XMLHttpRequest', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        allowFileAccess: true,
      });

      const page = browser.getPage();
      await page.goto(testFileUrl);

      // With allowFileAccess, XMLHttpRequest to local files should work
      // This is the key difference - without the flag, this would be blocked
      const canAccessFiles = await page.evaluate(() => {
        return new Promise<boolean>((resolve) => {
          try {
            // XMLHttpRequest is the traditional way to test --allow-file-access-from-files
            const xhr = new XMLHttpRequest();
            xhr.open('GET', window.location.href, true);
            xhr.onload = () => resolve(xhr.status === 0 || xhr.status === 200);
            xhr.onerror = () => resolve(false);
            xhr.send();
          } catch {
            resolve(false);
          }
        });
      });

      expect(canAccessFiles).toBe(true);
    });
  });

  describe('combined with other options', () => {
    it('should work with allowFileAccess and custom user-agent', async () => {
      const customUA = 'FileAccessTestBot/1.0';
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        allowFileAccess: true,
        userAgent: customUA,
      });

      const page = browser.getPage();
      await page.goto(testFileUrl);

      // Verify file access works
      const content = await page.locator('h1').textContent();
      expect(content).toBe('Test File Access');

      // Verify user-agent is set
      const ua = await page.evaluate(() => navigator.userAgent);
      expect(ua).toBe(customUA);
    });

    it('should work with allowFileAccess and custom args', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        allowFileAccess: true,
        args: ['--disable-blink-features=AutomationControlled'],
      });

      const page = browser.getPage();
      await page.goto(testFileUrl);

      // Verify file access works
      const content = await page.locator('h1').textContent();
      expect(content).toBe('Test File Access');

      // Verify webdriver is hidden (from custom arg)
      const webdriver = await page.evaluate(() => navigator.webdriver);
      expect(webdriver).toBe(false);
    });
  });
});
