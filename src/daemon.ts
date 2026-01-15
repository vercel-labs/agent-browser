import * as net from 'net';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { BrowserManager } from './browser.js';
import { parseCommand, serializeResponse, errorResponse } from './protocol.js';
import { executeCommand } from './actions.js';
import {
  getSessionsDir,
  ensureSessionsDir,
  getEncryptionKey,
  encryptData,
  isEncryptedPayload,
  decryptData,
  ENCRYPTION_KEY_ENV,
} from './state-utils.js';

// Platform detection
const isWindows = process.platform === 'win32';

// Session support - each session gets its own socket/pid
let currentSession = process.env.AGENT_BROWSER_SESSION || 'default';

// Stream server for browser preview
let streamServer: StreamServer | null = null;

// Default stream port (can be overridden with AGENT_BROWSER_STREAM_PORT)
const DEFAULT_STREAM_PORT = 9223;

/**
 * Save state to file with optional encryption.
 */
async function saveStateToFile(
  browser: BrowserManager,
  filepath: string
): Promise<{ encrypted: boolean }> {
  // First get the storage state from Playwright
  const context = browser.getContext();
  if (!context) {
    throw new Error('No browser context available');
  }

  const state = await context.storageState();
  const jsonData = JSON.stringify(state, null, 2);

  const key = getEncryptionKey();
  if (key) {
    const encrypted = encryptData(jsonData, key);
    fs.writeFileSync(filepath, JSON.stringify(encrypted, null, 2));
    return { encrypted: true };
  }

  fs.writeFileSync(filepath, jsonData);
  return { encrypted: false };
}

/**
 * Load state from file with automatic decryption.
 */
function loadStateFromFile(filepath: string): object {
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
    return JSON.parse(decrypted);
  }

  return parsed;
}

/**
 * Get the auto-save state file path for current session
 * Pattern: {SESSION_NAME}-{SESSION_ID}.json
 */
function getAutoStateFilePath(sessionName: string, sessionId: string): string | null {
  if (!sessionName) return null;
  const sessionsDir = ensureSessionsDir();
  return path.join(sessionsDir, `${sessionName}-${sessionId}.json`);
}

/**
 * Check if auto-state file exists
 */
function autoStateFileExists(sessionName: string, sessionId: string): boolean {
  const filePath = getAutoStateFilePath(sessionName, sessionId);
  return filePath ? fs.existsSync(filePath) : false;
}

// Auto-expiration configuration
const AUTO_EXPIRE_ENV = 'AGENT_BROWSER_STATE_EXPIRE_DAYS';
const DEFAULT_EXPIRE_DAYS = 30;

/**
 * Clean up expired state files (files older than N days).
 * Called on daemon startup.
 */
function cleanupExpiredStates(): void {
  const expireDaysStr = process.env[AUTO_EXPIRE_ENV];
  const expireDays = expireDaysStr ? parseInt(expireDaysStr, 10) : DEFAULT_EXPIRE_DAYS;

  // Skip if set to 0 or negative (disabled)
  if (isNaN(expireDays) || expireDays <= 0) {
    return;
  }

  const sessionsDir = getSessionsDir();
  if (!fs.existsSync(sessionsDir)) {
    return;
  }

  const now = Date.now();
  const maxAge = expireDays * 24 * 60 * 60 * 1000;
  let deletedCount = 0;

  try {
    const files = fs.readdirSync(sessionsDir).filter((f) => f.endsWith('.json'));

    for (const file of files) {
      const filepath = path.join(sessionsDir, file);
      try {
        const stats = fs.statSync(filepath);
        const age = now - stats.mtime.getTime();

        if (age > maxAge) {
          fs.unlinkSync(filepath);
          deletedCount++;
        }
      } catch {
        // Ignore individual file errors
      }
    }

    if (deletedCount > 0 && process.env.AGENT_BROWSER_DEBUG === '1') {
      console.error(
        `[DEBUG] Auto-expired ${deletedCount} state file(s) older than ${expireDays} days`
      );
    }
  } catch (err) {
    if (process.env.AGENT_BROWSER_DEBUG === '1') {
      console.error(`[DEBUG] Failed to clean up expired states:`, err);
    }
  }
}

/**
 * Set the current session
 */
export function setSession(session: string): void {
  currentSession = session;
}

/**
 * Get the current session
 */
export function getSession(): string {
  return currentSession;
}

/**
 * Get port number for TCP mode (Windows)
 * Uses a hash of the session name to get a consistent port
 */
function getPortForSession(session: string): number {
  let hash = 0;
  for (let i = 0; i < session.length; i++) {
    hash = (hash << 5) - hash + session.charCodeAt(i);
    hash |= 0;
  }
  // Port range 49152-65535 (dynamic/private ports)
  return 49152 + (Math.abs(hash) % 16383);
}

/**
 * Get the socket path for the current session (Unix) or port (Windows)
 */
export function getSocketPath(session?: string): string {
  const sess = session ?? currentSession;
  if (isWindows) {
    return String(getPortForSession(sess));
  }
  return path.join(os.tmpdir(), `agent-browser-${sess}.sock`);
}

/**
 * Get the port file path for Windows (stores the port number)
 */
export function getPortFile(session?: string): string {
  const sess = session ?? currentSession;
  return path.join(os.tmpdir(), `agent-browser-${sess}.port`);
}

/**
 * Get the PID file path for the current session
 */
export function getPidFile(session?: string): string {
  const sess = session ?? currentSession;
  return path.join(os.tmpdir(), `agent-browser-${sess}.pid`);
}

/**
 * Check if daemon is running for the current session
 */
export function isDaemonRunning(session?: string): boolean {
  const pidFile = getPidFile(session);
  if (!fs.existsSync(pidFile)) return false;

  try {
    const pid = parseInt(fs.readFileSync(pidFile, 'utf8').trim(), 10);
    // Check if process exists (works on both Unix and Windows)
    process.kill(pid, 0);
    return true;
  } catch {
    // Process doesn't exist, clean up stale files
    cleanupSocket(session);
    return false;
  }
}

/**
 * Get connection info for the current session
 * Returns { type: 'unix', path: string } or { type: 'tcp', port: number }
 */
export function getConnectionInfo(
  session?: string
): { type: 'unix'; path: string } | { type: 'tcp'; port: number } {
  const sess = session ?? currentSession;
  if (isWindows) {
    return { type: 'tcp', port: getPortForSession(sess) };
  }
  return { type: 'unix', path: path.join(os.tmpdir(), `agent-browser-${sess}.sock`) };
}

/**
 * Clean up socket and PID file for the current session
 */
export function cleanupSocket(session?: string): void {
  const pidFile = getPidFile(session);
  const streamPortFile = getStreamPortFile(session);
  try {
    if (fs.existsSync(pidFile)) fs.unlinkSync(pidFile);
    if (fs.existsSync(streamPortFile)) fs.unlinkSync(streamPortFile);
    if (isWindows) {
      const portFile = getPortFile(session);
      if (fs.existsSync(portFile)) fs.unlinkSync(portFile);
    } else {
      const socketPath = getSocketPath(session);
      if (fs.existsSync(socketPath)) fs.unlinkSync(socketPath);
    }
  } catch {
    // Ignore cleanup errors
  }
}

/**
 * Get the stream port file path
 */
export function getStreamPortFile(session?: string): string {
  const sess = session ?? currentSession;
  return path.join(os.tmpdir(), `agent-browser-${sess}.stream`);
}

/**
 * Start the daemon server
 * @param options.streamPort Port for WebSocket stream server (0 to disable)
 */
export async function startDaemon(options?: { streamPort?: number }): Promise<void> {
  // Clean up any stale socket
  cleanupSocket();

  // Clean up expired state files on startup
  cleanupExpiredStates();

  const browser = new BrowserManager();
  let shuttingDown = false;

  // Start stream server if port is specified (or use default if env var is set)
  const streamPort =
    options?.streamPort ??
    (process.env.AGENT_BROWSER_STREAM_PORT
      ? parseInt(process.env.AGENT_BROWSER_STREAM_PORT, 10)
      : 0);

  if (streamPort > 0) {
    streamServer = new StreamServer(browser, streamPort);
    await streamServer.start();

    // Write stream port to file for clients to discover
    const streamPortFile = getStreamPortFile();
    fs.writeFileSync(streamPortFile, streamPort.toString());
  }

  const server = net.createServer((socket) => {
    let buffer = '';

    socket.on('data', async (data) => {
      buffer += data.toString();

      // Process complete lines
      // Use indexOf directly instead of includes() + indexOf() to avoid double scan
      let newlineIdx: number;
      while ((newlineIdx = buffer.indexOf('\n')) !== -1) {
        const line = buffer.slice(0, newlineIdx);
        buffer = buffer.slice(newlineIdx + 1);

        if (!line.trim()) continue;

        try {
          const parseResult = parseCommand(line);

          if (!parseResult.success) {
            const resp = errorResponse(parseResult.id ?? 'unknown', parseResult.error);
            socket.write(serializeResponse(resp) + '\n');
            continue;
          }

          // Auto-launch browser if not already launched and this isn't a launch command
          if (
            !browser.isLaunched() &&
            parseResult.command.action !== 'launch' &&
            parseResult.command.action !== 'close'
          ) {
            // Check for auto-load state
            const sessionName = process.env.AGENT_BROWSER_SESSION_NAME;
            const sessionId = process.env.AGENT_BROWSER_SESSION || 'default';
            const autoStatePath = sessionName
              ? getAutoStateFilePath(sessionName, sessionId)
              : undefined;

            await browser.launch({
              id: 'auto',
              action: 'launch',
              headless: true,
              executablePath: process.env.AGENT_BROWSER_EXECUTABLE_PATH,
              autoStateFilePath:
                autoStatePath && fs.existsSync(autoStatePath) ? autoStatePath : undefined,
            });
          }

          // Handle explicit launch with auto-load state
          if (parseResult.command.action === 'launch') {
            const sessionName = process.env.AGENT_BROWSER_SESSION_NAME;
            const sessionId = process.env.AGENT_BROWSER_SESSION || 'default';

            if (sessionName && !parseResult.command.autoStateFilePath) {
              const autoStatePath = getAutoStateFilePath(sessionName, sessionId);
              if (autoStatePath && fs.existsSync(autoStatePath)) {
                parseResult.command.autoStateFilePath = autoStatePath;
              }
            }
          }

          // Handle close command specially
          if (parseResult.command.action === 'close') {
            // Auto-save state before closing
            const sessionName = process.env.AGENT_BROWSER_SESSION_NAME;
            const sessionId = process.env.AGENT_BROWSER_SESSION || 'default';

            if (sessionName && browser.isLaunched()) {
              const autoStatePath = getAutoStateFilePath(sessionName, sessionId);
              if (autoStatePath) {
                try {
                  const { encrypted } = await saveStateToFile(browser, autoStatePath);
                  // Set file permissions to owner read/write only (0o600)
                  fs.chmodSync(autoStatePath, 0o600);
                  if (process.env.AGENT_BROWSER_DEBUG === '1') {
                    console.error(
                      `Auto-saved session state: ${autoStatePath}${encrypted ? ' (encrypted)' : ''}`
                    );
                  }
                } catch (err) {
                  // Non-blocking: don't fail close if save fails
                  if (process.env.AGENT_BROWSER_DEBUG === '1') {
                    console.error(`Failed to auto-save session state:`, err);
                  }
                }
              }
            }

            const response = await executeCommand(parseResult.command, browser);
            socket.write(serializeResponse(response) + '\n');

            if (!shuttingDown) {
              shuttingDown = true;
              setTimeout(() => {
                server.close();
                cleanupSocket();
                process.exit(0);
              }, 100);
            }
            return;
          }

          const response = await executeCommand(parseResult.command, browser);

          // Add any launch warnings to the response
          const warnings = browser.getAndClearWarnings();
          if (warnings.length > 0 && response.success && response.data) {
            (response.data as Record<string, unknown>).warnings = warnings;
          }

          socket.write(serializeResponse(response) + '\n');
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          socket.write(serializeResponse(errorResponse('error', message)) + '\n');
        }
      }
    });

    socket.on('error', () => {
      // Client disconnected, ignore
    });
  });

  const pidFile = getPidFile();

  // Write PID file before listening
  fs.writeFileSync(pidFile, process.pid.toString());

  if (isWindows) {
    // Windows: use TCP socket on localhost
    const port = getPortForSession(currentSession);
    const portFile = getPortFile();
    fs.writeFileSync(portFile, port.toString());
    server.listen(port, '127.0.0.1', () => {
      // Daemon is ready on TCP port
    });
  } else {
    // Unix: use Unix domain socket
    const socketPath = getSocketPath();
    server.listen(socketPath, () => {
      // Daemon is ready
    });
  }

  server.on('error', (err) => {
    console.error('Server error:', err);
    cleanupSocket();
    process.exit(1);
  });

  // Handle shutdown signals
  const shutdown = async () => {
    if (shuttingDown) return;
    shuttingDown = true;

    // Stop stream server if running
    if (streamServer) {
      await streamServer.stop();
      streamServer = null;
      // Clean up stream port file
      const streamPortFile = getStreamPortFile();
      try {
        if (fs.existsSync(streamPortFile)) fs.unlinkSync(streamPortFile);
      } catch {
        // Ignore cleanup errors
      }
    }

    await browser.close();
    server.close();
    cleanupSocket();
    process.exit(0);
  };

  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
  process.on('SIGHUP', shutdown);

  // Handle unexpected errors - always cleanup
  process.on('uncaughtException', (err) => {
    console.error('Uncaught exception:', err);
    cleanupSocket();
    process.exit(1);
  });

  process.on('unhandledRejection', (reason) => {
    console.error('Unhandled rejection:', reason);
    cleanupSocket();
    process.exit(1);
  });

  // Cleanup on normal exit
  process.on('exit', () => {
    cleanupSocket();
  });

  // Keep process alive
  process.stdin.resume();
}

// Run daemon if this is the entry point
if (process.argv[1]?.endsWith('daemon.js') || process.env.AGENT_BROWSER_DAEMON === '1') {
  startDaemon().catch((err) => {
    console.error('Daemon error:', err);
    cleanupSocket();
    process.exit(1);
  });
}
