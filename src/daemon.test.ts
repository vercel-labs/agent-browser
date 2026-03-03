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
