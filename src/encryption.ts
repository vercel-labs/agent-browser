/**
 * Encryption utilities for state file protection using AES-256-GCM.
 */

import * as crypto from 'crypto';

// ============================================
// Constants
// ============================================
export const ENCRYPTION_ALGORITHM = 'aes-256-gcm';
export const ENCRYPTION_KEY_ENV = 'AGENT_BROWSER_ENCRYPTION_KEY';
export const IV_LENGTH = 12; // 96 bits for GCM

/**
 * Encrypted payload structure.
 */
export interface EncryptedPayload {
  version: 1;
  encrypted: true;
  iv: string; // Base64 encoded
  authTag: string; // Base64 encoded
  data: string; // Base64 encoded ciphertext
}

/**
 * Get encryption key from environment variable.
 * The key should be a 32-byte (256-bit) hex-encoded string (64 characters).
 * Generate with: openssl rand -hex 32
 *
 * @returns Buffer containing the key, or null if not set/invalid
 */
export function getEncryptionKey(): Buffer | null {
  const keyHex = process.env[ENCRYPTION_KEY_ENV];
  if (!keyHex) return null;

  // Key should be 64 hex chars = 32 bytes = 256 bits
  if (!/^[a-fA-F0-9]{64}$/.test(keyHex)) {
    console.warn(
      `Warning: ${ENCRYPTION_KEY_ENV} should be a 64-character hex string (256 bits). ` +
        `Generate one with: openssl rand -hex 32`
    );
    return null;
  }

  return Buffer.from(keyHex, 'hex');
}

/**
 * Encrypt data using AES-256-GCM.
 * Returns a JSON-serializable payload with IV, auth tag, and encrypted data.
 *
 * @param plaintext - The string to encrypt
 * @param key - The 256-bit encryption key
 * @returns Encrypted payload object
 */
export function encryptData(plaintext: string, key: Buffer): EncryptedPayload {
  const iv = crypto.randomBytes(IV_LENGTH);
  const cipher = crypto.createCipheriv(ENCRYPTION_ALGORITHM, key, iv);

  let encrypted = cipher.update(plaintext, 'utf8');
  encrypted = Buffer.concat([encrypted, cipher.final()]);

  return {
    version: 1,
    encrypted: true,
    iv: iv.toString('base64'),
    authTag: cipher.getAuthTag().toString('base64'),
    data: encrypted.toString('base64'),
  };
}

/**
 * Decrypt data using AES-256-GCM.
 *
 * @param payload - The encrypted payload object
 * @param key - The 256-bit encryption key
 * @returns Decrypted plaintext string
 * @throws Error if decryption fails (wrong key, tampered data, etc.)
 */
export function decryptData(payload: EncryptedPayload, key: Buffer): string {
  const iv = Buffer.from(payload.iv, 'base64');
  const authTag = Buffer.from(payload.authTag, 'base64');
  const encryptedData = Buffer.from(payload.data, 'base64');

  const decipher = crypto.createDecipheriv(ENCRYPTION_ALGORITHM, key, iv);
  decipher.setAuthTag(authTag);

  let decrypted = decipher.update(encryptedData);
  decrypted = Buffer.concat([decrypted, decipher.final()]);

  return decrypted.toString('utf8');
}

/**
 * Check if a parsed JSON object is an encrypted payload.
 *
 * @param data - The object to check
 * @returns True if the object is a valid encrypted payload
 */
export function isEncryptedPayload(data: unknown): data is EncryptedPayload {
  return (
    typeof data === 'object' &&
    data !== null &&
    'encrypted' in data &&
    (data as EncryptedPayload).encrypted === true &&
    'version' in data &&
    'iv' in data &&
    'authTag' in data &&
    'data' in data
  );
}
