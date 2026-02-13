import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

let tempHome: string;

vi.mock('os', async (importOriginal) => {
  const actual = await importOriginal<typeof import('os')>();
  return {
    ...actual,
    homedir: () => tempHome,
  };
});

import {
  getAutoStateFilePath,
  isValidSessionName,
  getSessionsDir,
  safeHeaderMerge,
  listStateFiles,
  cleanupExpiredStates,
} from './state-utils.js';

describe('state-utils', () => {
  beforeEach(() => {
    tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'agent-browser-test-'));
  });

  afterEach(() => {
    fs.rmSync(tempHome, { recursive: true, force: true });
  });

  describe('isValidSessionName', () => {
    it('should accept alphanumeric names', () => {
      expect(isValidSessionName('twitter')).toBe(true);
      expect(isValidSessionName('Twitter123')).toBe(true);
      expect(isValidSessionName('123')).toBe(true);
      expect(isValidSessionName('ABC')).toBe(true);
    });

    it('should accept names with hyphens', () => {
      expect(isValidSessionName('my-session')).toBe(true);
      expect(isValidSessionName('twitter-prod')).toBe(true);
      expect(isValidSessionName('a-b-c-d')).toBe(true);
    });

    it('should accept names with underscores', () => {
      expect(isValidSessionName('my_session')).toBe(true);
      expect(isValidSessionName('twitter_prod')).toBe(true);
      expect(isValidSessionName('a_b_c_d')).toBe(true);
    });

    it('should accept mixed valid characters', () => {
      expect(isValidSessionName('my-session_123')).toBe(true);
      expect(isValidSessionName('Twitter_Prod-v2')).toBe(true);
    });

    it('should reject empty string', () => {
      expect(isValidSessionName('')).toBe(false);
    });

    it('should reject path traversal attempts', () => {
      expect(isValidSessionName('../../../etc/passwd')).toBe(false);
      expect(isValidSessionName('..\\..\\windows\\system32')).toBe(false);
      expect(isValidSessionName('../parent')).toBe(false);
      expect(isValidSessionName('./current')).toBe(false);
    });

    it('should reject names with slashes', () => {
      expect(isValidSessionName('path/to/file')).toBe(false);
      expect(isValidSessionName('path\\to\\file')).toBe(false);
      expect(isValidSessionName('/absolute/path')).toBe(false);
    });

    it('should reject names with spaces', () => {
      expect(isValidSessionName('my session')).toBe(false);
      expect(isValidSessionName(' leading')).toBe(false);
      expect(isValidSessionName('trailing ')).toBe(false);
    });

    it('should reject names with special characters', () => {
      expect(isValidSessionName('session@user')).toBe(false);
      expect(isValidSessionName('session#1')).toBe(false);
      expect(isValidSessionName('session$var')).toBe(false);
      expect(isValidSessionName('session%20')).toBe(false);
      expect(isValidSessionName('session:name')).toBe(false);
      expect(isValidSessionName('session;drop')).toBe(false);
      expect(isValidSessionName("session'sql")).toBe(false);
      expect(isValidSessionName('session"quote')).toBe(false);
      expect(isValidSessionName('session<script>')).toBe(false);
      expect(isValidSessionName('session|pipe')).toBe(false);
    });

    it('should reject names with null bytes', () => {
      expect(isValidSessionName('session\x00name')).toBe(false);
    });

    it('should reject names with newlines', () => {
      expect(isValidSessionName('session\nname')).toBe(false);
      expect(isValidSessionName('session\rname')).toBe(false);
    });

    it('should reject Unicode tricks', () => {
      // Homograph attacks
      expect(isValidSessionName('sеssion')).toBe(false); // Cyrillic 'е'
      expect(isValidSessionName('session\u2024')).toBe(false); // One dot leader
      expect(isValidSessionName('session\u2025')).toBe(false); // Two dot leader
    });
  });

  describe('getAutoStateFilePath', () => {
    it('should return null for empty session name', () => {
      expect(getAutoStateFilePath('', 'default')).toBeNull();
    });

    it('should return valid path for valid inputs', () => {
      const result = getAutoStateFilePath('twitter', 'default');
      expect(result).not.toBeNull();
      expect(result).toContain('twitter-default.json');
      expect(result).toContain('.agent-browser');
      expect(result).toContain('sessions');
    });

    it('should throw error for path traversal in session name', () => {
      expect(() => getAutoStateFilePath('../etc/passwd', 'default')).toThrow(
        /Invalid session name/
      );
    });

    it('should throw error for path traversal in session ID', () => {
      expect(() => getAutoStateFilePath('twitter', '../../../etc/passwd')).toThrow(
        /Invalid session ID/
      );
    });

    it('should throw error for slashes in session name', () => {
      expect(() => getAutoStateFilePath('path/to/file', 'default')).toThrow(/Invalid session name/);
    });

    it('should throw error for slashes in session ID', () => {
      expect(() => getAutoStateFilePath('twitter', 'path/to/file')).toThrow(/Invalid session ID/);
    });

    it('should throw error for special characters in session name', () => {
      expect(() => getAutoStateFilePath('session@evil', 'default')).toThrow(/Invalid session name/);
    });

    it('should throw error for special characters in session ID', () => {
      expect(() => getAutoStateFilePath('twitter', 'id@evil')).toThrow(/Invalid session ID/);
    });

    it('should accept valid session name with hyphens and underscores', () => {
      const result = getAutoStateFilePath('my-session_v2', 'agent_1');
      expect(result).not.toBeNull();
      expect(result).toContain('my-session_v2-agent_1.json');
    });

    // Security: Ensure the resulting path is within the sessions directory
    it('should always produce path within sessions directory', () => {
      const sessionsDir = getSessionsDir();
      const result = getAutoStateFilePath('twitter', 'default');

      expect(result).not.toBeNull();
      expect(result!.startsWith(sessionsDir)).toBe(true);

      // Verify the path is actually within the directory (no traversal)
      const resolvedPath = path.resolve(result!);
      const resolvedSessionsDir = path.resolve(sessionsDir);
      expect(resolvedPath.startsWith(resolvedSessionsDir)).toBe(true);
    });
  });

  describe('safeHeaderMerge', () => {
    it('should merge two header objects', () => {
      const base = { 'Content-Type': 'application/json', Accept: 'text/html' };
      const override = { Authorization: 'Bearer token' };

      const result = safeHeaderMerge(base, override);

      expect(result['Content-Type']).toBe('application/json');
      expect(result['Accept']).toBe('text/html');
      expect(result['Authorization']).toBe('Bearer token');
    });

    it('should allow override to replace base values', () => {
      const base = { 'Content-Type': 'text/plain' };
      const override = { 'Content-Type': 'application/json' };

      const result = safeHeaderMerge(base, override);

      expect(result['Content-Type']).toBe('application/json');
    });

    it('should filter out __proto__ from base', () => {
      const base = { 'Content-Type': 'text/plain', __proto__: 'evil' } as Record<string, string>;
      const override = { Accept: 'text/html' };

      const result = safeHeaderMerge(base, override);

      expect(result['Content-Type']).toBe('text/plain');
      expect(result['Accept']).toBe('text/html');
      expect('__proto__' in result).toBe(false);
      expect(Object.prototype.hasOwnProperty.call(result, '__proto__')).toBe(false);
    });

    it('should filter out __proto__ from override', () => {
      const base = { 'Content-Type': 'text/plain' };
      const override = { Accept: 'text/html', __proto__: 'evil' } as Record<string, string>;

      const result = safeHeaderMerge(base, override);

      expect(result['Content-Type']).toBe('text/plain');
      expect(result['Accept']).toBe('text/html');
      expect('__proto__' in result).toBe(false);
    });

    it('should filter out constructor key', () => {
      const base = { constructor: 'evil' } as Record<string, string>;
      const override = { Accept: 'text/html' };

      const result = safeHeaderMerge(base, override);

      expect(result['Accept']).toBe('text/html');
      expect('constructor' in result).toBe(false);
    });

    it('should filter out prototype key', () => {
      const base = { prototype: 'evil' } as Record<string, string>;
      const override = { Accept: 'text/html' };

      const result = safeHeaderMerge(base, override);

      expect(result['Accept']).toBe('text/html');
      expect('prototype' in result).toBe(false);
    });

    it('should return null-prototype object', () => {
      const base = { 'Content-Type': 'text/plain' };
      const override = {};

      const result = safeHeaderMerge(base, override);

      expect(Object.getPrototypeOf(result)).toBeNull();
    });

    it('should handle empty objects', () => {
      const result = safeHeaderMerge({}, {});
      expect(Object.keys(result)).toHaveLength(0);
    });
  });

  describe('listStateFiles', () => {
    it('should return empty array when directory does not exist', () => {
      const result = listStateFiles();
      expect(Array.isArray(result)).toBe(true);
    });
  });

  describe('cleanupExpiredStates', () => {
    it('should return empty array for 0 days', () => {
      const result = cleanupExpiredStates(0);
      expect(result).toEqual([]);
    });

    it('should return empty array for negative days', () => {
      const result = cleanupExpiredStates(-5);
      expect(result).toEqual([]);
    });
  });
});
