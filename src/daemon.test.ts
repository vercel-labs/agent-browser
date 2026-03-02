import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import * as os from 'os';
import * as path from 'path';
import * as net from 'net';
import { EventEmitter } from 'events';
import { getSocketDir, safeWrite } from './daemon.js';

/**
 * HTTP request detection pattern used in daemon.ts to prevent cross-origin attacks.
 * This pattern detects HTTP method prefixes that browsers must send when using fetch().
 */
const HTTP_REQUEST_PATTERN = /^(GET|POST|PUT|DELETE|HEAD|OPTIONS|PATCH|CONNECT|TRACE)\s/i;

describe('HTTP request detection (security)', () => {
  it('should detect POST requests from fetch()', () => {
    const httpRequest = 'POST / HTTP/1.1\r\nHost: 127.0.0.1:51234\r\n';
    expect(HTTP_REQUEST_PATTERN.test(httpRequest.trimStart())).toBe(true);
  });

  it('should detect GET requests', () => {
    expect(HTTP_REQUEST_PATTERN.test('GET / HTTP/1.1')).toBe(true);
  });

  it('should detect OPTIONS preflight requests', () => {
    expect(HTTP_REQUEST_PATTERN.test('OPTIONS / HTTP/1.1')).toBe(true);
  });

  it('should NOT detect valid JSON commands', () => {
    const jsonCommand = '{"id":"1","action":"navigate","url":"https://example.com"}';
    expect(HTTP_REQUEST_PATTERN.test(jsonCommand.trimStart())).toBe(false);
  });

  it('should NOT detect JSON with leading whitespace', () => {
    const jsonCommand = '  {"id":"1","action":"click","selector":"button"}';
    expect(HTTP_REQUEST_PATTERN.test(jsonCommand.trimStart())).toBe(false);
  });

  it('should be case insensitive for HTTP methods', () => {
    expect(HTTP_REQUEST_PATTERN.test('post / HTTP/1.1')).toBe(true);
    expect(HTTP_REQUEST_PATTERN.test('Post / HTTP/1.1')).toBe(true);
  });
});

describe('getSocketDir', () => {
  const originalEnv = { ...process.env };

  beforeEach(() => {
    // Clear relevant env vars before each test
    delete process.env.AGENT_BROWSER_SOCKET_DIR;
    delete process.env.XDG_RUNTIME_DIR;
  });

  afterEach(() => {
    // Restore original env
    process.env = { ...originalEnv };
  });

  describe('AGENT_BROWSER_SOCKET_DIR', () => {
    it('should use custom path when set', () => {
      process.env.AGENT_BROWSER_SOCKET_DIR = '/custom/socket/path';
      expect(getSocketDir()).toBe('/custom/socket/path');
    });

    it('should ignore empty string', () => {
      process.env.AGENT_BROWSER_SOCKET_DIR = '';
      const result = getSocketDir();
      expect(result).toContain('.agent-browser');
    });

    it('should take priority over XDG_RUNTIME_DIR', () => {
      process.env.AGENT_BROWSER_SOCKET_DIR = '/custom/path';
      process.env.XDG_RUNTIME_DIR = '/run/user/1000';
      expect(getSocketDir()).toBe('/custom/path');
    });
  });

  describe('XDG_RUNTIME_DIR', () => {
    it('should use when AGENT_BROWSER_SOCKET_DIR is not set', () => {
      process.env.XDG_RUNTIME_DIR = '/run/user/1000';
      expect(getSocketDir()).toBe('/run/user/1000/agent-browser');
    });

    it('should ignore empty string', () => {
      process.env.AGENT_BROWSER_SOCKET_DIR = '';
      process.env.XDG_RUNTIME_DIR = '';
      const result = getSocketDir();
      expect(result).toContain('.agent-browser');
    });
  });

  describe('fallback', () => {
    it('should use home directory when env vars are not set', () => {
      const result = getSocketDir();
      const expected = path.join(os.homedir(), '.agent-browser');
      expect(result).toBe(expected);
    });
  });
});

function createMockSocket(opts: { destroyed?: boolean; writeReturns?: boolean } = {}) {
  const emitter = new EventEmitter();
  const socket = Object.assign(emitter, {
    destroyed: opts.destroyed ?? false,
    write: vi.fn().mockReturnValue(opts.writeReturns ?? true),
    removeListener: emitter.removeListener.bind(emitter),
  });
  return socket as unknown as net.Socket;
}

/**
 * AGENT_BROWSER_HEADERS env var parsing.
 *
 * In daemon.ts the headers are parsed with JSON.parse() and validated at
 * runtime (must be a plain object with string values). Here we replicate the
 * same validation contract so the test stays aligned with the daemon
 * implementation.
 */
describe('AGENT_BROWSER_HEADERS env parsing', () => {
  function parseHeaders(raw: string | undefined): Record<string, string> | undefined {
    if (!raw) return undefined;
    try {
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        const validated: Record<string, string> = {};
        for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
          if (typeof value === 'string') {
            validated[key] = value;
          }
        }
        return Object.keys(validated).length > 0 ? validated : undefined;
      }
      return undefined;
    } catch {
      return undefined;
    }
  }

  it('should parse valid JSON headers', () => {
    const raw = '{"Authorization":"Bearer tok","X-Custom":"val"}';
    expect(parseHeaders(raw)).toEqual({
      Authorization: 'Bearer tok',
      'X-Custom': 'val',
    });
  });

  it('should return undefined for invalid JSON', () => {
    expect(parseHeaders('{bad json')).toBeUndefined();
  });

  it('should return undefined when env var is not set', () => {
    expect(parseHeaders(undefined)).toBeUndefined();
  });

  it('should return undefined for empty object', () => {
    expect(parseHeaders('{}')).toBeUndefined();
  });

  it('should reject arrays', () => {
    expect(parseHeaders('["a","b"]')).toBeUndefined();
  });

  it('should reject non-object primitives', () => {
    expect(parseHeaders('"just a string"')).toBeUndefined();
    expect(parseHeaders('42')).toBeUndefined();
    expect(parseHeaders('true')).toBeUndefined();
    expect(parseHeaders('null')).toBeUndefined();
  });

  it('should strip non-string values and keep valid ones', () => {
    const raw = '{"good":"value","bad":123,"also_good":"ok"}';
    expect(parseHeaders(raw)).toEqual({
      good: 'value',
      also_good: 'ok',
    });
  });

  it('should return undefined when all values are non-string', () => {
    const raw = '{"a":1,"b":true,"c":null}';
    expect(parseHeaders(raw)).toBeUndefined();
  });
});

describe('safeWrite', () => {
  it('should resolve immediately when socket.write returns true', async () => {
    const socket = createMockSocket({ writeReturns: true });
    await safeWrite(socket, 'hello\n');
    expect(socket.write).toHaveBeenCalledWith('hello\n');
  });

  it('should resolve immediately when socket is already destroyed', async () => {
    const socket = createMockSocket({ destroyed: true });
    await safeWrite(socket, 'hello\n');
    expect(socket.write).not.toHaveBeenCalled();
  });

  it('should wait for drain event when socket.write returns false', async () => {
    const socket = createMockSocket({ writeReturns: false });
    const promise = safeWrite(socket, 'big payload');

    // Simulate drain after a tick
    setTimeout(() => socket.emit('drain'), 0);
    await promise;

    expect(socket.write).toHaveBeenCalledWith('big payload');
  });

  it('should reject on socket error while waiting for drain', async () => {
    const socket = createMockSocket({ writeReturns: false });
    const promise = safeWrite(socket, 'data');

    setTimeout(() => socket.emit('error', new Error('connection reset')), 0);
    await expect(promise).rejects.toThrow('connection reset');
  });

  it('should resolve on socket close while waiting for drain', async () => {
    const socket = createMockSocket({ writeReturns: false });
    const promise = safeWrite(socket, 'data');

    setTimeout(() => socket.emit('close'), 0);
    await promise;
  });

  it('should clean up listeners after drain resolves', async () => {
    const socket = createMockSocket({ writeReturns: false });
    const promise = safeWrite(socket, 'data');

    setTimeout(() => socket.emit('drain'), 0);
    await promise;

    expect(socket.listenerCount('drain')).toBe(0);
    expect(socket.listenerCount('error')).toBe(0);
    expect(socket.listenerCount('close')).toBe(0);
  });
});
