import { describe, it, expect } from 'vitest';
import { toAIFriendlyError } from './actions.js';

describe('toAIFriendlyError', () => {
  describe('element blocked by overlay', () => {
    it('should detect intercepts pointer events even when Timeout is in message', () => {
      // This is the exact error from Playwright when a cookie banner blocks an element
      // Bug: Previously this was incorrectly reported as "not found or not visible"
      const error = new Error(
        'TimeoutError: locator.click: Timeout 10000ms exceeded.\n' +
          'Call log:\n' +
          "  - waiting for getByRole('link', { name: 'Anmelden', exact: true }).first()\n" +
          '    - locator resolved to <a href="https://example.com/login">Anmelden</a>\n' +
          '  - attempting click action\n' +
          '    2 x waiting for element to be visible, enabled and stable\n' +
          '      - element is visible, enabled and stable\n' +
          '      - scrolling into view if needed\n' +
          '      - done scrolling\n' +
          '      - <body class="font-sans antialiased">...</body> intercepts pointer events\n' +
          '    - retrying click action'
      );

      const result = toAIFriendlyError(error, '@e4');

      // Must NOT say "not found" - the element WAS found
      expect(result.message).not.toContain('not found');
      // Must indicate the element is blocked
      expect(result.message).toContain('blocked by another element');
      expect(result.message).toContain('modal or overlay');
    });

    it('should suggest dismissing cookie banners', () => {
      const error = new Error('<div class="cookie-overlay"> intercepts pointer events');
      const result = toAIFriendlyError(error, '@e1');

      expect(result.message).toContain('cookie banners');
    });
  });

  describe('element is not stable (infinite CSS animation)', () => {
    // Playwright's stability check times out when a button has an infinite CSS
    // animation (e.g. a pulsing "call to action" effect in a game UI).
    // handleClick catches this specific error and retries with force:true rather
    // than surfacing a generic timeout to the user.
    //
    // If the force retry itself fails, the error reaches toAIFriendlyError.
    // Verify it is treated as a generic action timeout (not "not found").
    it('not-stable timeout falls back to generic timed-out message', () => {
      const error = new Error(
        'locator.click: Timeout 25000ms exceeded.\nCall log:\n' +
          "  - waiting for getByRole('button', { name: 'Gæt', exact: true })\n" +
          "  - locator resolved to <button class='_enter _q'>Gæt</button>\n" +
          '  - attempting click action\n' +
          '    - waiting for element to be visible, enabled and stable\n' +
          '    - element is not stable\n' +
          '    - retrying click action'
      );

      const result = toAIFriendlyError(error, '@e34');

      expect(result.message).toContain('timed out');
      expect(result.message).not.toContain('not found');
      expect(result.message).not.toContain('not visible');
    });
  });
});
