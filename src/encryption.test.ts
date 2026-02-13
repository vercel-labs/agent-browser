import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as crypto from 'crypto';
import {
  encryptData,
  decryptData,
  getEncryptionKey,
  isEncryptedPayload,
  ENCRYPTION_KEY_ENV,
  IV_LENGTH,
  type EncryptedPayload,
} from './encryption.js';

// Generate a valid test key (256 bits = 32 bytes = 64 hex chars)
const generateTestKey = () => crypto.randomBytes(32);
const generateTestKeyHex = () => crypto.randomBytes(32).toString('hex');

describe('encryption', () => {
  describe('encryptData / decryptData', () => {
    it('should round-trip encrypt and decrypt data correctly', () => {
      const key = generateTestKey();
      const plaintext = 'Hello, World! This is a test message.';

      const encrypted = encryptData(plaintext, key);
      const decrypted = decryptData(encrypted, key);

      expect(decrypted).toBe(plaintext);
    });

    it('should round-trip with complex JSON data', () => {
      const key = generateTestKey();
      const data = {
        cookies: [{ name: 'session', value: 'abc123', domain: '.example.com' }],
        localStorage: { theme: 'dark', userId: '12345' },
        sessionStorage: {},
      };
      const plaintext = JSON.stringify(data);

      const encrypted = encryptData(plaintext, key);
      const decrypted = decryptData(encrypted, key);

      expect(JSON.parse(decrypted)).toEqual(data);
    });

    it('should round-trip with empty string', () => {
      const key = generateTestKey();
      const plaintext = '';

      const encrypted = encryptData(plaintext, key);
      const decrypted = decryptData(encrypted, key);

      expect(decrypted).toBe(plaintext);
    });

    it('should round-trip with unicode characters', () => {
      const key = generateTestKey();
      const plaintext = '你好世界 🌍 Привет мир émojis: 🔐🔑';

      const encrypted = encryptData(plaintext, key);
      const decrypted = decryptData(encrypted, key);

      expect(decrypted).toBe(plaintext);
    });

    it('should round-trip with large data', () => {
      const key = generateTestKey();
      const plaintext = 'x'.repeat(100000); // 100KB of data

      const encrypted = encryptData(plaintext, key);
      const decrypted = decryptData(encrypted, key);

      expect(decrypted).toBe(plaintext);
    });
  });

  describe('IV uniqueness', () => {
    it('should generate different IVs for each encryption', () => {
      const key = generateTestKey();
      const plaintext = 'Same message encrypted twice';

      const encrypted1 = encryptData(plaintext, key);
      const encrypted2 = encryptData(plaintext, key);

      // IVs should be different
      expect(encrypted1.iv).not.toBe(encrypted2.iv);

      // Ciphertext should also be different due to different IVs
      expect(encrypted1.data).not.toBe(encrypted2.data);

      // Both should decrypt to the same plaintext
      expect(decryptData(encrypted1, key)).toBe(plaintext);
      expect(decryptData(encrypted2, key)).toBe(plaintext);
    });

    it('should have correct IV length', () => {
      const key = generateTestKey();
      const encrypted = encryptData('test', key);

      const ivBuffer = Buffer.from(encrypted.iv, 'base64');
      expect(ivBuffer.length).toBe(IV_LENGTH);
    });
  });

  describe('authentication (tamper detection)', () => {
    it('should throw error when auth tag is tampered', () => {
      const key = generateTestKey();
      const plaintext = 'Sensitive data';

      const encrypted = encryptData(plaintext, key);

      // Tamper with the auth tag
      const tamperedAuthTag = Buffer.from(encrypted.authTag, 'base64');
      tamperedAuthTag[0] ^= 0xff; // Flip bits
      const tamperedPayload: EncryptedPayload = {
        ...encrypted,
        authTag: tamperedAuthTag.toString('base64'),
      };

      expect(() => decryptData(tamperedPayload, key)).toThrow();
    });

    it('should throw error when ciphertext is tampered', () => {
      const key = generateTestKey();
      const plaintext = 'Sensitive data';

      const encrypted = encryptData(plaintext, key);

      // Tamper with the ciphertext
      const tamperedData = Buffer.from(encrypted.data, 'base64');
      tamperedData[0] ^= 0xff; // Flip bits
      const tamperedPayload: EncryptedPayload = {
        ...encrypted,
        data: tamperedData.toString('base64'),
      };

      expect(() => decryptData(tamperedPayload, key)).toThrow();
    });

    it('should throw error when IV is tampered', () => {
      const key = generateTestKey();
      const plaintext = 'Sensitive data';

      const encrypted = encryptData(plaintext, key);

      // Tamper with the IV
      const tamperedIv = Buffer.from(encrypted.iv, 'base64');
      tamperedIv[0] ^= 0xff; // Flip bits
      const tamperedPayload: EncryptedPayload = {
        ...encrypted,
        iv: tamperedIv.toString('base64'),
      };

      expect(() => decryptData(tamperedPayload, key)).toThrow();
    });
  });

  describe('wrong key handling', () => {
    it('should throw error when decrypting with wrong key', () => {
      const key1 = generateTestKey();
      const key2 = generateTestKey();
      const plaintext = 'Sensitive data';

      const encrypted = encryptData(plaintext, key1);

      // Try to decrypt with a different key
      expect(() => decryptData(encrypted, key2)).toThrow();
    });

    it('should throw error when key is partially wrong', () => {
      const key = generateTestKey();
      const plaintext = 'Sensitive data';

      const encrypted = encryptData(plaintext, key);

      // Create a key with one byte different
      const wrongKey = Buffer.from(key);
      wrongKey[0] ^= 0xff;

      expect(() => decryptData(encrypted, wrongKey)).toThrow();
    });
  });

  describe('malformed payload detection', () => {
    it('should throw error for empty IV', () => {
      const key = generateTestKey();
      const encrypted = encryptData('test', key);

      const malformed: EncryptedPayload = {
        ...encrypted,
        iv: '',
      };

      expect(() => decryptData(malformed, key)).toThrow();
    });

    it('should throw error for empty auth tag', () => {
      const key = generateTestKey();
      const encrypted = encryptData('test', key);

      const malformed: EncryptedPayload = {
        ...encrypted,
        authTag: '',
      };

      expect(() => decryptData(malformed, key)).toThrow();
    });

    it('should throw error for invalid base64 in IV', () => {
      const key = generateTestKey();
      const encrypted = encryptData('test', key);

      const malformed: EncryptedPayload = {
        ...encrypted,
        iv: '!!!not-valid-base64!!!',
      };

      expect(() => decryptData(malformed, key)).toThrow();
    });

    it('should throw error for truncated auth tag', () => {
      const key = generateTestKey();
      const encrypted = encryptData('test', key);

      // Truncate auth tag to just 4 bytes (minimum allowed, but wrong value)
      // This won't match the actual tag, so authentication will fail
      const truncatedTag = crypto.randomBytes(4); // Random 4 bytes won't match
      const malformed: EncryptedPayload = {
        ...encrypted,
        authTag: truncatedTag.toString('base64'),
      };

      // Note: With Node.js deprecation warning, very short tags may still be
      // accepted but will fail authentication during decipher.final()
      expect(() => decryptData(malformed, key)).toThrow();
    });

    it('should throw error for completely wrong auth tag length', () => {
      const key = generateTestKey();
      const encrypted = encryptData('test', key);

      // Use a completely wrong auth tag (right length but wrong value)
      const wrongTag = crypto.randomBytes(16); // Same length as real tag
      const malformed: EncryptedPayload = {
        ...encrypted,
        authTag: wrongTag.toString('base64'),
      };

      expect(() => decryptData(malformed, key)).toThrow();
    });
  });

  describe('getEncryptionKey', () => {
    const originalEnv = process.env[ENCRYPTION_KEY_ENV];

    afterEach(() => {
      // Restore original env
      if (originalEnv !== undefined) {
        process.env[ENCRYPTION_KEY_ENV] = originalEnv;
      } else {
        delete process.env[ENCRYPTION_KEY_ENV];
      }
    });

    it('should return null when env var is not set', () => {
      delete process.env[ENCRYPTION_KEY_ENV];
      expect(getEncryptionKey()).toBeNull();
    });

    it('should return null for empty string', () => {
      process.env[ENCRYPTION_KEY_ENV] = '';
      expect(getEncryptionKey()).toBeNull();
    });

    it('should return null for invalid hex (too short)', () => {
      process.env[ENCRYPTION_KEY_ENV] = 'abc123'; // Only 6 chars, need 64
      expect(getEncryptionKey()).toBeNull();
    });

    it('should return null for invalid hex (too long)', () => {
      process.env[ENCRYPTION_KEY_ENV] = 'a'.repeat(128); // 128 chars, need 64
      expect(getEncryptionKey()).toBeNull();
    });

    it('should return null for non-hex characters', () => {
      process.env[ENCRYPTION_KEY_ENV] = 'g'.repeat(64); // 'g' is not hex
      expect(getEncryptionKey()).toBeNull();
    });

    it('should return valid key buffer for correct hex string', () => {
      const keyHex = generateTestKeyHex();
      process.env[ENCRYPTION_KEY_ENV] = keyHex;

      const key = getEncryptionKey();
      expect(key).not.toBeNull();
      expect(key).toBeInstanceOf(Buffer);
      expect(key!.length).toBe(32); // 256 bits
      expect(key!.toString('hex')).toBe(keyHex.toLowerCase());
    });

    it('should accept uppercase hex', () => {
      const keyHex = generateTestKeyHex().toUpperCase();
      process.env[ENCRYPTION_KEY_ENV] = keyHex;

      const key = getEncryptionKey();
      expect(key).not.toBeNull();
      expect(key!.length).toBe(32);
    });

    it('should accept mixed case hex', () => {
      const keyHex = generateTestKeyHex();
      const mixedCase = keyHex
        .split('')
        .map((c, i) => (i % 2 === 0 ? c.toUpperCase() : c.toLowerCase()))
        .join('');
      process.env[ENCRYPTION_KEY_ENV] = mixedCase;

      const key = getEncryptionKey();
      expect(key).not.toBeNull();
      expect(key!.length).toBe(32);
    });
  });

  describe('isEncryptedPayload', () => {
    it('should return true for valid encrypted payload', () => {
      const key = generateTestKey();
      const encrypted = encryptData('test', key);

      expect(isEncryptedPayload(encrypted)).toBe(true);
    });

    it('should return false for null', () => {
      expect(isEncryptedPayload(null)).toBe(false);
    });

    it('should return false for undefined', () => {
      expect(isEncryptedPayload(undefined)).toBe(false);
    });

    it('should return false for plain object without encrypted flag', () => {
      expect(isEncryptedPayload({ data: 'test' })).toBe(false);
    });

    it('should return false for object with encrypted: false', () => {
      expect(
        isEncryptedPayload({
          encrypted: false,
          version: 1,
          iv: 'test',
          authTag: 'test',
          data: 'test',
        })
      ).toBe(false);
    });

    it('should return false for object missing version', () => {
      expect(
        isEncryptedPayload({
          encrypted: true,
          iv: 'test',
          authTag: 'test',
          data: 'test',
        })
      ).toBe(false);
    });

    it('should return false for object missing iv', () => {
      expect(
        isEncryptedPayload({
          encrypted: true,
          version: 1,
          authTag: 'test',
          data: 'test',
        })
      ).toBe(false);
    });

    it('should return false for object missing authTag', () => {
      expect(
        isEncryptedPayload({
          encrypted: true,
          version: 1,
          iv: 'test',
          data: 'test',
        })
      ).toBe(false);
    });

    it('should return false for object missing data', () => {
      expect(
        isEncryptedPayload({
          encrypted: true,
          version: 1,
          iv: 'test',
          authTag: 'test',
        })
      ).toBe(false);
    });

    it('should return false for array', () => {
      expect(isEncryptedPayload([])).toBe(false);
    });

    it('should return false for string', () => {
      expect(isEncryptedPayload('encrypted')).toBe(false);
    });

    it('should return false for number', () => {
      expect(isEncryptedPayload(42)).toBe(false);
    });
  });
});
