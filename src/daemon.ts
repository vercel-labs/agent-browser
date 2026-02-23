import * as net from 'net';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { BrowserManager } from './browser.js';
import { IOSManager } from './ios-manager.js';
import { parseCommand, serializeResponse, errorResponse } from './protocol.js';
import { executeCommand } from './actions.js';
import { executeIOSCommand } from './ios-actions.js';
import { StreamServer } from './stream-server.js';
import {
  getSessionsDir,
  ensureSessionsDir,
  getEncryptionKey,
  encryptData,
  isValidSessionName,
  cleanupExpiredStates,
  getAutoStateFilePath,
} from './state-utils.js';

// Manager type - either desktop browser or iOS
type Manager = BrowserManager | IOSManager;

/**
 * Backpressure-aware socket write.
 * If the kernel buffer is full (socket.write returns false),
 * waits for the 'drain' event before resolving.
 */
export function safeWrite(socket: net.Socket, payload: string): Promise<void> {
  return new Promise((resolve, reject) => {
    if (socket.destroyed) {
      resolve();
      return;
    }
    const canContinue = socket.write(payload);
    if (canContinue) {
      resolve();
    } else if (socket.destroyed) {
      resolve();
    } else {
      const cleanup = () => {
        socket.removeListener('drain', onDrain);
        socket.removeListener('error', onError);
        socket.removeListener('close', onClose);
      };
      const onDrain = () => {
        cleanup();
        resolve();
      };
      const onError = (err: Error) => {
        cleanup();
        reject(err);
      };
      const onClose = () => {
        cleanup();
        resolve();
      };
      socket.once('drain', onDrain);
      socket.once('error', onError);
      socket.once('close', onClose);
    }
  });
}

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

const AUTO_EXPIRE_ENV = 'AGENT_BROWSER_STATE_EXPIRE_DAYS';
const DEFAULT_EXPIRE_DAYS = 30;

function runCleanupExpiredStates(): void {
  const expireDaysStr = process.env[AUTO_EXPIRE_ENV];
  const expireDays = expireDaysStr ? parseInt(expireDaysStr, 10) : DEFAULT_EXPIRE_DAYS;

  if (isNaN(expireDays) || expireDays <= 0) {
    return;
  }

  try {
    const deleted = cleanupExpiredStates(expireDays);
    if (deleted.length > 0 && process.env.AGENT_BROWSER_DEBUG === '1') {
      console.error(
        `[DEBUG] Auto-expired ${deleted.length} state file(s) older than ${expireDays} days`
      );
    }
  } catch (err) {
    if (process.env.AGENT_BROWSER_DEBUG === '1') {
      console.error(`[DEBUG] Failed to clean up expired states:`, err);
    }
  }
}

/**
 * Get the validated session name and auto-state file path.
 * Centralizes session name validation to prevent path traversal.
 */
function getSessionAutoStatePath(): string | undefined {
  const sessionNameRaw = process.env.AGENT_BROWSER_SESSION_NAME;
  if (!sessionNameRaw) return undefined;

  if (!isValidSessionName(sessionNameRaw)) {
    if (process.env.AGENT_BROWSER_DEBUG === '1') {
      console.error(`[SECURITY] Invalid session name rejected: ${sessionNameRaw}`);
    }
    return undefined;
  }

  const sessionId = process.env.AGENT_BROWSER_SESSION || 'default';
  try {
    const autoStatePath = getAutoStateFilePath(sessionNameRaw, sessionId);
    return autoStatePath && fs.existsSync(autoStatePath) ? autoStatePath : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Get the auto-state file path for saving (creates sessions dir if needed).
 * Returns undefined if no valid session name is configured.
 */
function getSessionSaveStatePath(): string | undefined {
  const sessionNameRaw = process.env.AGENT_BROWSER_SESSION_NAME;
  if (!sessionNameRaw) return undefined;

  if (!isValidSessionName(sessionNameRaw)) return undefined;

  const sessionId = process.env.AGENT_BROWSER_SESSION || 'default';
  try {
    return getAutoStateFilePath(sessionNameRaw, sessionId) ?? undefined;
  } catch {
    return undefined;
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
 * Get the base directory for socket/pid files.
 * Priority: AGENT_BROWSER_SOCKET_DIR > XDG_RUNTIME_DIR > ~/.agent-browser > tmpdir
 */
export function getAppDir(): string {
  // 1. XDG_RUNTIME_DIR (Linux standard)
  if (process.env.XDG_RUNTIME_DIR) {
    return path.join(process.env.XDG_RUNTIME_DIR, 'agent-browser');
  }

  // 2. Home directory fallback (like Docker Desktop's ~/.docker/run/)
  const homeDir = os.homedir();
  if (homeDir) {
    return path.join(homeDir, '.agent-browser');
  }

  // 3. Last resort: temp dir
  return path.join(os.tmpdir(), 'agent-browser');
}

export function getSocketDir(): string {
  // Allow explicit override for socket directory
  if (process.env.AGENT_BROWSER_SOCKET_DIR) {
    return process.env.AGENT_BROWSER_SOCKET_DIR;
  }
  return getAppDir();
}

/**
 * Get the socket path for the current session (Unix) or port (Windows)
 */
export function getSocketPath(session?: string): string {
  const sess = session ?? currentSession;
  if (isWindows) {
    return String(getPortForSession(sess));
  }
  return path.join(getSocketDir(), `${sess}.sock`);
}

/**
 * Get the port file path for Windows (stores the port number)
 */
export function getPortFile(session?: string): string {
  const sess = session ?? currentSession;
  return path.join(getSocketDir(), `${sess}.port`);
}

/**
 * Get the PID file path for the current session
 */
export function getPidFile(session?: string): string {
  const sess = session ?? currentSession;
  return path.join(getSocketDir(), `${sess}.pid`);
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
  return { type: 'unix', path: path.join(getSocketDir(), `${sess}.sock`) };
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
  return path.join(getSocketDir(), `${sess}.stream`);
}

/**
 * Start the daemon server
 * @param options.streamPort Port for WebSocket stream server (0 to disable)
 * @param options.provider Provider type ('ios' for iOS Simulator, undefined for desktop)
 */
export async function startDaemon(options?: {
  streamPort?: number;
  provider?: string;
}): Promise<void> {
  // Ensure socket directory exists with restricted permissions (owner-only access)
  const socketDir = getSocketDir();
  if (!fs.existsSync(socketDir)) {
    fs.mkdirSync(socketDir, { recursive: true, mode: 0o700 });
  }

  // Clean up any stale socket
  cleanupSocket();

  // Clean up expired state files on startup
  runCleanupExpiredStates();

  // Determine provider from options or environment
  const provider = options?.provider ?? process.env.AGENT_BROWSER_PROVIDER;
  const isIOS = provider === 'ios';

  // Create appropriate manager
  const manager: Manager = isIOS ? new IOSManager() : new BrowserManager();
  let shuttingDown = false;

  // Start stream server if port is specified (or use default if env var is set)
  // Note: Stream server only works with BrowserManager (desktop), not iOS
  const streamPort =
    options?.streamPort ??
    (process.env.AGENT_BROWSER_STREAM_PORT
      ? parseInt(process.env.AGENT_BROWSER_STREAM_PORT, 10)
      : 0);

  if (streamPort > 0 && !isIOS && manager instanceof BrowserManager) {
    streamServer = new StreamServer(manager, streamPort);
    await streamServer.start();

    // Write stream port to file for clients to discover
    const streamPortFile = getStreamPortFile();
    fs.writeFileSync(streamPortFile, streamPort.toString());
  }

  const server = net.createServer((socket) => {
    let buffer = '';
    let httpChecked = false;

    // Command serialization: queue incoming lines and process them one at a time.
    // This prevents concurrent command execution which can cause socket.write
    // buffer contention and EAGAIN errors on the Rust CLI side.
    const commandQueue: string[] = [];
    let processing = false;

    async function processQueue(): Promise<void> {
      if (processing) return;
      processing = true;

      while (commandQueue.length > 0) {
        const line = commandQueue.shift()!;

        try {
          const parseResult = parseCommand(line);

          if (!parseResult.success) {
            const resp = errorResponse(parseResult.id ?? 'unknown', parseResult.error);
            await safeWrite(socket, serializeResponse(resp) + '\n');
            continue;
          }

          // Handle device_list specially - it works without a session and always uses IOSManager
          if (parseResult.command.action === 'device_list') {
            const iosManager = new IOSManager();
            try {
              const devices = await iosManager.listAllDevices();
              const response = {
                id: parseResult.command.id,
                success: true as const,
                data: { devices },
              };
              await safeWrite(socket, serializeResponse(response) + '\n');
            } catch (err) {
              const message = err instanceof Error ? err.message : String(err);
              await safeWrite(
                socket,
                serializeResponse(errorResponse(parseResult.command.id, message)) + '\n'
              );
            }
            continue;
          }

          // Auto-launch if not already launched and this isn't a launch/close/state_load command
          if (
            !manager.isLaunched() &&
            parseResult.command.action !== 'launch' &&
            parseResult.command.action !== 'close' &&
            parseResult.command.action !== 'state_load'
          ) {
            if (isIOS && manager instanceof IOSManager) {
              // Auto-launch iOS Safari
              // Check for device in command first (for reused daemons), then fall back to env vars
              const cmd = parseResult.command as { iosDevice?: string };
              const iosDevice = cmd.iosDevice || process.env.AGENT_BROWSER_IOS_DEVICE;
              await manager.launch({
                device: iosDevice,
                udid: process.env.AGENT_BROWSER_IOS_UDID,
              });
            } else if (manager instanceof BrowserManager) {
              // Auto-launch desktop browser
              const extensions = process.env.AGENT_BROWSER_EXTENSIONS
                ? process.env.AGENT_BROWSER_EXTENSIONS.split(',')
                    .map((p) => p.trim())
                    .filter(Boolean)
                : undefined;

              // Parse args from env (comma or newline separated)
              const argsEnv = process.env.AGENT_BROWSER_ARGS;
              const args = argsEnv
                ? argsEnv
                    .split(/[,\n]/)
                    .map((a) => a.trim())
                    .filter((a) => a.length > 0)
                : undefined;

              // Parse proxy from env
              const proxyServer = process.env.AGENT_BROWSER_PROXY;
              const proxyBypass = process.env.AGENT_BROWSER_PROXY_BYPASS;
              const proxy = proxyServer
                ? {
                    server: proxyServer,
                    ...(proxyBypass && { bypass: proxyBypass }),
                  }
                : undefined;

              const ignoreHTTPSErrors = process.env.AGENT_BROWSER_IGNORE_HTTPS_ERRORS === '1';
              const allowFileAccess = process.env.AGENT_BROWSER_ALLOW_FILE_ACCESS === '1';
              const colorSchemeEnv = process.env.AGENT_BROWSER_COLOR_SCHEME;
              const colorScheme =
                colorSchemeEnv === 'dark' ||
                colorSchemeEnv === 'light' ||
                colorSchemeEnv === 'no-preference'
                  ? colorSchemeEnv
                  : undefined;
              await manager.launch({
                id: 'auto',
                action: 'launch' as const,
                headless: process.env.AGENT_BROWSER_HEADED !== '1',
                executablePath: process.env.AGENT_BROWSER_EXECUTABLE_PATH,
                extensions: extensions,
                profile: process.env.AGENT_BROWSER_PROFILE,
                storageState: process.env.AGENT_BROWSER_STATE,
                args,
                userAgent: process.env.AGENT_BROWSER_USER_AGENT,
                proxy,
                ignoreHTTPSErrors: ignoreHTTPSErrors,
                allowFileAccess: allowFileAccess,
                colorScheme,
                autoStateFilePath: getSessionAutoStatePath(),
              });
            }
          }

          // Recover from stale state: browser is launched but all pages were closed
          if (
            manager instanceof BrowserManager &&
            manager.isLaunched() &&
            !manager.hasPages() &&
            parseResult.command.action !== 'launch' &&
            parseResult.command.action !== 'close'
          ) {
            await manager.ensurePage();
          }

          // Handle explicit launch with auto-load state
          if (
            parseResult.command.action === 'launch' &&
            manager instanceof BrowserManager &&
            !parseResult.command.autoStateFilePath
          ) {
            const autoStatePath = getSessionAutoStatePath();
            if (autoStatePath) {
              parseResult.command.autoStateFilePath = autoStatePath;
            }
          }

          // Handle close command specially - shuts down daemon
          if (parseResult.command.action === 'close') {
            // Auto-save state before closing
            if (manager instanceof BrowserManager && manager.isLaunched()) {
              const savePath = getSessionSaveStatePath();
              if (savePath) {
                try {
                  const { encrypted } = await saveStateToFile(manager, savePath);
                  fs.chmodSync(savePath, 0o600);
                  if (process.env.AGENT_BROWSER_DEBUG === '1') {
                    console.error(
                      `Auto-saved session state: ${savePath}${encrypted ? ' (encrypted)' : ''}`
                    );
                  }
                } catch (err) {
                  if (process.env.AGENT_BROWSER_DEBUG === '1') {
                    console.error(`Failed to auto-save session state:`, err);
                  }
                }
              }
            }

            const response =
              isIOS && manager instanceof IOSManager
                ? await executeIOSCommand(parseResult.command, manager)
                : await executeCommand(parseResult.command, manager as BrowserManager);
            await safeWrite(socket, serializeResponse(response) + '\n');

            if (!shuttingDown) {
              shuttingDown = true;
              setTimeout(() => {
                server.close();
                cleanupSocket();
                process.exit(0);
              }, 100);
            }

            commandQueue.length = 0;
            processing = false;
            return;
          }

          // Execute command with appropriate handler
          const response =
            isIOS && manager instanceof IOSManager
              ? await executeIOSCommand(parseResult.command, manager)
              : await executeCommand(parseResult.command, manager as BrowserManager);

          // Add any launch warnings to the response
          if (manager instanceof BrowserManager) {
            const warnings = manager.getAndClearWarnings();
            if (warnings.length > 0 && response.success && response.data) {
              (response.data as Record<string, unknown>).warnings = warnings;
            }
          }

          await safeWrite(socket, serializeResponse(response) + '\n');
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          await safeWrite(socket, serializeResponse(errorResponse('error', message)) + '\n').catch(
            () => {}
          ); // Socket may already be destroyed
        }
      }

      processing = false;
    }

    socket.on('data', (data) => {
      buffer += data.toString();

      // Security: Detect and reject HTTP requests to prevent cross-origin attacks.
      // Browsers using fetch() must send HTTP headers (e.g., "POST / HTTP/1.1"),
      // while legitimate clients send raw JSON starting with "{".
      if (!httpChecked) {
        httpChecked = true;
        const trimmed = buffer.trimStart();
        if (/^(GET|POST|PUT|DELETE|HEAD|OPTIONS|PATCH|CONNECT|TRACE)\s/i.test(trimmed)) {
          socket.destroy();
          return;
        }
      }

      // Extract complete lines and enqueue them for serial processing
      while (buffer.includes('\n')) {
        const newlineIdx = buffer.indexOf('\n');
        const line = buffer.substring(0, newlineIdx);
        buffer = buffer.substring(newlineIdx + 1);

        if (!line.trim()) continue;
        commandQueue.push(line);
      }

      processQueue().catch((err) => {
        // Socket write failures during queue processing are non-fatal;
        // the client has likely disconnected.
        console.warn('[warn] processQueue error:', err?.message ?? err);
        if (process.env.AGENT_BROWSER_DEBUG === '1') {
          console.error('[DEBUG] processQueue error (full):', err);
        }
      });
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

    await manager.close();
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
