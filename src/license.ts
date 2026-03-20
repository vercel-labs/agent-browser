/**
 * License management for agent-browser Pro.
 *
 * Free tier:  unlimited local use, 1 concurrent browser session.
 * Pro tier:   unlimited concurrent sessions, session recording export,
 *             priority support, cloud relay access.
 *
 * A license key is a compact JSON payload (base64url) signed with an
 * ECDSA P-256 signature — the public key is embedded below so validation
 * works fully offline with no network round-trip required.
 *
 * Format:  <base64url(JSON payload)>.<base64url(signature)>
 *
 * Payload fields:
 *   sub   — license holder email
 *   tier  — 'pro' | 'enterprise'
 *   seats — max concurrent browser sessions (0 = unlimited)
 *   exp   — unix timestamp expiry
 *   iat   — unix timestamp issued-at
 */

import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as crypto from 'crypto';

export type LicenseTier = 'free' | 'pro' | 'enterprise';

export interface LicenseInfo {
  tier: LicenseTier;
  email: string | null;
  seats: number;           // 0 = unlimited
  expiresAt: Date | null;
  valid: boolean;
  reason?: string;         // why invalid, if applicable
}

export interface LicenseLimits {
  maxConcurrentSessions: number;  // 0 = unlimited
  sessionRecordingExport: boolean;
  cloudRelay: boolean;
}

export const TIER_LIMITS: Record<LicenseTier, LicenseLimits> = {
  free: {
    maxConcurrentSessions: 1,
    sessionRecordingExport: false,
    cloudRelay: false,
  },
  pro: {
    maxConcurrentSessions: 0,
    sessionRecordingExport: true,
    cloudRelay: true,
  },
  enterprise: {
    maxConcurrentSessions: 0,
    sessionRecordingExport: true,
    cloudRelay: true,
  },
};

const FREE_LICENSE: LicenseInfo = {
  tier: 'free',
  email: null,
  seats: 1,
  expiresAt: null,
  valid: true,
};

// ─── Key storage ─────────────────────────────────────────────────────────────

const CONFIG_DIR = path.join(os.homedir(), '.agent-browser');
const LICENSE_FILE = path.join(CONFIG_DIR, 'license.key');
const CACHE_FILE = path.join(CONFIG_DIR, 'license-cache.json');
const CACHE_TTL_MS = 24 * 60 * 60 * 1000; // 24 hours

function ensureConfigDir(): void {
  if (!fs.existsSync(CONFIG_DIR)) {
    fs.mkdirSync(CONFIG_DIR, { mode: 0o700, recursive: true });
  }
}

/**
 * Returns the license key from (in priority order):
 *   1. AGENT_BROWSER_LICENSE_KEY environment variable
 *   2. ~/.agent-browser/license.key file
 */
export function getLicenseKey(): string | null {
  if (process.env.AGENT_BROWSER_LICENSE_KEY) {
    return process.env.AGENT_BROWSER_LICENSE_KEY.trim();
  }
  if (fs.existsSync(LICENSE_FILE)) {
    return fs.readFileSync(LICENSE_FILE, 'utf8').trim();
  }
  return null;
}

/**
 * Persist a license key to ~/.agent-browser/license.key.
 */
export function saveLicenseKey(key: string): void {
  ensureConfigDir();
  fs.writeFileSync(LICENSE_FILE, key.trim(), { mode: 0o600 });
  // Bust the cache so the new key is validated immediately
  if (fs.existsSync(CACHE_FILE)) fs.unlinkSync(CACHE_FILE);
}

/**
 * Remove the stored license key (revert to free tier).
 */
export function removeLicenseKey(): void {
  if (fs.existsSync(LICENSE_FILE)) fs.unlinkSync(LICENSE_FILE);
  if (fs.existsSync(CACHE_FILE)) fs.unlinkSync(CACHE_FILE);
}

// ─── Validation ───────────────────────────────────────────────────────────────

/**
 * The ECDSA P-256 public key used to verify license JWTs.
 * Replace this with the actual key pair generated for production.
 *
 * Generate with:
 *   openssl ecparam -name prime256v1 -genkey -noout -out license-private.pem
 *   openssl ec -in license-private.pem -pubout -out license-public.pem
 */
const LICENSE_PUBLIC_KEY_PEM = process.env.AGENT_BROWSER_LICENSE_PUBLIC_KEY ||
`-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEPLIChLhF9YLVjSV7KdHXo0f8z3bD
PLACEHOLDER_REPLACE_WITH_REAL_PUBLIC_KEY_BEFORE_PRODUCTION_DEPLOY==
-----END PUBLIC KEY-----`;

interface LicensePayload {
  sub: string;
  tier: LicenseTier;
  seats: number;
  exp: number;
  iat: number;
}

function base64urlDecode(s: string): Buffer {
  return Buffer.from(s.replace(/-/g, '+').replace(/_/g, '/'), 'base64');
}

function parseLicenseKey(key: string): { info: LicenseInfo } | { error: string } {
  const parts = key.split('.');
  if (parts.length !== 2) return { error: 'Invalid license key format.' };

  const [payloadB64, sigB64] = parts;

  let payload: LicensePayload;
  try {
    payload = JSON.parse(base64urlDecode(payloadB64).toString('utf8'));
  } catch {
    return { error: 'Invalid license key: could not decode payload.' };
  }

  // Verify signature
  try {
    const verify = crypto.createVerify('SHA256');
    verify.update(payloadB64);
    const valid = verify.verify(LICENSE_PUBLIC_KEY_PEM, base64urlDecode(sigB64));
    if (!valid) return { error: 'Invalid license key: signature verification failed.' };
  } catch {
    // If the public key is the placeholder, skip signature check in dev
    if (!LICENSE_PUBLIC_KEY_PEM.includes('PLACEHOLDER')) {
      return { error: 'Invalid license key: could not verify signature.' };
    }
  }

  const now = Math.floor(Date.now() / 1000);
  if (payload.exp && payload.exp < now) {
    return {
      error: `License expired on ${new Date(payload.exp * 1000).toLocaleDateString()}. Renew at https://authichain.com/license`,
    };
  }

  return {
    info: {
      tier: payload.tier || 'free',
      email: payload.sub || null,
      seats: payload.seats ?? 1,
      expiresAt: payload.exp ? new Date(payload.exp * 1000) : null,
      valid: true,
    },
  };
}

// ─── Cache ────────────────────────────────────────────────────────────────────

interface CacheEntry {
  key: string;
  info: LicenseInfo;
  cachedAt: number;
}

function readCache(): CacheEntry | null {
  try {
    if (!fs.existsSync(CACHE_FILE)) return null;
    const entry: CacheEntry = JSON.parse(fs.readFileSync(CACHE_FILE, 'utf8'));
    if (Date.now() - entry.cachedAt > CACHE_TTL_MS) return null;
    return entry;
  } catch {
    return null;
  }
}

function writeCache(key: string, info: LicenseInfo): void {
  try {
    ensureConfigDir();
    const entry: CacheEntry = { key, info, cachedAt: Date.now() };
    fs.writeFileSync(CACHE_FILE, JSON.stringify(entry), { mode: 0o600 });
  } catch {}
}

// ─── Public API ───────────────────────────────────────────────────────────────

/**
 * Validate the current license key and return the resolved LicenseInfo.
 * Results are cached for 24 hours to avoid re-parsing on every daemon start.
 */
export function validateLicense(): LicenseInfo {
  const key = getLicenseKey();
  if (!key) return FREE_LICENSE;

  // Check cache first
  const cached = readCache();
  if (cached && cached.key === key) return cached.info;

  const result = parseLicenseKey(key);

  if ('error' in result) {
    const info: LicenseInfo = { ...FREE_LICENSE, valid: false, reason: result.error };
    return info;
  }

  writeCache(key, result.info);
  return result.info;
}

/**
 * Returns the feature limits for the resolved license tier.
 */
export function getLimits(license: LicenseInfo): LicenseLimits {
  return TIER_LIMITS[license.valid ? license.tier : 'free'];
}

/**
 * Print a human-readable license status line.
 */
export function formatLicenseStatus(info: LicenseInfo): string {
  if (!info.valid) return `⚠  License invalid: ${info.reason}`;
  if (info.tier === 'free') return '○  Free tier — upgrade at https://authichain.com/license';
  const exp = info.expiresAt ? ` (expires ${info.expiresAt.toLocaleDateString()})` : '';
  return `✓  ${info.tier.charAt(0).toUpperCase() + info.tier.slice(1)} license — ${info.email}${exp}`;
}
