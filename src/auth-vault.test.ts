import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

let tempHome: string;

vi.mock('node:os', async (importOriginal) => {
  const actual = await importOriginal<typeof import('os')>();
  return {
    ...actual,
    default: {
      ...actual,
      homedir: () => tempHome,
    },
    homedir: () => tempHome,
  };
});

import {
  saveAuthProfile,
  getAuthProfile,
  getAuthProfileMeta,
  listAuthProfiles,
  deleteAuthProfile,
  updateLastLogin,
} from './auth-vault.js';

describe('auth-vault', () => {
  beforeEach(() => {
    tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'agent-browser-auth-test-'));
    delete process.env.AGENT_BROWSER_ENCRYPTION_KEY;
  });

  afterEach(() => {
    try {
      fs.rmSync(tempHome, { recursive: true, force: true });
    } catch {
      // ignore cleanup errors
    }
  });

  function cleanAuthDir() {
    const authDir = path.join(tempHome, '.agent-browser', 'auth');
    if (fs.existsSync(authDir)) {
      for (const f of fs.readdirSync(authDir)) {
        fs.unlinkSync(path.join(authDir, f));
      }
    }
  }

  describe('saveAuthProfile', () => {
    it('should save a new profile', () => {
      const result = saveAuthProfile({
        name: 'github',
        url: 'https://github.com/login',
        username: 'user',
        password: 'pass',
      });

      expect(result.name).toBe('github');
      expect(result.url).toBe('https://github.com/login');
      expect(result.username).toBe('user');
      expect(result.updated).toBe(false);
      expect(result.createdAt).toBeTruthy();
    });

    it('should mark as updated when overwriting', () => {
      saveAuthProfile({
        name: 'github',
        url: 'https://github.com/login',
        username: 'user1',
        password: 'pass1',
      });

      const result = saveAuthProfile({
        name: 'github',
        url: 'https://github.com/login',
        username: 'user2',
        password: 'pass2',
      });

      expect(result.updated).toBe(true);
      expect(result.username).toBe('user2');
    });

    it('should preserve createdAt on update', () => {
      const first = saveAuthProfile({
        name: 'github',
        url: 'https://github.com/login',
        username: 'user',
        password: 'pass',
      });

      const second = saveAuthProfile({
        name: 'github',
        url: 'https://github.com/login',
        username: 'user2',
        password: 'pass2',
      });

      expect(second.createdAt).toBe(first.createdAt);
    });

    it('should save with custom selectors', () => {
      saveAuthProfile({
        name: 'myapp',
        url: 'https://example.com/login',
        username: 'user',
        password: 'pass',
        usernameSelector: '#email',
        passwordSelector: '#password',
        submitSelector: 'button.login',
      });

      const profile = getAuthProfile('myapp');
      expect(profile).not.toBeNull();
      expect(profile!.usernameSelector).toBe('#email');
      expect(profile!.passwordSelector).toBe('#password');
      expect(profile!.submitSelector).toBe('button.login');
    });

    it('should reject invalid profile names', () => {
      expect(() =>
        saveAuthProfile({
          name: '../escape',
          url: 'https://example.com',
          username: 'user',
          password: 'pass',
        })
      ).toThrow('only alphanumeric');
    });
  });

  describe('getAuthProfile', () => {
    it('should return null for non-existent profile', () => {
      expect(getAuthProfile('nonexistent')).toBeNull();
    });

    it('should return full profile with password', () => {
      saveAuthProfile({
        name: 'test',
        url: 'https://example.com',
        username: 'user',
        password: 'secret',
      });

      const profile = getAuthProfile('test');
      expect(profile).not.toBeNull();
      expect(profile!.password).toBe('secret');
    });
  });

  describe('getAuthProfileMeta', () => {
    it('should return metadata without password', () => {
      saveAuthProfile({
        name: 'test',
        url: 'https://example.com',
        username: 'user',
        password: 'secret',
      });

      const meta = getAuthProfileMeta('test');
      expect(meta).not.toBeNull();
      expect(meta!.name).toBe('test');
      expect(meta!.username).toBe('user');
      expect((meta as Record<string, unknown>).password).toBeUndefined();
    });

    it('should return null for non-existent profile', () => {
      expect(getAuthProfileMeta('nonexistent')).toBeNull();
    });
  });

  describe('listAuthProfiles', () => {
    it('should return empty array when no profiles', () => {
      cleanAuthDir();
      expect(listAuthProfiles()).toEqual([]);
    });

    it('should list all saved profiles', () => {
      cleanAuthDir();
      saveAuthProfile({
        name: 'github',
        url: 'https://github.com/login',
        username: 'user1',
        password: 'pass1',
      });
      saveAuthProfile({
        name: 'gitlab',
        url: 'https://gitlab.com/login',
        username: 'user2',
        password: 'pass2',
      });

      const profiles = listAuthProfiles();
      expect(profiles).toHaveLength(2);
      const names = profiles.map((p) => p.name).sort();
      expect(names).toEqual(['github', 'gitlab']);
    });
  });

  describe('deleteAuthProfile', () => {
    it('should delete an existing profile', () => {
      saveAuthProfile({
        name: 'test',
        url: 'https://example.com',
        username: 'user',
        password: 'pass',
      });

      expect(deleteAuthProfile('test')).toBe(true);
      expect(getAuthProfile('test')).toBeNull();
    });

    it('should return false for non-existent profile', () => {
      expect(deleteAuthProfile('nonexistent')).toBe(false);
    });
  });

  describe('updateLastLogin', () => {
    it('should update lastLoginAt timestamp', () => {
      saveAuthProfile({
        name: 'test',
        url: 'https://example.com',
        username: 'user',
        password: 'pass',
      });

      const metaBefore = getAuthProfileMeta('test');
      expect(metaBefore!.lastLoginAt).toBeUndefined();

      updateLastLogin('test');

      const metaAfter = getAuthProfileMeta('test');
      expect(metaAfter!.lastLoginAt).toBeTruthy();
    });
  });

  describe('auto-generated encryption key', () => {
    it('should auto-create key file and encrypt profile when no env var is set', () => {
      delete process.env.AGENT_BROWSER_ENCRYPTION_KEY;

      saveAuthProfile({
        name: 'autokey',
        url: 'https://example.com',
        username: 'user',
        password: 'secret',
      });

      const keyFilePath = path.join(tempHome, '.agent-browser', '.encryption-key');
      expect(fs.existsSync(keyFilePath)).toBe(true);

      const keyHex = fs.readFileSync(keyFilePath, 'utf-8').trim();
      expect(keyHex).toMatch(/^[a-f0-9]{64}$/);

      const profilePath = path.join(tempHome, '.agent-browser', 'auth', 'autokey.json');
      const raw = JSON.parse(fs.readFileSync(profilePath, 'utf-8'));
      expect(raw.encrypted).toBe(true);
      expect(raw.iv).toBeTruthy();
    });

    it('should read back profile using auto-generated key', () => {
      delete process.env.AGENT_BROWSER_ENCRYPTION_KEY;

      saveAuthProfile({
        name: 'readback',
        url: 'https://example.com',
        username: 'user',
        password: 'secret123',
      });

      const profile = getAuthProfile('readback');
      expect(profile).not.toBeNull();
      expect(profile!.password).toBe('secret123');
    });
  });
});
