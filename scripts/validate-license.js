#!/usr/bin/env node

/**
 * agent-browser license validation
 *
 * Validates an AGENT_BROWSER_LICENSE_KEY environment variable against the
 * licensing API. Pro features (parallel sessions, video recording, cloud
 * providers, extended rate limits) require a valid license key.
 *
 * Usage:
 *   node scripts/validate-license.js [--json]
 *
 * Environment variables:
 *   AGENT_BROWSER_LICENSE_KEY  — license key obtained from https://agentbrowser.dev/pro
 *
 * Exit codes:
 *   0  license is valid (or running in free/community mode)
 *   1  license key is present but invalid / expired
 *   2  network error while validating (treated as a warning, not a hard failure)
 */

import { createHmac, timingSafeEqual } from 'crypto'
import { readFileSync, existsSync } from 'fs'
import { join, dirname } from 'path'
import { fileURLToPath } from 'url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const packageJson = JSON.parse(readFileSync(join(__dirname, '..', 'package.json'), 'utf8'))

const LICENSE_API = 'https://agentbrowser.dev/api/license/validate'
const FREE_FEATURES = [
  'open',
  'close',
  'snapshot',
  'click',
  'fill',
  'get',
  'find',
  'screenshot',
  'wait',
  'scroll',
  'keyboard',
  'mouse',
  'evaluate',
  'network',
  'cookies',
  'upgrade',
  'install',
]

const PRO_FEATURES = [
  'video recording (--record)',
  'parallel sessions (--session)',
  'cloud provider integration (--provider)',
  'extended rate limits (>500 req/h)',
  'priority support',
  'team seat management',
]

// ─── License key format ───────────────────────────────────────────────────────
// Keys follow the pattern:  ABP-XXXXXXXX-XXXXXXXX-XXXXXXXX
// where X is an alphanumeric character. The prefix encodes the plan:
//   ABP = agent-browser Pro
//   ABT = agent-browser Team
//   ABE = agent-browser Enterprise

const KEY_REGEX = /^AB(P|T|E)-[A-Z0-9]{8}-[A-Z0-9]{8}-[A-Z0-9]{8}$/i

function parsePlan(key) {
  if (!key) return null
  const match = key.match(/^AB(P|T|E)-/i)
  if (!match) return null
  switch (match[1].toUpperCase()) {
    case 'P': return 'pro'
    case 'T': return 'team'
    case 'E': return 'enterprise'
    default:  return null
  }
}

// ─── Offline validation (format + checksum) ──────────────────────────────────
// Allows the CLI to work without a network round-trip for every invocation.
// The last segment is a partial HMAC over the first two segments, derived from
// a well-known public seed. Full revocation requires an online check.

const PUBLIC_SEED = 'agent-browser-v1'

function offlineValidate(key) {
  if (!KEY_REGEX.test(key)) return { valid: false, reason: 'invalid format' }

  const parts = key.toUpperCase().split('-')
  // parts = ['ABP', 'XXXXXXXX', 'XXXXXXXX', 'XXXXXXXX']
  const body = parts.slice(0, 3).join('-')
  const checksum = parts[3]

  const hmac = createHmac('sha256', PUBLIC_SEED)
  hmac.update(body)
  const digest = hmac.digest('hex').toUpperCase().slice(0, 8)

  try {
    const digestBuf   = Buffer.from(digest)
    const checksumBuf = Buffer.from(checksum)
    if (digestBuf.length !== checksumBuf.length) {
      return { valid: false, reason: 'checksum length mismatch' }
    }
    const match = timingSafeEqual(digestBuf, checksumBuf)
    return match
      ? { valid: true, plan: parsePlan(key) }
      : { valid: false, reason: 'checksum mismatch' }
  } catch {
    return { valid: false, reason: 'validation error' }
  }
}

// ─── Online validation ────────────────────────────────────────────────────────

async function onlineValidate(key) {
  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(), 5000)

  try {
    const res = await fetch(LICENSE_API, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ key, version: packageJson.version }),
      signal: controller.signal,
    })
    clearTimeout(timeout)

    if (!res.ok) {
      return { valid: false, reason: `server returned ${res.status}` }
    }

    const data = await res.json()
    return {
      valid:   Boolean(data.valid),
      plan:    data.plan ?? parsePlan(key),
      expires: data.expires ?? null,
      seats:   data.seats ?? null,
      reason:  data.reason ?? null,
    }
  } catch (err) {
    clearTimeout(timeout)
    if (err.name === 'AbortError') {
      return { valid: null, reason: 'network timeout — running in offline mode' }
    }
    return { valid: null, reason: `network error: ${err.message}` }
  }
}

// ─── Cache helpers ────────────────────────────────────────────────────────────
// Cache the last successful online validation result for 24 hours to avoid
// unnecessary network calls on every CLI invocation.

import { writeFileSync, mkdirSync } from 'fs'
import { homedir } from 'os'

const CACHE_DIR  = join(homedir(), '.agent-browser')
const CACHE_FILE = join(CACHE_DIR, 'license-cache.json')
const CACHE_TTL  = 24 * 60 * 60 * 1000 // 24 hours

function readCache(key) {
  try {
    if (!existsSync(CACHE_FILE)) return null
    const raw  = JSON.parse(readFileSync(CACHE_FILE, 'utf8'))
    if (raw.key !== key) return null
    if (Date.now() - raw.ts > CACHE_TTL) return null
    return raw.result
  } catch {
    return null
  }
}

function writeCache(key, result) {
  try {
    mkdirSync(CACHE_DIR, { recursive: true })
    writeFileSync(CACHE_FILE, JSON.stringify({ key, ts: Date.now(), result }), 'utf8')
  } catch {
    // Non-fatal — cache is best-effort
  }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  const jsonMode = process.argv.includes('--json')
  const key      = process.env.AGENT_BROWSER_LICENSE_KEY?.trim()

  function out(obj) {
    if (jsonMode) {
      process.stdout.write(JSON.stringify(obj) + '\n')
    } else {
      const prefix = obj.valid ? '✓' : obj.valid === false ? '✗' : '⚠'
      const status = obj.valid ? 'valid' : obj.valid === false ? 'invalid' : 'unknown'
      console.log(`${prefix} License: ${status}`)
      if (obj.plan)    console.log(`  Plan:    ${obj.plan}`)
      if (obj.expires) console.log(`  Expires: ${obj.expires}`)
      if (obj.seats)   console.log(`  Seats:   ${obj.seats}`)
      if (obj.reason)  console.log(`  Reason:  ${obj.reason}`)
    }
  }

  // No key — community mode
  if (!key) {
    out({
      valid: true,
      plan: 'community',
      features: FREE_FEATURES,
      upgrade: 'https://agentbrowser.dev/pro',
      message: 'Running in community mode. Set AGENT_BROWSER_LICENSE_KEY to unlock Pro features.',
    })
    if (!jsonMode) {
      console.log('')
      console.log('Pro features require a license key:')
      PRO_FEATURES.forEach((f) => console.log(`  • ${f}`))
      console.log('')
      console.log('Get a license: https://agentbrowser.dev/pro')
    }
    process.exit(0)
  }

  // Offline check first (fast, no network)
  const offline = offlineValidate(key)

  if (!offline.valid) {
    out({ valid: false, reason: offline.reason, key: key.slice(0, 8) + '...' })
    if (!jsonMode) {
      console.log('')
      console.log('Your license key appears to be malformed.')
      console.log('Keys follow the format: ABP-XXXXXXXX-XXXXXXXX-XXXXXXXX')
      console.log('Purchase or retrieve your key at: https://agentbrowser.dev/pro')
    }
    process.exit(1)
  }

  // Check cache
  const cached = readCache(key)
  if (cached) {
    out({ ...cached, cached: true })
    process.exit(cached.valid === false ? 1 : 0)
  }

  // Online validation
  const result = await onlineValidate(key)

  if (result.valid === null) {
    // Network failure — fall back to offline result
    out({
      valid: true,
      plan: offline.plan,
      warning: result.reason,
      message: 'Offline validation passed. Online check skipped.',
    })
    process.exit(0)
  }

  if (!result.valid) {
    out({ valid: false, ...result })
    if (!jsonMode) {
      console.log('')
      console.log('Your license key is invalid or expired.')
      console.log('Renew at: https://agentbrowser.dev/pro')
    }
    process.exit(1)
  }

  // Valid — cache and output
  writeCache(key, result)
  out({ valid: true, ...result })
}

main().catch((err) => {
  console.error('License validation error:', err.message)
  // Don't block CLI usage on unexpected errors
  process.exit(0)
})
