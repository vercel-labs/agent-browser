import { describe, it, expect, beforeAll, afterAll, beforeEach, vi } from 'vitest';
import { BrowserManager } from './browser.js';
import { executeCommand } from './actions.js';
import { chromium } from 'playwright-core';

describe('BrowserManager', () => {
  let browser: BrowserManager;

  beforeAll(async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true });
  });

  afterAll(async () => {
    await browser.close();
  });

  describe('launch and close', () => {
    it('should report as launched', () => {
      expect(browser.isLaunched()).toBe(true);
    });

    it('should have a page', () => {
      const page = browser.getPage();
      expect(page).toBeDefined();
    });

    it('should reject invalid executablePath', async () => {
      const testBrowser = new BrowserManager();
      await expect(
        testBrowser.launch({
          headless: true,
          executablePath: '/nonexistent/path/to/chromium',
        })
      ).rejects.toThrow();
    });

    it('should be no-op when relaunching with same options', async () => {
      const browserInstance = browser.getBrowser();
      await browser.launch({ id: 'test', action: 'launch', headless: true });
      expect(browser.getBrowser()).toBe(browserInstance);
    });

    it('should reconnect when CDP port changes', async () => {
      const newBrowser = new BrowserManager();
      await newBrowser.launch({ id: 'test', action: 'launch', headless: true });
      expect(newBrowser.getBrowser()).not.toBeNull();

      await expect(
        newBrowser.launch({ id: 'test', action: 'launch', cdpPort: 59999 })
      ).rejects.toThrow();

      expect(newBrowser.getBrowser()).toBeNull();
      await newBrowser.close();
    });
  });

  describe('stale session recovery (all pages closed)', () => {
    it('should recover when all pages are closed externally', async () => {
      const testBrowser = new BrowserManager();
      await testBrowser.launch({ headless: true });

      // Verify initial state
      expect(testBrowser.isLaunched()).toBe(true);
      expect(testBrowser.getPage()).toBeDefined();

      // Close all pages externally (simulates stale daemon state)
      const pages = testBrowser.getPages();
      for (const page of [...pages]) {
        await page.close();
      }

      // Wait for close events to propagate
      await new Promise((resolve) => setTimeout(resolve, 100));

      // isLaunched() is true but pages array is empty -- this is the stale state
      expect(testBrowser.isLaunched()).toBe(true);
      expect(testBrowser.getPages().length).toBe(0);

      // ensurePage() should recover by creating a new page
      await testBrowser.ensurePage();
      expect(testBrowser.getPages().length).toBe(1);
      expect(testBrowser.getPage()).toBeDefined();

      await testBrowser.close();
    });

    it('should be a no-op when pages already exist', async () => {
      const testBrowser = new BrowserManager();
      await testBrowser.launch({ headless: true });

      const pageBefore = testBrowser.getPage();
      await testBrowser.ensurePage();
      const pageAfter = testBrowser.getPage();

      // Should be the same page -- no-op
      expect(pageAfter).toBe(pageBefore);
      expect(testBrowser.getPages().length).toBe(1);

      await testBrowser.close();
    });
  });

  describe('scrollintoview with refs', () => {
    it('should resolve refs in scrollintoview command', async () => {
      const page = browser.getPage();
      await page.setContent(`
        <html>
          <body style="height: 3000px;">
            <div style="height: 2000px;"></div>
            <button id="far-button">Far Away Button</button>
          </body>
        </html>
      `);

      // Get snapshot to populate refs
      const { refs } = await browser.getSnapshot({ interactive: true });

      // Find the ref for our button
      const buttonRef = Object.keys(refs).find((k) => refs[k].name === 'Far Away Button');
      expect(buttonRef).toBeDefined();

      // scrollintoview with a ref should work, not throw a CSS selector error
      const result = await executeCommand(
        { id: 'test-1', action: 'scrollintoview', selector: `@${buttonRef}` },
        browser
      );
      expect(result.success).toBe(true);
    });

    it('should resolve refs in scroll command with selector', async () => {
      const page = browser.getPage();
      await page.setContent(`
        <html>
          <body style="height: 3000px;">
            <div id="scroll-container" style="height: 200px; overflow: auto;">
              <div style="height: 1000px;">Scrollable content</div>
            </div>
            <button id="target-btn">Target Button</button>
          </body>
        </html>
      `);

      const { refs } = await browser.getSnapshot({ interactive: true });
      const buttonRef = Object.keys(refs).find((k) => refs[k].name === 'Target Button');
      expect(buttonRef).toBeDefined();

      // scroll with a ref selector should work
      const result = await executeCommand(
        { id: 'test-2', action: 'scroll', selector: `@${buttonRef}`, y: 100 },
        browser
      );
      expect(result.success).toBe(true);
    });
  });

  describe('cursor-ref selector uniqueness', () => {
    it('should produce unique selectors for repeated DOM structures', async () => {
      const page = browser.getPage();
      // Build deeply nested identical structures where the distinguishing
      // ancestor (div.branch) is at level 4 from the target element --
      // beyond the previous 3-level path cutoff.
      await page.setContent(`
        <html>
          <body>
            <div class="root">
              <div class="branch">
                <div class="level1">
                  <div class="level2">
                    <div class="target" style="cursor: pointer; width: 100px; height: 30px;" onclick="void(0)">Item Alpha</div>
                  </div>
                </div>
              </div>
              <div class="branch">
                <div class="level1">
                  <div class="level2">
                    <div class="target" style="cursor: pointer; width: 100px; height: 30px;" onclick="void(0)">Item Beta</div>
                  </div>
                </div>
              </div>
            </div>
          </body>
        </html>
      `);

      const { refs } = await browser.getSnapshot({ interactive: true, cursor: true });

      // Find the cursor-interactive refs
      const cursorRefs = Object.entries(refs).filter(([, r]) => r.role === 'clickable');
      expect(cursorRefs.length).toBe(2);

      // Each ref's selector must be unique -- clicking it should not
      // trigger a strict mode violation.
      for (const [refKey] of cursorRefs) {
        const locator = browser.getLocator(`@${refKey}`);
        const count = await locator.count();
        expect(count).toBe(1);
      }
    });

    it('should click the correct element when refs have repeated structure', async () => {
      const page = browser.getPage();
      await page.setContent(`
        <html>
          <body>
            <div class="root">
              <div class="branch">
                <div class="level1">
                  <div class="level2">
                    <div class="target" style="cursor: pointer; width: 100px; height: 30px;"
                         onclick="document.getElementById('result').textContent = 'alpha'">Item Alpha</div>
                  </div>
                </div>
              </div>
              <div class="branch">
                <div class="level1">
                  <div class="level2">
                    <div class="target" style="cursor: pointer; width: 100px; height: 30px;"
                         onclick="document.getElementById('result').textContent = 'beta'">Item Beta</div>
                  </div>
                </div>
              </div>
            </div>
            <div id="result">none</div>
          </body>
        </html>
      `);

      const { refs } = await browser.getSnapshot({ interactive: true, cursor: true });

      // Find the ref for "Item Beta"
      const betaRef = Object.keys(refs).find((k) => refs[k].name === 'Item Beta');
      expect(betaRef).toBeDefined();

      // Click it -- should not throw strict mode violation
      const locator = browser.getLocator(`@${betaRef}`);
      await locator.click();

      const result = await page.locator('#result').textContent();
      expect(result).toBe('beta');
    });
  });

  describe('navigation', () => {
    it('should navigate to URL', async () => {
      const page = browser.getPage();
      await page.goto('https://example.com');
      expect(page.url()).toBe('https://example.com/');
    });

    it('should get page title', async () => {
      const page = browser.getPage();
      const title = await page.title();
      expect(title).toBe('Example Domain');
    });
  });

  describe('element interaction', () => {
    it('should find element by selector', async () => {
      const page = browser.getPage();
      const heading = await page.locator('h1').textContent();
      expect(heading).toBe('Example Domain');
    });

    it('should check element visibility', async () => {
      const page = browser.getPage();
      const isVisible = await page.locator('h1').isVisible();
      expect(isVisible).toBe(true);
    });

    it('should count elements', async () => {
      const page = browser.getPage();
      const count = await page.locator('p').count();
      expect(count).toBeGreaterThan(0);
    });
  });

  describe('screenshots', () => {
    it('should take screenshot as buffer', async () => {
      const page = browser.getPage();
      const buffer = await page.screenshot();
      expect(buffer).toBeInstanceOf(Buffer);
      expect(buffer.length).toBeGreaterThan(0);
    });
  });

  describe('evaluate', () => {
    it('should evaluate JavaScript', async () => {
      const page = browser.getPage();
      const result = await page.evaluate(() => document.title);
      expect(result).toBe('Example Domain');
    });

    it('should evaluate with arguments', async () => {
      const page = browser.getPage();
      const result = await page.evaluate((x: number) => x * 2, 5);
      expect(result).toBe(10);
    });
  });

  describe('tabs', () => {
    it('should create new tab', async () => {
      const result = await browser.newTab();
      expect(result.index).toBe(1);
      expect(result.total).toBe(2);
    });

    it('should list tabs', async () => {
      const tabs = await browser.listTabs();
      expect(tabs.length).toBe(2);
    });

    it('should close tab', async () => {
      // Switch to second tab and close it
      const page = browser.getPage();
      const tabs = await browser.listTabs();
      if (tabs.length > 1) {
        const result = await browser.closeTab(1);
        expect(result.remaining).toBe(1);
      }
    });

    it('should auto-switch to externally opened tab (window.open)', async () => {
      // Ensure we start on tab 0
      const initialIndex = browser.getActiveIndex();
      expect(initialIndex).toBe(0);

      const page = browser.getPage();

      // Use window.open to create a new tab externally (as a user/script would)
      await page.evaluate(() => {
        window.open('about:blank', '_blank');
      });

      // Wait for the new page event to be processed
      await new Promise((resolve) => setTimeout(resolve, 500));

      // Active tab should now be the newly opened tab
      const newIndex = browser.getActiveIndex();
      expect(newIndex).toBe(1);

      const tabs = await browser.listTabs();
      expect(tabs.length).toBe(2);
      expect(tabs[1].active).toBe(true);

      // Clean up: close the new tab
      await browser.closeTab(1);
    });
  });

  describe('context operations', () => {
    it('should get cookies from context', async () => {
      const page = browser.getPage();
      const cookies = await page.context().cookies();
      expect(Array.isArray(cookies)).toBe(true);
    });

    it('should set and get cookies', async () => {
      const page = browser.getPage();
      const context = page.context();
      await context.addCookies([{ name: 'test', value: 'value', url: 'https://example.com' }]);
      const cookies = await context.cookies();
      const testCookie = cookies.find((c) => c.name === 'test');
      expect(testCookie?.value).toBe('value');
    });

    it('should set cookie with domain', async () => {
      const page = browser.getPage();
      const context = page.context();
      await context.addCookies([
        { name: 'domainCookie', value: 'domainValue', domain: 'example.com', path: '/' },
      ]);
      const cookies = await context.cookies();
      const testCookie = cookies.find((c) => c.name === 'domainCookie');
      expect(testCookie?.value).toBe('domainValue');
    });

    it('should set multiple cookies at once', async () => {
      const page = browser.getPage();
      const context = page.context();
      await context.clearCookies();
      await context.addCookies([
        { name: 'cookie1', value: 'value1', url: 'https://example.com' },
        { name: 'cookie2', value: 'value2', url: 'https://example.com' },
      ]);
      const cookies = await context.cookies();
      expect(cookies.find((c) => c.name === 'cookie1')?.value).toBe('value1');
      expect(cookies.find((c) => c.name === 'cookie2')?.value).toBe('value2');
    });

    it('should clear cookies', async () => {
      const page = browser.getPage();
      const context = page.context();
      await context.clearCookies();
      const cookies = await context.cookies();
      expect(cookies.length).toBe(0);
    });
  });

  describe('localStorage operations', () => {
    it('should set and get localStorage item', async () => {
      const page = browser.getPage();
      await page.goto('https://example.com');
      await page.evaluate(() => localStorage.setItem('testKey', 'testValue'));
      const value = await page.evaluate(() => localStorage.getItem('testKey'));
      expect(value).toBe('testValue');
    });

    it('should get all localStorage items', async () => {
      const page = browser.getPage();
      await page.evaluate(() => {
        localStorage.clear();
        localStorage.setItem('key1', 'value1');
        localStorage.setItem('key2', 'value2');
      });
      const storage = await page.evaluate(() => {
        const items: Record<string, string> = {};
        for (let i = 0; i < localStorage.length; i++) {
          const key = localStorage.key(i);
          if (key) items[key] = localStorage.getItem(key) || '';
        }
        return items;
      });
      expect(storage.key1).toBe('value1');
      expect(storage.key2).toBe('value2');
    });

    it('should clear localStorage', async () => {
      const page = browser.getPage();
      await page.evaluate(() => localStorage.clear());
      const value = await page.evaluate(() => localStorage.getItem('testKey'));
      expect(value).toBeNull();
    });

    it('should return null for non-existent key', async () => {
      const page = browser.getPage();
      await page.evaluate(() => localStorage.clear());
      const value = await page.evaluate(() => localStorage.getItem('nonexistent'));
      expect(value).toBeNull();
    });
  });

  describe('sessionStorage operations', () => {
    it('should set and get sessionStorage item', async () => {
      const page = browser.getPage();
      await page.goto('https://example.com');
      await page.evaluate(() => sessionStorage.setItem('sessionKey', 'sessionValue'));
      const value = await page.evaluate(() => sessionStorage.getItem('sessionKey'));
      expect(value).toBe('sessionValue');
    });

    it('should get all sessionStorage items', async () => {
      const page = browser.getPage();
      await page.evaluate(() => {
        sessionStorage.clear();
        sessionStorage.setItem('skey1', 'svalue1');
        sessionStorage.setItem('skey2', 'svalue2');
      });
      const storage = await page.evaluate(() => {
        const items: Record<string, string> = {};
        for (let i = 0; i < sessionStorage.length; i++) {
          const key = sessionStorage.key(i);
          if (key) items[key] = sessionStorage.getItem(key) || '';
        }
        return items;
      });
      expect(storage.skey1).toBe('svalue1');
      expect(storage.skey2).toBe('svalue2');
    });

    it('should clear sessionStorage', async () => {
      const page = browser.getPage();
      await page.evaluate(() => sessionStorage.clear());
      const value = await page.evaluate(() => sessionStorage.getItem('sessionKey'));
      expect(value).toBeNull();
    });
  });

  describe('viewport', () => {
    it('should set viewport', async () => {
      await browser.setViewport(1920, 1080);
      const page = browser.getPage();
      const size = page.viewportSize();
      expect(size?.width).toBe(1920);
      expect(size?.height).toBe(1080);
    });

    it('should disable viewport when --start-maximized is in args', async () => {
      const testBrowser = new BrowserManager();
      await testBrowser.launch({ headless: true, args: ['--start-maximized'] });
      const page = testBrowser.getPage();
      expect(page.viewportSize()).toBeNull();
      await testBrowser.close();
    });

    it('should disable viewport when --window-size is in args', async () => {
      const testBrowser = new BrowserManager();
      await testBrowser.launch({ headless: true, args: ['--window-size=800,600'] });
      const page = testBrowser.getPage();
      expect(page.viewportSize()).toBeNull();
      await testBrowser.close();
    });

    it('should use default viewport when no window size args', async () => {
      const testBrowser = new BrowserManager();
      await testBrowser.launch({ headless: true });
      const page = testBrowser.getPage();
      expect(page.viewportSize()).toEqual({ width: 1280, height: 720 });
      await testBrowser.close();
    });

    it('should use explicit viewport even with --start-maximized', async () => {
      const testBrowser = new BrowserManager();
      await testBrowser.launch({
        headless: true,
        args: ['--start-maximized'],
        viewport: { width: 800, height: 600 },
      });
      const page = testBrowser.getPage();
      expect(page.viewportSize()).toEqual({ width: 800, height: 600 });
      await testBrowser.close();
    });
  });

  describe('snapshot', () => {
    it('should get snapshot with refs', async () => {
      const page = browser.getPage();
      await page.goto('https://example.com');
      const { tree, refs } = await browser.getSnapshot();
      expect(tree).toContain('heading');
      expect(tree).toContain('Example Domain');
      expect(typeof refs).toBe('object');
    });

    it('should get interactive-only snapshot', async () => {
      const { tree: fullSnapshot } = await browser.getSnapshot();
      const { tree: interactiveSnapshot } = await browser.getSnapshot({ interactive: true });
      // Interactive snapshot should be shorter (fewer elements)
      expect(interactiveSnapshot.length).toBeLessThanOrEqual(fullSnapshot.length);
    });

    it('should get snapshot with depth limit', async () => {
      const { tree: fullSnapshot } = await browser.getSnapshot();
      const { tree: limitedSnapshot } = await browser.getSnapshot({ maxDepth: 2 });
      // Limited depth should have fewer nested elements
      const fullLines = fullSnapshot.split('\n').length;
      const limitedLines = limitedSnapshot.split('\n').length;
      expect(limitedLines).toBeLessThanOrEqual(fullLines);
    });

    it('should get compact snapshot', async () => {
      const { tree: fullSnapshot } = await browser.getSnapshot();
      const { tree: compactSnapshot } = await browser.getSnapshot({ compact: true });
      // Compact should be equal or shorter
      expect(compactSnapshot.length).toBeLessThanOrEqual(fullSnapshot.length);
    });

    it('should not capture cursor-interactive elements without cursor flag', async () => {
      const page = browser.getPage();
      await page.setContent(`
        <html>
          <body>
            <button id="standard-btn">Standard Button</button>
            <div id="clickable-div" style="cursor: pointer;" onclick="void(0)">Clickable Div</div>
          </body>
        </html>
      `);

      const { tree, refs } = await browser.getSnapshot({ interactive: true });

      // Standard button should be captured via ARIA
      expect(tree).toContain('button "Standard Button"');

      // Cursor-interactive elements should NOT be captured without cursor flag
      expect(tree).not.toContain('Cursor-interactive elements');
      expect(tree).not.toContain('clickable "Clickable Div"');

      // Should only have refs for ARIA interactive elements
      const refValues = Object.values(refs);
      expect(refValues.some((r) => r.role === 'button')).toBe(true);
      expect(refValues.some((r) => r.role === 'clickable')).toBe(false);
    });

    it('should capture cursor-interactive elements with cursor flag', async () => {
      const page = browser.getPage();
      await page.setContent(`
        <html>
          <body>
            <button id="standard-btn">Standard Button</button>
            <div id="clickable-div" style="cursor: pointer;" onclick="void(0)">Clickable Div</div>
            <span onclick="void(0)">Onclick Span</span>
          </body>
        </html>
      `);

      const { tree, refs } = await browser.getSnapshot({ interactive: true, cursor: true });

      // Standard button should be captured via ARIA
      expect(tree).toContain('button "Standard Button"');

      // Cursor-interactive elements should be captured with cursor flag
      expect(tree).toContain('Cursor-interactive elements');
      expect(tree).toContain('clickable "Clickable Div"');
      expect(tree).toContain('clickable "Onclick Span"');

      // Should have refs for all interactive elements
      const refValues = Object.values(refs);
      expect(refValues.some((r) => r.role === 'button')).toBe(true);
      expect(refValues.some((r) => r.role === 'clickable')).toBe(true);
    });

    it('should click cursor-interactive elements via refs', async () => {
      const page = browser.getPage();
      await page.setContent(`
        <html>
          <body>
            <div id="clickable" style="cursor: pointer;" onclick="document.getElementById('result').textContent = 'clicked'">Click Me</div>
            <div id="result">not clicked</div>
          </body>
        </html>
      `);

      const { refs } = await browser.getSnapshot({ cursor: true });

      // Find the ref for the clickable element
      const clickableRef = Object.keys(refs).find((k) => refs[k].name === 'Click Me');
      expect(clickableRef).toBeDefined();

      // Click using the ref
      const locator = browser.getLocator(`@${clickableRef}`);
      await locator.click();

      // Verify click worked
      const result = await page.locator('#result').textContent();
      expect(result).toBe('clicked');
    });
  });

  describe('locator resolution', () => {
    it('should resolve CSS selector', async () => {
      const page = browser.getPage();
      await page.goto('https://example.com');
      const locator = browser.getLocator('h1');
      const text = await locator.textContent();
      expect(text).toBe('Example Domain');
    });

    it('should resolve ref from snapshot', async () => {
      await browser.getSnapshot(); // Populates refs
      // After snapshot, refs like @e1 should be available
      // This tests the ref resolution mechanism
      const page = browser.getPage();
      const h1 = await page.locator('h1').textContent();
      expect(h1).toBe('Example Domain');
    });
  });

  describe('scoped headers', () => {
    it('should register route for scoped headers', async () => {
      // Test that setScopedHeaders doesn't throw and completes successfully
      await browser.clearScopedHeaders();
      await expect(
        browser.setScopedHeaders('https://example.com', { 'X-Test': 'value' })
      ).resolves.not.toThrow();
      await browser.clearScopedHeaders();
    });

    it('should handle full URL origin', async () => {
      await browser.clearScopedHeaders();
      await expect(
        browser.setScopedHeaders('https://api.example.com/path', { Authorization: 'Bearer token' })
      ).resolves.not.toThrow();
      await browser.clearScopedHeaders();
    });

    it('should handle hostname-only origin', async () => {
      await browser.clearScopedHeaders();
      await expect(
        browser.setScopedHeaders('example.com', { 'X-Custom': 'value' })
      ).resolves.not.toThrow();
      await browser.clearScopedHeaders();
    });

    it('should clear scoped headers for specific origin', async () => {
      await browser.clearScopedHeaders();
      await browser.setScopedHeaders('https://example.com', { 'X-Test': 'value' });
      await expect(browser.clearScopedHeaders('https://example.com')).resolves.not.toThrow();
    });

    it('should clear all scoped headers', async () => {
      await browser.setScopedHeaders('https://example.com', { 'X-Test-1': 'value1' });
      await browser.setScopedHeaders('https://example.org', { 'X-Test-2': 'value2' });
      await expect(browser.clearScopedHeaders()).resolves.not.toThrow();
    });

    it('should replace headers when called twice for same origin', async () => {
      await browser.clearScopedHeaders();
      await browser.setScopedHeaders('https://example.com', { 'X-First': 'first' });
      // Second call should replace, not add
      await expect(
        browser.setScopedHeaders('https://example.com', { 'X-Second': 'second' })
      ).resolves.not.toThrow();
      await browser.clearScopedHeaders();
    });

    it('should handle clearing non-existent origin gracefully', async () => {
      await browser.clearScopedHeaders();
      // Should not throw when clearing headers that were never set
      await expect(browser.clearScopedHeaders('https://never-set.com')).resolves.not.toThrow();
    });
  });

  describe('CDP session', () => {
    it('should create CDP session on demand', async () => {
      const cdp = await browser.getCDPSession();
      expect(cdp).toBeDefined();
    });

    it('should reuse existing CDP session', async () => {
      const cdp1 = await browser.getCDPSession();
      const cdp2 = await browser.getCDPSession();
      expect(cdp1).toBe(cdp2);
    });

    it('should filter out pages with empty URLs during CDP connection', async () => {
      const mockBrowser = {
        contexts: () => [
          {
            pages: () => [
              { url: () => 'http://example.com', on: vi.fn() },
              { url: () => '', on: vi.fn() }, // This page should be filtered out
              { url: () => 'http://anothersite.com', on: vi.fn() },
            ],
            on: vi.fn(),
            setDefaultTimeout: vi.fn(),
          },
        ],
        close: vi.fn(),
      };
      const spy = vi.spyOn(chromium, 'connectOverCDP').mockResolvedValue(mockBrowser as any);

      const cdpBrowser = new BrowserManager();
      await cdpBrowser.launch({ cdpPort: 9222 });

      // Should have 2 pages, not 3
      expect(cdpBrowser.getPages().length).toBe(2);

      // Verify that the empty URL page is not in the list
      const urls = cdpBrowser.getPages().map((p) => p.url());
      expect(urls).not.toContain('');
      expect(urls).toContain('http://example.com');
      spy.mockRestore();
    });
  });

  describe('screencast', () => {
    it('should report screencasting state correctly', () => {
      expect(browser.isScreencasting()).toBe(false);
    });

    it('should start screencast', async () => {
      const frames: Array<{ data: string }> = [];
      await browser.startScreencast((frame) => {
        frames.push(frame);
      });
      expect(browser.isScreencasting()).toBe(true);

      // Wait a bit for at least one frame
      await new Promise((resolve) => setTimeout(resolve, 1000));

      await browser.stopScreencast();
      expect(browser.isScreencasting()).toBe(false);
      expect(frames.length).toBeGreaterThan(0);
    });

    it('should start screencast with custom options', async () => {
      const frames: Array<{ data: string }> = [];
      await browser.startScreencast(
        (frame) => {
          frames.push(frame);
        },
        {
          format: 'png',
          quality: 100,
          maxWidth: 800,
          maxHeight: 600,
          everyNthFrame: 1,
        }
      );
      expect(browser.isScreencasting()).toBe(true);

      // Wait for a frame
      await new Promise((resolve) => setTimeout(resolve, 200));

      await browser.stopScreencast();
      expect(frames.length).toBeGreaterThan(0);
    });

    it('should throw when starting screencast twice', async () => {
      await browser.startScreencast(() => {});
      await expect(browser.startScreencast(() => {})).rejects.toThrow('Screencast already active');
      await browser.stopScreencast();
    });

    it('should handle stop when not screencasting', async () => {
      // Should not throw
      await expect(browser.stopScreencast()).resolves.not.toThrow();
    });
  });

  describe('tab switch invalidates CDP session', () => {
    // Clean up any extra tabs before each test
    beforeEach(async () => {
      // Close all tabs except the first one
      const tabs = await browser.listTabs();
      for (let i = tabs.length - 1; i > 0; i--) {
        await browser.closeTab(i);
      }
      // Ensure we're on tab 0
      await browser.switchTo(0);
      // Stop any active screencast
      if (browser.isScreencasting()) {
        await browser.stopScreencast();
      }
    });

    it('should not invalidate CDP when switching to same tab', async () => {
      // Get CDP session for current tab
      const cdp1 = await browser.getCDPSession();

      // Switch to same tab - should NOT invalidate
      await browser.switchTo(0);

      // Should be the same session
      const cdp2 = await browser.getCDPSession();
      expect(cdp2).toBe(cdp1);
    });

    it('should invalidate CDP session on tab switch', async () => {
      // Get CDP session for tab 0
      const cdp1 = await browser.getCDPSession();
      expect(cdp1).toBeDefined();

      // Create new tab - this switches to the new tab automatically
      await browser.newTab();

      // Get CDP session - should be different since we're on a new page
      const cdp2 = await browser.getCDPSession();
      expect(cdp2).toBeDefined();

      // Sessions should be different objects (different pages have different CDP sessions)
      expect(cdp2).not.toBe(cdp1);
    });

    it('should stop screencast on tab switch', async () => {
      // Start screencast on tab 0
      await browser.startScreencast(() => {});
      expect(browser.isScreencasting()).toBe(true);

      // Create new tab and switch
      await browser.newTab();
      await browser.switchTo(1);

      // Screencast should be stopped (it's page-specific)
      expect(browser.isScreencasting()).toBe(false);
    });
  });

  describe('profiling (CDP tracing)', () => {
    const fs = require('node:fs/promises');
    const path = require('node:path');
    const testOutputDir = '/tmp/agent-browser-test';

    beforeAll(async () => {
      // Ensure test output directory exists
      await fs.mkdir(testOutputDir, { recursive: true }).catch(() => {});
    });

    afterEach(async () => {
      // Stop profiling if still active
      if (browser.isProfilingActive()) {
        const tempPath = path.join(testOutputDir, 'cleanup.json');
        await browser.stopProfiling(tempPath).catch(() => {});
      }
    });

    it('should report profiling state correctly', () => {
      expect(browser.isProfilingActive()).toBe(false);
    });

    it('should start profiling', async () => {
      await browser.startProfiling();
      expect(browser.isProfilingActive()).toBe(true);
    });

    it('should throw when starting profiling twice', async () => {
      await browser.startProfiling();
      await expect(browser.startProfiling()).rejects.toThrow('Profiling already active');
    });

    it('should stop profiling and write file', async () => {
      await browser.startProfiling();

      const outputPath = path.join(testOutputDir, 'test-profile.json');
      const result = await browser.stopProfiling(outputPath);

      expect(result.path).toBe(outputPath);
      expect(typeof result.eventCount).toBe('number');
      expect(browser.isProfilingActive()).toBe(false);

      // Verify file was written
      const fileExists = await fs
        .access(outputPath)
        .then(() => true)
        .catch(() => false);
      expect(fileExists).toBe(true);

      // Verify file content is valid JSON with traceEvents
      const content = await fs.readFile(outputPath, 'utf-8');
      const data = JSON.parse(content);
      expect(data).toHaveProperty('traceEvents');
      expect(Array.isArray(data.traceEvents)).toBe(true);

      // Cleanup
      await fs.unlink(outputPath).catch(() => {});
    });

    it('should throw when stopping without start', async () => {
      expect(browser.isProfilingActive()).toBe(false);
      await expect(browser.stopProfiling('/tmp/should-not-exist.json')).rejects.toThrow(
        'No profiling session active'
      );
    });

    it('should start profiling with custom categories', async () => {
      await browser.startProfiling({ categories: ['devtools.timeline', 'v8.execute'] });
      expect(browser.isProfilingActive()).toBe(true);

      // Stop and cleanup
      const outputPath = path.join(testOutputDir, 'custom-categories.json');
      await browser.stopProfiling(outputPath);
      await fs.unlink(outputPath).catch(() => {});
    });
  });

  describe('input injection', () => {
    it('should inject mouse move event', async () => {
      await expect(
        browser.injectMouseEvent({
          type: 'mouseMoved',
          x: 100,
          y: 100,
        })
      ).resolves.not.toThrow();
    });

    it('should inject mouse click events', async () => {
      await expect(
        browser.injectMouseEvent({
          type: 'mousePressed',
          x: 100,
          y: 100,
          button: 'left',
          clickCount: 1,
        })
      ).resolves.not.toThrow();

      await expect(
        browser.injectMouseEvent({
          type: 'mouseReleased',
          x: 100,
          y: 100,
          button: 'left',
        })
      ).resolves.not.toThrow();
    });

    it('should inject mouse wheel event', async () => {
      await expect(
        browser.injectMouseEvent({
          type: 'mouseWheel',
          x: 100,
          y: 100,
          deltaX: 0,
          deltaY: 100,
        })
      ).resolves.not.toThrow();
    });

    it('should inject keyboard events', async () => {
      await expect(
        browser.injectKeyboardEvent({
          type: 'keyDown',
          key: 'a',
          code: 'KeyA',
        })
      ).resolves.not.toThrow();

      await expect(
        browser.injectKeyboardEvent({
          type: 'keyUp',
          key: 'a',
          code: 'KeyA',
        })
      ).resolves.not.toThrow();
    });

    it('should inject char event', async () => {
      // CDP char events only accept single characters
      await expect(
        browser.injectKeyboardEvent({
          type: 'char',
          text: 'h',
        })
      ).resolves.not.toThrow();
    });

    it('should inject keyboard with modifiers', async () => {
      await expect(
        browser.injectKeyboardEvent({
          type: 'keyDown',
          key: 'c',
          code: 'KeyC',
          modifiers: 2, // Ctrl
        })
      ).resolves.not.toThrow();
    });

    it('should inject touch events', async () => {
      await expect(
        browser.injectTouchEvent({
          type: 'touchStart',
          touchPoints: [{ x: 100, y: 100 }],
        })
      ).resolves.not.toThrow();

      await expect(
        browser.injectTouchEvent({
          type: 'touchMove',
          touchPoints: [{ x: 150, y: 150 }],
        })
      ).resolves.not.toThrow();

      await expect(
        browser.injectTouchEvent({
          type: 'touchEnd',
          touchPoints: [],
        })
      ).resolves.not.toThrow();
    });

    it('should inject multi-touch events', async () => {
      await expect(
        browser.injectTouchEvent({
          type: 'touchStart',
          touchPoints: [
            { x: 100, y: 100, id: 0 },
            { x: 200, y: 200, id: 1 },
          ],
        })
      ).resolves.not.toThrow();

      await expect(
        browser.injectTouchEvent({
          type: 'touchEnd',
          touchPoints: [],
        })
      ).resolves.not.toThrow();
    });
  });
});
