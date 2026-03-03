import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  readdirSync,
  unlinkSync,
} from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {
  getEncryptionKey,
  ensureEncryptionKey,
  encryptData,
  decryptData,
  isEncryptedPayload,
  getKeyFilePath,
  restrictFilePermissions,
  restrictDirPermissions,
  type EncryptedPayload,
} from './encryption.js';

const AUTH_DIR = 'auth';

interface AuthProfile {
  name: string;
  url: string;
  username: string;
  password: string;
  usernameSelector?: string;
  passwordSelector?: string;
  submitSelector?: string;
  createdAt: string;
  lastLoginAt?: string;
}

export interface AuthProfileMeta {
  name: string;
  url: string;
  username: string;
  createdAt: string;
  lastLoginAt?: string;
}

function getAuthDir(): string {
  const dir = path.join(os.homedir(), '.agent-browser', AUTH_DIR);
  if (!existsSync(dir)) {
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    restrictDirPermissions(dir);
  }
  return dir;
}

const SAFE_NAME_RE = /^[a-zA-Z0-9_-]+$/;

function validateProfileName(name: string): void {
  if (!SAFE_NAME_RE.test(name)) {
    throw new Error(
      `Invalid auth profile name '${name}': only alphanumeric characters, hyphens, and underscores are allowed`
    );
  }
}

function profilePath(name: string): string {
  validateProfileName(name);
  return path.join(getAuthDir(), `${name}.json`);
}

function readProfile(name: string): AuthProfile | null {
  const p = profilePath(name);
  if (!existsSync(p)) return null;

  const raw = readFileSync(p, 'utf-8');
  const parsed = JSON.parse(raw);

  if (isEncryptedPayload(parsed)) {
    const key = getEncryptionKey();
    if (!key) {
      throw new Error(
        `Encryption key required to read encrypted auth profiles. ` +
          `Set AGENT_BROWSER_ENCRYPTION_KEY or ensure ${getKeyFilePath()} exists.`
      );
    }
    const decrypted = decryptData(parsed as EncryptedPayload, key);
    return JSON.parse(decrypted) as AuthProfile;
  }

  return parsed as AuthProfile;
}

function writeProfile(profile: AuthProfile): void {
  const key = ensureEncryptionKey();
  const serialized = JSON.stringify(profile, null, 2);
  const encrypted = encryptData(serialized, key);
  const filePath = profilePath(profile.name);
  writeFileSync(filePath, JSON.stringify(encrypted, null, 2), {
    mode: 0o600,
  });
  restrictFilePermissions(filePath);
}

export function saveAuthProfile(opts: {
  name: string;
  url: string;
  username: string;
  password: string;
  usernameSelector?: string;
  passwordSelector?: string;
  submitSelector?: string;
}): AuthProfileMeta & { updated: boolean } {
  const existing = readProfile(opts.name);

  const profile: AuthProfile = {
    name: opts.name,
    url: opts.url,
    username: opts.username,
    password: opts.password,
    usernameSelector: opts.usernameSelector,
    passwordSelector: opts.passwordSelector,
    submitSelector: opts.submitSelector,
    createdAt: existing?.createdAt ?? new Date().toISOString(),
    lastLoginAt: existing?.lastLoginAt,
  };

  writeProfile(profile);

  return {
    name: profile.name,
    url: profile.url,
    username: profile.username,
    createdAt: profile.createdAt,
    lastLoginAt: profile.lastLoginAt,
    updated: existing !== null,
  };
}

export function getAuthProfile(name: string): AuthProfile | null {
  return readProfile(name);
}

export function getAuthProfileMeta(name: string): AuthProfileMeta | null {
  const profile = readProfile(name);
  if (!profile) return null;
  return {
    name: profile.name,
    url: profile.url,
    username: profile.username,
    createdAt: profile.createdAt,
    lastLoginAt: profile.lastLoginAt,
  };
}

export function listAuthProfiles(): AuthProfileMeta[] {
  const dir = getAuthDir();
  const files = readdirSync(dir).filter((f) => f.endsWith('.json'));
  const profiles: AuthProfileMeta[] = [];

  for (const file of files) {
    const name = file.replace(/\.json$/, '');
    try {
      const meta = getAuthProfileMeta(name);
      if (meta) profiles.push(meta);
    } catch {
      profiles.push({
        name,
        url: '(encrypted)',
        username: '(encrypted)',
        createdAt: '(unknown)',
      });
    }
  }

  return profiles;
}

export function deleteAuthProfile(name: string): boolean {
  const p = profilePath(name);
  if (!existsSync(p)) return false;
  unlinkSync(p);
  return true;
}

export function updateLastLogin(name: string): void {
  const profile = readProfile(name);
  if (profile) {
    profile.lastLoginAt = new Date().toISOString();
    writeProfile(profile);
  }
}
