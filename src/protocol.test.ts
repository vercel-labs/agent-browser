import { describe, it, expect } from 'vitest';
import { parseCommand } from './protocol.js';

// Helper to create command JSON string
const cmd = (obj: object) => JSON.stringify(obj);

describe('parseCommand', () => {
  describe('navigation', () => {
    it('should parse navigate command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'navigate', url: 'https://example.com' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('navigate');
        expect(result.command.url).toBe('https://example.com');
      }
    });

    it('should parse navigate with headers', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'navigate',
          url: 'https://example.com',
          headers: { Authorization: 'Bearer token' },
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('navigate');
        expect(result.command.headers).toEqual({ Authorization: 'Bearer token' });
      }
    });

    it('should reject navigate without url', () => {
      const result = parseCommand(cmd({ id: '1', action: 'navigate' }));
      expect(result.success).toBe(false);
    });

    it('should parse back command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'back' }));
      expect(result.success).toBe(true);
    });

    it('should parse forward command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'forward' }));
      expect(result.success).toBe(true);
    });

    it('should parse reload command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'reload' }));
      expect(result.success).toBe(true);
    });
  });

  describe('click', () => {
    it('should parse click command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'click', selector: '#btn' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('click');
        expect(result.command.selector).toBe('#btn');
      }
    });

    it('should reject click without selector', () => {
      const result = parseCommand(cmd({ id: '1', action: 'click' }));
      expect(result.success).toBe(false);
    });
  });

  describe('type', () => {
    it('should parse type command', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'type', selector: '#input', text: 'hello' })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('type');
        expect(result.command.selector).toBe('#input');
        expect(result.command.text).toBe('hello');
      }
    });
  });

  describe('fill', () => {
    it('should parse fill command', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'fill', selector: '#input', value: 'hello' })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('fill');
        expect(result.command.value).toBe('hello');
      }
    });
  });

  describe('wait', () => {
    it('should parse wait with selector', () => {
      const result = parseCommand(cmd({ id: '1', action: 'wait', selector: '#loading' }));
      expect(result.success).toBe(true);
    });

    it('should parse wait with timeout', () => {
      const result = parseCommand(cmd({ id: '1', action: 'wait', timeout: 5000 }));
      expect(result.success).toBe(true);
    });

    it('should parse wait with text', () => {
      const result = parseCommand(cmd({ id: '1', action: 'wait', text: 'Welcome' }));
      expect(result.success).toBe(true);
    });
  });

  describe('screenshot', () => {
    it('should parse screenshot command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'screenshot', path: 'test.png' }));
      expect(result.success).toBe(true);
    });

    it('should parse screenshot with fullPage', () => {
      const result = parseCommand(cmd({ id: '1', action: 'screenshot', fullPage: true }));
      expect(result.success).toBe(true);
    });

    it('should parse screenshot with null selector', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'screenshot', path: 'test.png', selector: null })
      );
      expect(result.success).toBe(true);
    });

    it('should parse screenshot with annotate', () => {
      const result = parseCommand(cmd({ id: '1', action: 'screenshot', annotate: true }));
      expect(result.success).toBe(true);
    });
  });

  describe('cookies', () => {
    it('should parse cookies_get', () => {
      const result = parseCommand(cmd({ id: '1', action: 'cookies_get' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('cookies_get');
      }
    });

    it('should parse cookies_get with urls filter', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'cookies_get', urls: ['https://example.com'] })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.urls).toEqual(['https://example.com']);
      }
    });

    it('should parse cookies_set with minimal cookie', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'cookies_set',
          cookies: [{ name: 'session', value: 'abc123' }],
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('cookies_set');
        expect(result.command.cookies).toHaveLength(1);
        expect(result.command.cookies[0].name).toBe('session');
        expect(result.command.cookies[0].value).toBe('abc123');
      }
    });

    it('should parse cookies_set with full cookie options', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'cookies_set',
          cookies: [
            {
              name: 'auth',
              value: 'token123',
              domain: 'example.com',
              path: '/',
              expires: Date.now() / 1000 + 3600,
              httpOnly: true,
              secure: true,
              sameSite: 'Strict',
            },
          ],
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.cookies[0].httpOnly).toBe(true);
        expect(result.command.cookies[0].secure).toBe(true);
        expect(result.command.cookies[0].sameSite).toBe('Strict');
      }
    });

    it('should parse cookies_set with multiple cookies', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'cookies_set',
          cookies: [
            { name: 'cookie1', value: 'value1' },
            { name: 'cookie2', value: 'value2' },
          ],
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.cookies).toHaveLength(2);
      }
    });

    it('should reject cookies_set without cookies array', () => {
      const result = parseCommand(cmd({ id: '1', action: 'cookies_set' }));
      expect(result.success).toBe(false);
    });

    it('should accept cookies_set with empty cookies array', () => {
      // Empty array is technically valid (no-op)
      const result = parseCommand(cmd({ id: '1', action: 'cookies_set', cookies: [] }));
      expect(result.success).toBe(true);
    });

    it('should reject cookies_set with cookie missing name', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'cookies_set', cookies: [{ value: 'test' }] })
      );
      expect(result.success).toBe(false);
    });

    it('should reject cookies_set with cookie missing value', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'cookies_set', cookies: [{ name: 'test' }] })
      );
      expect(result.success).toBe(false);
    });

    it('should reject cookies_set with invalid sameSite value', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'cookies_set',
          cookies: [{ name: 'test', value: 'val', sameSite: 'Invalid' }],
        })
      );
      expect(result.success).toBe(false);
    });

    it('should parse cookies_clear', () => {
      const result = parseCommand(cmd({ id: '1', action: 'cookies_clear' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('cookies_clear');
      }
    });
  });

  describe('storage', () => {
    it('should parse storage_get for localStorage', () => {
      const result = parseCommand(cmd({ id: '1', action: 'storage_get', type: 'local' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('storage_get');
        expect(result.command.type).toBe('local');
      }
    });

    it('should parse storage_get for sessionStorage', () => {
      const result = parseCommand(cmd({ id: '1', action: 'storage_get', type: 'session' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.type).toBe('session');
      }
    });

    it('should parse storage_get with specific key', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'storage_get', type: 'local', key: 'mykey' })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.key).toBe('mykey');
      }
    });

    it('should parse storage_set', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'storage_set',
          type: 'local',
          key: 'test',
          value: 'value',
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('storage_set');
        expect(result.command.key).toBe('test');
        expect(result.command.value).toBe('value');
      }
    });

    it('should reject storage_set without key', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'storage_set',
          type: 'local',
          value: 'value',
        })
      );
      expect(result.success).toBe(false);
    });

    it('should reject storage_set without value', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'storage_set',
          type: 'local',
          key: 'test',
        })
      );
      expect(result.success).toBe(false);
    });

    it('should parse storage_clear for localStorage', () => {
      const result = parseCommand(cmd({ id: '1', action: 'storage_clear', type: 'local' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('storage_clear');
        expect(result.command.type).toBe('local');
      }
    });

    it('should parse storage_clear for sessionStorage', () => {
      const result = parseCommand(cmd({ id: '1', action: 'storage_clear', type: 'session' }));
      expect(result.success).toBe(true);
    });

    it('should reject storage_get without type', () => {
      const result = parseCommand(cmd({ id: '1', action: 'storage_get' }));
      expect(result.success).toBe(false);
    });

    it('should reject storage_get with invalid type', () => {
      const result = parseCommand(cmd({ id: '1', action: 'storage_get', type: 'invalid' }));
      expect(result.success).toBe(false);
    });
  });

  describe('semantic locators', () => {
    it('should parse getbyrole', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'getbyrole',
          role: 'button',
          subaction: 'click',
        })
      );
      expect(result.success).toBe(true);
    });

    it('should parse getbyrole with exact', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'getbyrole',
          role: 'button',
          name: 'Continue',
          subaction: 'click',
          exact: true,
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.exact).toBe(true);
      }
    });

    it('should parse getbytext', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'getbytext',
          text: 'Submit',
          subaction: 'click',
        })
      );
      expect(result.success).toBe(true);
    });

    it('should parse getbylabel', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'getbylabel',
          label: 'Email',
          subaction: 'fill',
          value: 'test@test.com',
        })
      );
      expect(result.success).toBe(true);
    });

    it('should parse getbylabel with exact', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'getbylabel',
          label: 'Email',
          subaction: 'fill',
          value: 'test@test.com',
          exact: true,
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.exact).toBe(true);
      }
    });

    it('should parse getbyplaceholder with exact', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'getbyplaceholder',
          placeholder: 'Search',
          subaction: 'fill',
          value: 'test',
          exact: true,
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.exact).toBe(true);
      }
    });
  });

  describe('tabs', () => {
    it('should parse tab_new', () => {
      const result = parseCommand(cmd({ id: '1', action: 'tab_new' }));
      expect(result.success).toBe(true);
    });

    it('should parse tab_new with url', () => {
      const result = parseCommand(cmd({ id: '1', action: 'tab_new', url: 'https://example.com' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect((result.command as { url?: string }).url).toBe('https://example.com');
      }
    });

    it('should parse tab_list', () => {
      const result = parseCommand(cmd({ id: '1', action: 'tab_list' }));
      expect(result.success).toBe(true);
    });

    it('should parse tab_switch', () => {
      const result = parseCommand(cmd({ id: '1', action: 'tab_switch', index: 0 }));
      expect(result.success).toBe(true);
    });

    it('should parse tab_close', () => {
      const result = parseCommand(cmd({ id: '1', action: 'tab_close' }));
      expect(result.success).toBe(true);
    });
  });

  describe('snapshot', () => {
    it('should parse basic snapshot command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'snapshot' }));
      expect(result.success).toBe(true);
    });

    it('should parse snapshot with interactive filter', () => {
      const result = parseCommand(cmd({ id: '1', action: 'snapshot', interactive: true }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.interactive).toBe(true);
      }
    });

    it('should parse snapshot with compact filter', () => {
      const result = parseCommand(cmd({ id: '1', action: 'snapshot', compact: true }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.compact).toBe(true);
      }
    });

    it('should parse snapshot with maxDepth', () => {
      const result = parseCommand(cmd({ id: '1', action: 'snapshot', maxDepth: 3 }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.maxDepth).toBe(3);
      }
    });

    it('should parse snapshot with selector scope', () => {
      const result = parseCommand(cmd({ id: '1', action: 'snapshot', selector: '#main' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.selector).toBe('#main');
      }
    });

    it('should parse snapshot with all options', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'snapshot',
          interactive: true,
          compact: true,
          maxDepth: 5,
          selector: '.content',
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.interactive).toBe(true);
        expect(result.command.compact).toBe(true);
        expect(result.command.maxDepth).toBe(5);
        expect(result.command.selector).toBe('.content');
      }
    });
  });

  describe('launch', () => {
    it('should parse launch command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch' }));
      expect(result.success).toBe(true);
    });

    it('should parse launch with headless false', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', headless: false }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.headless).toBe(false);
      }
    });

    it('should parse launch with cdpPort', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', cdpPort: 9222 }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.cdpPort).toBe(9222);
      }
    });

    it('should reject launch with invalid cdpPort', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', cdpPort: -1 }));
      expect(result.success).toBe(false);
    });

    it('should reject launch with non-numeric cdpPort', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', cdpPort: 'invalid' }));
      expect(result.success).toBe(false);
    });

    it('should parse launch with ignoreHTTPSErrors true', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', ignoreHTTPSErrors: true }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.ignoreHTTPSErrors).toBe(true);
      }
    });

    it('should parse launch with ignoreHTTPSErrors false', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', ignoreHTTPSErrors: false }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.ignoreHTTPSErrors).toBe(false);
      }
    });

    it('should reject launch with non-boolean ignoreHTTPSErrors', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', ignoreHTTPSErrors: 'true' }));
      expect(result.success).toBe(false);
    });

    it('should parse launch with allowFileAccess true', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', allowFileAccess: true }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.allowFileAccess).toBe(true);
      }
    });

    it('should parse launch with allowFileAccess false', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', allowFileAccess: false }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.allowFileAccess).toBe(false);
      }
    });

    it('should parse launch with viewport dimensions', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'launch', viewport: { width: 1920, height: 1080 } })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.viewport).toEqual({ width: 1920, height: 1080 });
      }
    });

    it('should parse launch with viewport null', () => {
      const result = parseCommand(cmd({ id: '1', action: 'launch', viewport: null }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.viewport).toBeNull();
      }
    });

    it('should reject launch with invalid viewport', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'launch', viewport: { width: -1, height: 720 } })
      );
      expect(result.success).toBe(false);
    });
  });

  describe('mouse actions', () => {
    it('should parse mousemove', () => {
      const result = parseCommand(cmd({ id: '1', action: 'mousemove', x: 100, y: 200 }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.x).toBe(100);
        expect(result.command.y).toBe(200);
      }
    });

    it('should parse mousedown', () => {
      const result = parseCommand(cmd({ id: '1', action: 'mousedown', button: 'left' }));
      expect(result.success).toBe(true);
    });

    it('should parse mouseup', () => {
      const result = parseCommand(cmd({ id: '1', action: 'mouseup', button: 'left' }));
      expect(result.success).toBe(true);
    });

    it('should parse wheel', () => {
      const result = parseCommand(cmd({ id: '1', action: 'wheel', deltaX: 0, deltaY: 100 }));
      expect(result.success).toBe(true);
    });
  });

  describe('scroll', () => {
    it('should parse scroll command', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'scroll', direction: 'down', amount: 300 })
      );
      expect(result.success).toBe(true);
    });

    it('should parse scrollintoview', () => {
      const result = parseCommand(cmd({ id: '1', action: 'scrollintoview', selector: '#element' }));
      expect(result.success).toBe(true);
    });
  });

  describe('element state', () => {
    it('should parse isvisible', () => {
      const result = parseCommand(cmd({ id: '1', action: 'isvisible', selector: '#btn' }));
      expect(result.success).toBe(true);
    });

    it('should parse isenabled', () => {
      const result = parseCommand(cmd({ id: '1', action: 'isenabled', selector: '#btn' }));
      expect(result.success).toBe(true);
    });

    it('should parse ischecked', () => {
      const result = parseCommand(cmd({ id: '1', action: 'ischecked', selector: '#checkbox' }));
      expect(result.success).toBe(true);
    });
  });

  describe('viewport and settings', () => {
    it('should parse viewport', () => {
      const result = parseCommand(cmd({ id: '1', action: 'viewport', width: 1920, height: 1080 }));
      expect(result.success).toBe(true);
    });

    it('should parse geolocation', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'geolocation', latitude: 37.7749, longitude: -122.4194 })
      );
      expect(result.success).toBe(true);
    });

    it('should parse offline', () => {
      const result = parseCommand(cmd({ id: '1', action: 'offline', offline: true }));
      expect(result.success).toBe(true);
    });
  });

  describe('trace', () => {
    it('should parse trace_start', () => {
      const result = parseCommand(cmd({ id: '1', action: 'trace_start' }));
      expect(result.success).toBe(true);
    });

    it('should parse trace_stop', () => {
      const result = parseCommand(cmd({ id: '1', action: 'trace_stop', path: 'trace.zip' }));
      expect(result.success).toBe(true);
    });
  });

  describe('console and errors', () => {
    it('should parse console', () => {
      const result = parseCommand(cmd({ id: '1', action: 'console' }));
      expect(result.success).toBe(true);
    });

    it('should parse console with clear', () => {
      const result = parseCommand(cmd({ id: '1', action: 'console', clear: true }));
      expect(result.success).toBe(true);
    });

    it('should parse errors', () => {
      const result = parseCommand(cmd({ id: '1', action: 'errors' }));
      expect(result.success).toBe(true);
    });
  });

  describe('dialog', () => {
    it('should parse dialog accept', () => {
      const result = parseCommand(cmd({ id: '1', action: 'dialog', response: 'accept' }));
      expect(result.success).toBe(true);
    });

    it('should parse dialog dismiss', () => {
      const result = parseCommand(cmd({ id: '1', action: 'dialog', response: 'dismiss' }));
      expect(result.success).toBe(true);
    });

    it('should parse dialog accept with prompt text', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'dialog', response: 'accept', promptText: 'hello' })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.promptText).toBe('hello');
      }
    });
  });

  describe('frame', () => {
    it('should parse frame command', () => {
      const result = parseCommand(cmd({ id: '1', action: 'frame', selector: '#iframe' }));
      expect(result.success).toBe(true);
    });

    it('should reject frame with no selector, name, or url', () => {
      const result = parseCommand(cmd({ id: '1', action: 'frame' }));
      expect(result.success).toBe(false);
    });

    it('should parse frame with name', () => {
      const result = parseCommand(cmd({ id: '1', action: 'frame', name: 'myframe' }));
      expect(result.success).toBe(true);
    });

    it('should parse frame with url', () => {
      const result = parseCommand(cmd({ id: '1', action: 'frame', url: 'https://example.com' }));
      expect(result.success).toBe(true);
    });

    it('should parse mainframe', () => {
      const result = parseCommand(cmd({ id: '1', action: 'mainframe' }));
      expect(result.success).toBe(true);
    });
  });

  describe('screencast', () => {
    it('should parse screencast_start with defaults', () => {
      const result = parseCommand(cmd({ id: '1', action: 'screencast_start' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('screencast_start');
      }
    });

    it('should parse screencast_start with all options', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'screencast_start',
          format: 'png',
          quality: 90,
          maxWidth: 1920,
          maxHeight: 1080,
          everyNthFrame: 2,
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.format).toBe('png');
        expect(result.command.quality).toBe(90);
        expect(result.command.maxWidth).toBe(1920);
        expect(result.command.maxHeight).toBe(1080);
        expect(result.command.everyNthFrame).toBe(2);
      }
    });

    it('should reject screencast_start with invalid format', () => {
      const result = parseCommand(cmd({ id: '1', action: 'screencast_start', format: 'gif' }));
      expect(result.success).toBe(false);
    });

    it('should reject screencast_start with quality out of range', () => {
      const result = parseCommand(cmd({ id: '1', action: 'screencast_start', quality: 150 }));
      expect(result.success).toBe(false);
    });

    it('should reject screencast_start with negative maxWidth', () => {
      const result = parseCommand(cmd({ id: '1', action: 'screencast_start', maxWidth: -100 }));
      expect(result.success).toBe(false);
    });

    it('should parse screencast_stop', () => {
      const result = parseCommand(cmd({ id: '1', action: 'screencast_stop' }));
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.action).toBe('screencast_stop');
      }
    });
  });

  describe('input injection', () => {
    describe('input_mouse', () => {
      it('should parse mousePressed event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'mousePressed',
            x: 100,
            y: 200,
            button: 'left',
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.action).toBe('input_mouse');
          expect(result.command.type).toBe('mousePressed');
          expect(result.command.x).toBe(100);
          expect(result.command.y).toBe(200);
          expect(result.command.button).toBe('left');
        }
      });

      it('should parse mouseReleased event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'mouseReleased',
            x: 100,
            y: 200,
          })
        );
        expect(result.success).toBe(true);
      });

      it('should parse mouseMoved event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'mouseMoved',
            x: 150,
            y: 250,
          })
        );
        expect(result.success).toBe(true);
      });

      it('should parse mouseWheel event with deltas', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'mouseWheel',
            x: 100,
            y: 200,
            deltaX: 0,
            deltaY: 100,
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.deltaX).toBe(0);
          expect(result.command.deltaY).toBe(100);
        }
      });

      it('should parse mouse event with modifiers', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'mousePressed',
            x: 100,
            y: 200,
            modifiers: 6, // Ctrl + Meta
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.modifiers).toBe(6);
        }
      });

      it('should parse mouse event with clickCount', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'mousePressed',
            x: 100,
            y: 200,
            clickCount: 2,
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.clickCount).toBe(2);
        }
      });

      it('should reject input_mouse with invalid type', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'invalid',
            x: 100,
            y: 200,
          })
        );
        expect(result.success).toBe(false);
      });

      it('should reject input_mouse without x coordinate', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'mousePressed',
            y: 200,
          })
        );
        expect(result.success).toBe(false);
      });

      it('should reject input_mouse without y coordinate', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_mouse',
            type: 'mousePressed',
            x: 100,
          })
        );
        expect(result.success).toBe(false);
      });
    });

    describe('input_keyboard', () => {
      it('should parse keyDown event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_keyboard',
            type: 'keyDown',
            key: 'Enter',
            code: 'Enter',
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.action).toBe('input_keyboard');
          expect(result.command.type).toBe('keyDown');
          expect(result.command.key).toBe('Enter');
          expect(result.command.code).toBe('Enter');
        }
      });

      it('should parse keyUp event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_keyboard',
            type: 'keyUp',
            key: 'a',
          })
        );
        expect(result.success).toBe(true);
      });

      it('should parse char event with text', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_keyboard',
            type: 'char',
            text: 'hello',
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.text).toBe('hello');
        }
      });

      it('should parse keyboard event with modifiers', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_keyboard',
            type: 'keyDown',
            key: 'c',
            modifiers: 2, // Ctrl
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.modifiers).toBe(2);
        }
      });

      it('should reject input_keyboard with invalid type', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_keyboard',
            type: 'invalid',
          })
        );
        expect(result.success).toBe(false);
      });
    });

    describe('input_touch', () => {
      it('should parse touchStart event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_touch',
            type: 'touchStart',
            touchPoints: [{ x: 100, y: 200 }],
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.action).toBe('input_touch');
          expect(result.command.type).toBe('touchStart');
          expect(result.command.touchPoints).toHaveLength(1);
          expect(result.command.touchPoints[0].x).toBe(100);
          expect(result.command.touchPoints[0].y).toBe(200);
        }
      });

      it('should parse touchEnd event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_touch',
            type: 'touchEnd',
            touchPoints: [],
          })
        );
        expect(result.success).toBe(true);
      });

      it('should parse touchMove event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_touch',
            type: 'touchMove',
            touchPoints: [{ x: 150, y: 250 }],
          })
        );
        expect(result.success).toBe(true);
      });

      it('should parse touchCancel event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_touch',
            type: 'touchCancel',
            touchPoints: [],
          })
        );
        expect(result.success).toBe(true);
      });

      it('should parse multi-touch event', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_touch',
            type: 'touchStart',
            touchPoints: [
              { x: 100, y: 200, id: 0 },
              { x: 300, y: 400, id: 1 },
            ],
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.touchPoints).toHaveLength(2);
        }
      });

      it('should parse touch event with modifiers', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_touch',
            type: 'touchStart',
            touchPoints: [{ x: 100, y: 200 }],
            modifiers: 8, // Shift
          })
        );
        expect(result.success).toBe(true);
        if (result.success) {
          expect(result.command.modifiers).toBe(8);
        }
      });

      it('should reject input_touch with invalid type', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_touch',
            type: 'invalid',
            touchPoints: [],
          })
        );
        expect(result.success).toBe(false);
      });

      it('should reject input_touch without touchPoints', () => {
        const result = parseCommand(
          cmd({
            id: '1',
            action: 'input_touch',
            type: 'touchStart',
          })
        );
        expect(result.success).toBe(false);
      });
    });
  });

  describe('addscript and addstyle', () => {
    it('should parse addscript with content', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'addscript', content: 'console.log("hi")' })
      );
      expect(result.success).toBe(true);
    });

    it('should parse addscript with url', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'addscript', url: 'https://example.com/script.js' })
      );
      expect(result.success).toBe(true);
    });

    it('should reject addscript with neither content nor url', () => {
      const result = parseCommand(cmd({ id: '1', action: 'addscript' }));
      expect(result.success).toBe(false);
    });

    it('should parse addstyle with content', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'addstyle', content: 'body { color: red }' })
      );
      expect(result.success).toBe(true);
    });

    it('should parse addstyle with url', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'addstyle', url: 'https://example.com/style.css' })
      );
      expect(result.success).toBe(true);
    });

    it('should reject addstyle with neither content nor url', () => {
      const result = parseCommand(cmd({ id: '1', action: 'addstyle' }));
      expect(result.success).toBe(false);
    });
  });

  describe('diff', () => {
    it('should parse diff_snapshot with no options', () => {
      const result = parseCommand(cmd({ id: '1', action: 'diff_snapshot' }));
      expect(result.success).toBe(true);
    });

    it('should parse diff_snapshot with baseline', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'diff_snapshot', baseline: 'before.txt' })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.baseline).toBe('before.txt');
      }
    });

    it('should parse diff_snapshot with all options', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'diff_snapshot',
          baseline: 'snap.txt',
          selector: '#main',
          compact: true,
          maxDepth: 3,
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.baseline).toBe('snap.txt');
        expect(result.command.selector).toBe('#main');
        expect(result.command.compact).toBe(true);
        expect(result.command.maxDepth).toBe(3);
      }
    });

    it('should reject diff_snapshot with negative maxDepth', () => {
      const result = parseCommand(cmd({ id: '1', action: 'diff_snapshot', maxDepth: -1 }));
      expect(result.success).toBe(false);
    });

    it('should parse diff_screenshot with baseline', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'diff_screenshot', baseline: 'before.png' })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.baseline).toBe('before.png');
      }
    });

    it('should parse diff_screenshot with all options', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'diff_screenshot',
          baseline: 'before.png',
          output: 'diff.png',
          threshold: 0.2,
          selector: '#hero',
          fullPage: true,
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.baseline).toBe('before.png');
        expect(result.command.output).toBe('diff.png');
        expect(result.command.threshold).toBe(0.2);
        expect(result.command.selector).toBe('#hero');
        expect(result.command.fullPage).toBe(true);
      }
    });

    it('should reject diff_screenshot without baseline', () => {
      const result = parseCommand(cmd({ id: '1', action: 'diff_screenshot' }));
      expect(result.success).toBe(false);
    });

    it('should reject diff_screenshot with threshold out of range', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'diff_screenshot', baseline: 'b.png', threshold: 1.5 })
      );
      expect(result.success).toBe(false);
    });

    it('should parse diff_url with two URLs', () => {
      const result = parseCommand(
        cmd({ id: '1', action: 'diff_url', url1: 'https://a.com', url2: 'https://b.com' })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.url1).toBe('https://a.com');
        expect(result.command.url2).toBe('https://b.com');
      }
    });

    it('should parse diff_url with screenshot and fullPage', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'diff_url',
          url1: 'https://a.com',
          url2: 'https://b.com',
          screenshot: true,
          fullPage: true,
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.screenshot).toBe(true);
        expect(result.command.fullPage).toBe(true);
      }
    });

    it('should parse diff_url with waitUntil', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'diff_url',
          url1: 'https://a.com',
          url2: 'https://b.com',
          waitUntil: 'networkidle',
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.waitUntil).toBe('networkidle');
      }
    });

    it('should reject diff_url without url1', () => {
      const result = parseCommand(cmd({ id: '1', action: 'diff_url', url2: 'https://b.com' }));
      expect(result.success).toBe(false);
    });

    it('should reject diff_url without url2', () => {
      const result = parseCommand(cmd({ id: '1', action: 'diff_url', url1: 'https://a.com' }));
      expect(result.success).toBe(false);
    });

    it('should reject diff_url with invalid waitUntil', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'diff_url',
          url1: 'https://a.com',
          url2: 'https://b.com',
          waitUntil: 'invalid',
        })
      );
      expect(result.success).toBe(false);
    });

    it('should parse diff_url with selector', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'diff_url',
          url1: 'https://a.com',
          url2: 'https://b.com',
          selector: '#main',
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.selector).toBe('#main');
      }
    });

    it('should parse diff_url with all snapshot options', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'diff_url',
          url1: 'https://a.com',
          url2: 'https://b.com',
          selector: '#content',
          compact: true,
          maxDepth: 5,
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect(result.command.selector).toBe('#content');
        expect(result.command.compact).toBe(true);
        expect(result.command.maxDepth).toBe(5);
      }
    });

    it('should reject diff_url with negative maxDepth', () => {
      const result = parseCommand(
        cmd({
          id: '1',
          action: 'diff_url',
          url1: 'https://a.com',
          url2: 'https://b.com',
          maxDepth: -1,
        })
      );
      expect(result.success).toBe(false);
    });
  });

  describe('invalid commands', () => {
    it('should reject unknown action', () => {
      const result = parseCommand(cmd({ id: '1', action: 'unknown' }));
      expect(result.success).toBe(false);
    });

    it('should reject missing id', () => {
      const result = parseCommand(cmd({ action: 'click', selector: '#btn' }));
      expect(result.success).toBe(false);
    });

    it('should reject invalid JSON', () => {
      const result = parseCommand('not json');
      expect(result.success).toBe(false);
    });
  });
});
