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

// Manager type - either desktop browser or iOS
type Manager = BrowserManager | IOSManager;

// Platform detection
const isWindows = process.platform === 'win32';

// Session support - each session gets its own socket/pid
let currentSession = process.env.AGENT_BROWSER_SESSION || 'default';

// Stream server for browser preview
let streamServer: StreamServer | null = null;

// Default stream port (can be overridden with AGENT_BROWSER_STREAM_PORT)
const DEFAULT_STREAM_PORT = 9223;

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
  // Only use if the directory is writable (avoids WSL permission issues)
  if (process.env.XDG_RUNTIME_DIR) {
    const candidate = path.join(process.env.XDG_RUNTIME_DIR, 'agent-browser');
    try {
      fs.mkdirSync(candidate, { recursive: true });
      const testFile = path.join(candidate, '.write_test');
      fs.writeFileSync(testFile, '');
      fs.unlinkSync(testFile);
      return candidate;
    } catch {
      // Fall through to home directory
    }
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
  // Ensure socket directory exists
  const socketDir = getSocketDir();
  if (!fs.existsSync(socketDir)) {
    fs.mkdirSync(socketDir, { recursive: true });
  }

  // Clean up any stale socket
  cleanupSocket();

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

    socket.on('data', async (data) => {
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

      // Process complete lines
      while (buffer.includes('\n')) {
        const newlineIdx = buffer.indexOf('\n');
        const line = buffer.substring(0, newlineIdx);
        buffer = buffer.substring(newlineIdx + 1);

        if (!line.trim()) continue;

        try {
          const parseResult = parseCommand(line);

          if (!parseResult.success) {
            const resp = errorResponse(parseResult.id ?? 'unknown', parseResult.error);
            socket.write(serializeResponse(resp) + '\n');
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
              socket.write(serializeResponse(response) + '\n');
            } catch (err) {
              const message = err instanceof Error ? err.message : String(err);
              socket.write(
                serializeResponse(errorResponse(parseResult.command.id, message)) + '\n'
              );
            }
            continue;
          }

          // Auto-launch if not already launched and this isn't a launch/close command
          if (
            !manager.isLaunched() &&
            parseResult.command.action !== 'launch' &&
            parseResult.command.action !== 'close'
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
              });
            }
          }

          // Handle close command specially - shuts down daemon
          if (parseResult.command.action === 'close') {
            const response =
              isIOS && manager instanceof IOSManager
                ? await executeIOSCommand(parseResult.command, manager)
                : await executeCommand(parseResult.command, manager as BrowserManager);
            socket.write(serializeResponse(response) + '\n');

            if (!shuttingDown) {
              shuttingDown = true;
              // Give close() (already called by executeCommand) time to finish,
              // then cleanup and exit
              setTimeout(() => {
                server.close();
                cleanupSocket();
                process.exit(0);
              }, 1000);
            }
            return;
          }

          // Execute command with appropriate handler
          const response =
            isIOS && manager instanceof IOSManager
              ? await executeIOSCommand(parseResult.command, manager)
              : await executeCommand(parseResult.command, manager as BrowserManager);
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
    // Try the computed port first, then retry with alternatives if it's in an excluded range
    const basePort = getPortForSession(currentSession);
    const portFile = getPortFile();
    const maxRetries = 10;

    const tryListen = (port: number, attempt: number) => {
      server.listen(port, '127.0.0.1', () => {
        fs.writeFileSync(portFile, port.toString());
      });
      server.once('error', (err: NodeJS.ErrnoException) => {
        if (err.code === 'EACCES' && attempt < maxRetries) {
          // Port is in Windows excluded range, try next port
          server.removeAllListeners('error');
          tryListen(port + 1 > 65535 ? 49152 : port + 1, attempt + 1);
        } else {
          console.error('Server error:', err);
          cleanupSocket();
          process.exit(1);
        }
      });
    };
    tryListen(basePort, 0);
  } else {
    // Unix: use Unix domain socket
    const socketPath = getSocketPath();
    server.listen(socketPath, () => {
      // Daemon is ready
    });

    server.on('error', (err) => {
      console.error('Server error:', err);
      cleanupSocket();
      process.exit(1);
    });
  }

  // Handle shutdown signals
  const shutdown = async () => {
    if (shuttingDown) return;
    shuttingDown = true;

    // Force exit after 5 seconds if graceful shutdown hangs
    const forceExit = setTimeout(() => {
      cleanupSocket();
      process.exit(1);
    }, 5000);

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
    clearTimeout(forceExit);
    server.close();
    cleanupSocket();
    process.exit(0);
  };

  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
  process.on('SIGHUP', shutdown);

  // Handle unexpected errors - always cleanup including browser
  process.on('uncaughtException', async (err) => {
    console.error('Uncaught exception:', err);
    if (shuttingDown) {
      process.exit(1);
      return;
    }
    shuttingDown = true;
    // Force exit after 3s if close hangs
    const forceExit = setTimeout(() => {
      cleanupSocket();
      process.exit(1);
    }, 3000);
    try {
      await manager.close();
    } catch {
      // Ignore close errors during crash cleanup
    }
    clearTimeout(forceExit);
    cleanupSocket();
    process.exit(1);
  });

  process.on('unhandledRejection', async (reason) => {
    console.error('Unhandled rejection:', reason);
    if (shuttingDown) {
      process.exit(1);
      return;
    }
    shuttingDown = true;
    const forceExit = setTimeout(() => {
      cleanupSocket();
      process.exit(1);
    }, 3000);
    try {
      await manager.close();
    } catch {
      // Ignore close errors during crash cleanup
    }
    clearTimeout(forceExit);
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
