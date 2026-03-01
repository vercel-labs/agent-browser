import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { BrowserManager } from './browser.js';
import { executeCommand } from './actions.js';

describe('frameLocator support', () => {
  let browser: BrowserManager;

  beforeAll(async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true });
  });

  afterAll(async () => {
    await browser.close();
  });

  describe('setFrameLocator / getLocatorBase', () => {
    it('should return page when no frameLocator is set', () => {
      const base = browser.getLocatorBase();
      expect(base).toBe(browser.getPage());
    });

    it('should return a FrameLocator when frameLocator is set', () => {
      browser.setFrameLocator('iframe#child');
      const base = browser.getLocatorBase();
      // FrameLocator is not a Page — it should differ
      expect(base).not.toBe(browser.getPage());
      // FrameLocator must support locator-producing methods
      expect(typeof (base as any).getByRole).toBe('function');
      expect(typeof (base as any).getByText).toBe('function');
      expect(typeof (base as any).locator).toBe('function');
    });

    it('should clear frameLocator when set to null', () => {
      browser.setFrameLocator('iframe#child');
      expect(browser.getLocatorBase()).not.toBe(browser.getPage());

      browser.setFrameLocator(null);
      expect(browser.getLocatorBase()).toBe(browser.getPage());
    });

    it('should clear frameLocator via switchToMainFrame', () => {
      browser.setFrameLocator('iframe#child');
      expect(browser.getLocatorBase()).not.toBe(browser.getPage());

      browser.switchToMainFrame();
      expect(browser.getLocatorBase()).toBe(browser.getPage());
    });
  });

  describe('framelocator command via executeCommand', () => {
    it('should set frameLocator with selector', async () => {
      const resp = await executeCommand(
        { id: '1', action: 'framelocator', selector: 'iframe#child' },
        browser
      );
      expect(resp.success).toBe(true);
      expect((resp as any).data.switched).toBe(true);
      // getLocatorBase should now scope to the frame
      expect(browser.getLocatorBase()).not.toBe(browser.getPage());
    });

    it('should clear frameLocator without selector', async () => {
      // First set it
      await executeCommand({ id: '1', action: 'framelocator', selector: 'iframe#child' }, browser);

      // Then clear it
      const resp = await executeCommand({ id: '2', action: 'framelocator' }, browser);
      expect(resp.success).toBe(true);
      expect((resp as any).data.cleared).toBe(true);
      expect(browser.getLocatorBase()).toBe(browser.getPage());
    });
  });

  describe('getLocator respects frameLocator', () => {
    it('should scope getLocator through frameLocator when set', () => {
      browser.setFrameLocator('iframe#child');
      const locator = browser.getLocator('button');
      // The locator should be scoped; verify it's created without error
      expect(locator).toBeDefined();
      browser.setFrameLocator(null);
    });

    it('should use page.locator when frameLocator is cleared', () => {
      browser.setFrameLocator(null);
      const locator = browser.getLocator('button');
      expect(locator).toBeDefined();
    });
  });
});
