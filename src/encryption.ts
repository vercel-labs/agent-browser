/**
 * Encryption utilities for state file protection using AES-256-GCM.
 */

import * as crypto from 'crypto';
import { execSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import os from 'node:os';

// ============================================
// Constants
// ============================================
export const ENCRYPTION_ALGORITHM = 'aes-256-gcm';
export const ENCRYPTION_KEY_ENV = 'AGENT_BROWSER_ENCRYPTION_KEY';
export const IV_LENGTH = 12; // 96 bits for GCM
const KEY_FILE_NAME = '.encryption-key';

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

export function getKeyFilePath(): string {
  return join(os.homedir(), '.agent-browser', KEY_FILE_NAME);
}

/**
 * Restrict file permissions to the current user only.
 * On Unix, the caller should use `mode: 0o600` when writing. This function
 * handles Windows where Node's mode parameter is ignored.
 */
export function restrictFilePermissions(filePath: string): void {
  if (os.platform() !== 'win32') return;
  try {
    execSync(`icacls "${filePath}" /inheritance:r /grant:r "%USERNAME%:F"`, {
      stdio: 'ignore',
      windowsHide: true,
    });
  } catch {
    // Best-effort; may fail in some environments (containers, restricted shells)
  }
}

/**
 * Restrict directory permissions to the current user only.
 * On Unix, the caller should use `mode: 0o700` when creating. This function
 * handles Windows where Node's mode parameter is ignored.
 */
export function restrictDirPermissions(dirPath: string): void {
  if (os.platform() !== 'win32') return;
  try {
    execSync(`icacls "${dirPath}" /inheritance:r /grant:r "%USERNAME%:(OI)(CI)F"`, {
      stdio: 'ignore',
      windowsHide: true,
    });
  } catch {
    // Best-effort
  }
}

function parseKeyHex(keyHex: string): Buffer | null {
  if (!/^[a-fA-F0-9]{64}$/.test(keyHex.trim())) return null;
  return Buffer.from(keyHex.trim(), 'hex');
}

/**
 * Get encryption key from environment variable or key file.
 * The key should be a 32-byte (256-bit) hex-encoded string (64 characters).
 * Generate with: openssl rand -hex 32
 *
 * Checks (in order):
 * 1. AGENT_BROWSER_ENCRYPTION_KEY env var
 * 2. ~/.agent-browser/.encryption-key file
 *
 * @returns Buffer containing the key, or null if not available
 */
export function getEncryptionKey(): Buffer | null {
  const keyHex = process.env[ENCRYPTION_KEY_ENV];
  if (keyHex) {
    const key = parseKeyHex(keyHex);
    if (!key) {
      console.warn(
        `Warning: ${ENCRYPTION_KEY_ENV} should be a 64-character hex string (256 bits). ` +
          `Generate one with: openssl rand -hex 32`
      );
      return null;
    }
    return key;
  }

  const keyFilePath = getKeyFilePath();
  if (existsSync(keyFilePath)) {
    try {
      const fileHex = readFileSync(keyFilePath, 'utf-8');
      return parseKeyHex(fileHex);
    } catch {
      return null;
    }
  }

  return null;
}

/**
 * Ensure an encryption key is available, auto-generating one if needed.
 * On first call without an existing key, generates a random 256-bit key
 * and writes it to ~/.agent-browser/.encryption-key (mode 0600).
 */
export function ensureEncryptionKey(): Buffer {
  const existing = getEncryptionKey();
  if (existing) return existing;

  const key = crypto.randomBytes(32);
  const keyHex = key.toString('hex');

  const dir = join(os.homedir(), '.agent-browser');
  if (!existsSync(dir)) {
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    restrictDirPermissions(dir);
  }

  const keyFilePath = getKeyFilePath();
  writeFileSync(keyFilePath, keyHex + '\n', { mode: 0o600 });
  restrictFilePermissions(keyFilePath);

  console.error(
    `[agent-browser] Auto-generated encryption key at ${keyFilePath} -- back up this file or set ${ENCRYPTION_KEY_ENV}`
  );

  return key;
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
