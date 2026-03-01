import { describe, it, expect, beforeEach } from 'vitest';
import { isAllowedOrigin, setAllowedOrigins } from './stream-server.js';

describe('isAllowedOrigin', () => {
  describe('allowed origins', () => {
    it('should allow connections with no origin (CLI tools)', () => {
      expect(isAllowedOrigin(undefined)).toBe(true);
    });

    it('should allow empty string origin', () => {
      expect(isAllowedOrigin('')).toBe(true);
    });

    it('should allow file:// origins', () => {
      expect(isAllowedOrigin('file:///path/to/viewer.html')).toBe(true);
      expect(isAllowedOrigin('file:///C:/Users/user/viewer.html')).toBe(true);
    });

    it('should allow http://localhost origins', () => {
      expect(isAllowedOrigin('http://localhost')).toBe(true);
      expect(isAllowedOrigin('http://localhost:3000')).toBe(true);
      expect(isAllowedOrigin('http://localhost:9223')).toBe(true);
      expect(isAllowedOrigin('http://localhost:8080')).toBe(true);
    });

    it('should allow https://localhost origins', () => {
      expect(isAllowedOrigin('https://localhost')).toBe(true);
      expect(isAllowedOrigin('https://localhost:3000')).toBe(true);
    });

    it('should allow http://127.0.0.1 origins', () => {
      expect(isAllowedOrigin('http://127.0.0.1')).toBe(true);
      expect(isAllowedOrigin('http://127.0.0.1:3000')).toBe(true);
      expect(isAllowedOrigin('http://127.0.0.1:9223')).toBe(true);
    });

    it('should allow IPv6 loopback origins', () => {
      expect(isAllowedOrigin('http://[::1]')).toBe(true);
      expect(isAllowedOrigin('http://[::1]:3000')).toBe(true);
    });

    it('should allow vscode-webview:// origins', () => {
      expect(isAllowedOrigin('vscode-webview://extension-id')).toBe(true);
      expect(isAllowedOrigin('vscode-webview://some.extension/path')).toBe(true);
    });
  });

  describe('rejected origins', () => {
    it('should reject remote origins', () => {
      expect(isAllowedOrigin('https://evil.com')).toBe(false);
      expect(isAllowedOrigin('http://attacker.local:8080')).toBe(false);
      expect(isAllowedOrigin('https://example.com')).toBe(false);
    });

    it('should reject origins with localhost in path but not hostname', () => {
      expect(isAllowedOrigin('https://evil.com/localhost')).toBe(false);
    });

    it('should reject origins that look like localhost but are not', () => {
      expect(isAllowedOrigin('http://localhost.evil.com')).toBe(false);
      expect(isAllowedOrigin('http://not-localhost:3000')).toBe(false);
    });

    it('should reject invalid origin URLs', () => {
      expect(isAllowedOrigin('not-a-url')).toBe(false);
      expect(isAllowedOrigin('://missing-scheme')).toBe(false);
    });

    it('should not allow vscode-webview prefix without ://', () => {
      expect(isAllowedOrigin('vscode-webview-fake://evil')).toBe(false);
    });

    it('should not allow partial file:// prefix spoofing', () => {
      expect(isAllowedOrigin('file-evil://path')).toBe(false);
    });
  });

  describe('custom allowed origins via setAllowedOrigins', () => {
    beforeEach(() => {
      setAllowedOrigins([]);
    });

    it('should allow exact custom origin', () => {
      setAllowedOrigins(['https://my-app.example.com']);
      expect(isAllowedOrigin('https://my-app.example.com')).toBe(true);
      expect(isAllowedOrigin('https://other.example.com')).toBe(false);
    });

    it('should allow scheme-only prefix (ending with ://)', () => {
      setAllowedOrigins(['custom-scheme://']);
      expect(isAllowedOrigin('custom-scheme://anything')).toBe(true);
      expect(isAllowedOrigin('custom-scheme://another/path')).toBe(true);
      expect(isAllowedOrigin('custom-scheme-fake://bad')).toBe(false);
    });

    it('should allow origin with sub-path separator', () => {
      setAllowedOrigins(['https://my-app.example.com']);
      expect(isAllowedOrigin('https://my-app.example.com/path')).toBe(true);
      expect(isAllowedOrigin('https://my-app.example.com:8080')).toBe(true);
    });

    it('should not allow partial hostname matches', () => {
      setAllowedOrigins(['https://my-app.example.com']);
      expect(isAllowedOrigin('https://my-app.example.com.evil.com')).toBe(false);
    });

    it('should trim and filter empty entries', () => {
      setAllowedOrigins(['  https://trimmed.com  ', '', '  ']);
      expect(isAllowedOrigin('https://trimmed.com')).toBe(true);
    });

    it('should handle multiple custom origins', () => {
      setAllowedOrigins(['https://app1.com', 'https://app2.com']);
      expect(isAllowedOrigin('https://app1.com')).toBe(true);
      expect(isAllowedOrigin('https://app2.com')).toBe(true);
      expect(isAllowedOrigin('https://app3.com')).toBe(false);
    });

    it('should reset when called with empty array', () => {
      setAllowedOrigins(['https://allowed.com']);
      expect(isAllowedOrigin('https://allowed.com')).toBe(true);
      setAllowedOrigins([]);
      expect(isAllowedOrigin('https://allowed.com')).toBe(false);
    });
  });
});
