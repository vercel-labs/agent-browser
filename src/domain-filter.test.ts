import { describe, it, expect } from 'vitest';
import { isDomainAllowed, parseDomainList, buildWebSocketFilterScript } from './domain-filter.js';

describe('domain-filter', () => {
  describe('isDomainAllowed', () => {
    it('should match exact domains', () => {
      expect(isDomainAllowed('example.com', ['example.com'])).toBe(true);
      expect(isDomainAllowed('github.com', ['github.com'])).toBe(true);
    });

    it('should reject non-matching domains', () => {
      expect(isDomainAllowed('evil.com', ['example.com'])).toBe(false);
      expect(isDomainAllowed('notexample.com', ['example.com'])).toBe(false);
    });

    it('should match wildcard patterns', () => {
      expect(isDomainAllowed('sub.example.com', ['*.example.com'])).toBe(true);
      expect(isDomainAllowed('deep.sub.example.com', ['*.example.com'])).toBe(true);
    });

    it('should match bare domain against wildcard pattern', () => {
      expect(isDomainAllowed('example.com', ['*.example.com'])).toBe(true);
    });

    it('should reject non-matching wildcard patterns', () => {
      expect(isDomainAllowed('example.org', ['*.example.com'])).toBe(false);
      expect(isDomainAllowed('evil.com', ['*.example.com'])).toBe(false);
    });

    it('should return false for empty allowlist', () => {
      expect(isDomainAllowed('example.com', [])).toBe(false);
    });

    it('should match against multiple patterns', () => {
      const patterns = ['example.com', '*.github.com', 'vercel.app'];
      expect(isDomainAllowed('example.com', patterns)).toBe(true);
      expect(isDomainAllowed('api.github.com', patterns)).toBe(true);
      expect(isDomainAllowed('vercel.app', patterns)).toBe(true);
      expect(isDomainAllowed('evil.com', patterns)).toBe(false);
    });

    it('should not partially match domain suffixes without wildcard', () => {
      expect(isDomainAllowed('sub.example.com', ['example.com'])).toBe(false);
    });
  });

  describe('parseDomainList', () => {
    it('should split comma-separated domains', () => {
      expect(parseDomainList('a.com,b.com')).toEqual(['a.com', 'b.com']);
    });

    it('should trim whitespace', () => {
      expect(parseDomainList(' a.com , b.com ')).toEqual(['a.com', 'b.com']);
    });

    it('should lowercase domains', () => {
      expect(parseDomainList('Example.COM,GitHub.Com')).toEqual(['example.com', 'github.com']);
    });

    it('should filter empty entries', () => {
      expect(parseDomainList('a.com,,b.com,')).toEqual(['a.com', 'b.com']);
    });

    it('should handle empty string', () => {
      expect(parseDomainList('')).toEqual([]);
    });

    it('should preserve wildcard prefixes', () => {
      expect(parseDomainList('*.example.com')).toEqual(['*.example.com']);
    });
  });

  describe('buildWebSocketFilterScript', () => {
    it('should produce a valid JavaScript IIFE', () => {
      const script = buildWebSocketFilterScript(['example.com', '*.github.com']);
      expect(script).toContain('_allowedDomains');
      expect(script).toContain('"example.com"');
      expect(script).toContain('"*.github.com"');
    });

    it('should embed the domain list as JSON', () => {
      const script = buildWebSocketFilterScript(['a.com']);
      expect(script).toContain('["a.com"]');
    });

    it('should include WebSocket, EventSource, and sendBeacon patches', () => {
      const script = buildWebSocketFilterScript(['a.com']);
      expect(script).toContain('WebSocket');
      expect(script).toContain('EventSource');
      expect(script).toContain('SecurityError');
      expect(script).toContain('sendBeacon');
    });

    it('should handle empty allowlist', () => {
      const script = buildWebSocketFilterScript([]);
      expect(script).toContain('[]');
    });

    it('should include domain matching logic consistent with isDomainAllowed', () => {
      const script = buildWebSocketFilterScript(['*.example.com']);
      expect(script).toContain('_isDomainAllowed');
      expect(script).toContain('slice(1)');
      expect(script).toContain('slice(2)');
    });
  });
});
