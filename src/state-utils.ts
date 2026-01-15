/**
 * Shared utilities for session state management.
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import {
  getEncryptionKey,
  encryptData,
  decryptData,
  isEncryptedPayload,
  type EncryptedPayload,
  ENCRYPTION_KEY_ENV,
} from './encryption.js';

/**
 * Get the session persistence directory.
 * Located at ~/.agent-browser/sessions/
 */
export function getSessionsDir(): string {
  return path.join(os.homedir(), '.agent-browser', 'sessions');
}

/**
 * Ensure the sessions directory exists with proper permissions.
 * Creates directory with mode 0o700 (owner only).
 */
export function ensureSessionsDir(): string {
  const sessionsDir = getSessionsDir();
  if (!fs.existsSync(sessionsDir)) {
    fs.mkdirSync(sessionsDir, { recursive: true, mode: 0o700 });
  }
  return sessionsDir;
}

/**
 * Get the auto-save state file path for a session.
 * Pattern: {SESSION_NAME}-{SESSION_ID}.json
 *
 * @param sessionName - The session name (e.g., "twitter")
 * @param sessionId - The session ID (e.g., "default" or "agent1")
 * @returns Full path to the state file, or null if sessionName is empty
 */
export function getAutoStateFilePath(sessionName: string, sessionId: string): string | null {
  if (!sessionName) return null;
  const sessionsDir = ensureSessionsDir();
  return path.join(sessionsDir, `${sessionName}-${sessionId}.json`);
}

/**
 * Check if an auto-state file exists for a session.
 */
export function autoStateFileExists(sessionName: string, sessionId: string): boolean {
  const filePath = getAutoStateFilePath(sessionName, sessionId);
  return filePath ? fs.existsSync(filePath) : false;
}

/**
 * Write state data to file, encrypting if encryption key is available.
 *
 * @param filepath - Path to write the state file
 * @param data - State data object to write
 * @returns Object indicating whether the file was encrypted
 */
export function writeStateFile(filepath: string, data: object): { encrypted: boolean } {
  const key = getEncryptionKey();
  const jsonData = JSON.stringify(data, null, 2);

  if (key) {
    const encrypted = encryptData(jsonData, key);
    fs.writeFileSync(filepath, JSON.stringify(encrypted, null, 2));
    return { encrypted: true };
  }

  fs.writeFileSync(filepath, jsonData);
  return { encrypted: false };
}

/**
 * Read state data from file, decrypting if necessary.
 *
 * @param filepath - Path to the state file
 * @returns Object containing the data and whether it was encrypted
 * @throws Error if file is encrypted but no key is available
 */
export function readStateFile(filepath: string): { data: object; wasEncrypted: boolean } {
  const content = fs.readFileSync(filepath, 'utf-8');
  const parsed = JSON.parse(content);

  if (isEncryptedPayload(parsed)) {
    const key = getEncryptionKey();
    if (!key) {
      throw new Error(
        `State file is encrypted but ${ENCRYPTION_KEY_ENV} is not set. ` +
          `Set the environment variable to decrypt.`
      );
    }
    const decrypted = decryptData(parsed, key);
    return { data: JSON.parse(decrypted), wasEncrypted: true };
  }

  return { data: parsed, wasEncrypted: false };
}

/**
 * Validate a session name for safety (no path traversal).
 * Only allows alphanumeric characters, dashes, and underscores.
 */
export function isValidSessionName(name: string): boolean {
  return /^[a-zA-Z0-9_-]+$/.test(name);
}

// Re-export encryption utilities for convenience
export {
  getEncryptionKey,
  encryptData,
  decryptData,
  isEncryptedPayload,
  type EncryptedPayload,
  ENCRYPTION_KEY_ENV,
};
