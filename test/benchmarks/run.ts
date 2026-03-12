import { spawn, ChildProcess } from "child_process";
import * as http from "http";
import * as net from "net";
import * as os from "os";
import * as path from "path";
import * as fs from "fs";
import { fileURLToPath } from "url";
import { scenarios, type BenchmarkCommand, type Scenario } from "./scenarios.js";
import { engineScenarios } from "./engine-scenarios.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// ---------------------------------------------------------------------------
// Static file server for HTTP-served benchmarks
// ---------------------------------------------------------------------------

const PAGES_DIR = path.join(__dirname, "pages");

const MIME_TYPES: Record<string, string> = {
  ".html": "text/html",
  ".css": "text/css",
  ".js": "application/javascript",
  ".json": "application/json",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".svg": "image/svg+xml",
};

function startFileServer(): Promise<{ server: http.Server; port: number }> {
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const url = new URL(req.url || "/", `http://localhost`);
      let filePath = path.join(PAGES_DIR, url.pathname === "/" ? "article.html" : url.pathname);

      if (!filePath.startsWith(PAGES_DIR)) {
        res.writeHead(403);
        res.end();
        return;
      }

      if (fs.existsSync(filePath) && fs.statSync(filePath).isDirectory()) {
        filePath = path.join(filePath, "index.html");
      }

      try {
        const content = fs.readFileSync(filePath);
        const ext = path.extname(filePath);
        res.writeHead(200, { "Content-Type": MIME_TYPES[ext] || "application/octet-stream" });
        res.end(content);
      } catch {
        res.writeHead(404);
        res.end("Not found");
      }
    });

    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      if (!addr || typeof addr === "string") {
        reject(new Error("Failed to get server address"));
        return;
      }
      resolve({ server, port: addr.port });
    });

    server.on("error", reject);
  });
}

function stopFileServer(server: http.Server): Promise<void> {
  return new Promise((resolve) => {
    server.close(() => resolve());
  });
}

// ---------------------------------------------------------------------------
// Memory measurement via /proc or ps
// ---------------------------------------------------------------------------

function getProcessMemoryKB(pid: number): number | null {
  if (process.platform === "linux") {
    try {
      const status = fs.readFileSync(`/proc/${pid}/status`, "utf-8");
      const match = status.match(/VmRSS:\s+(\d+)\s+kB/);
      if (match) return parseInt(match[1], 10);
    } catch { /* */ }
  }

  try {
    const { execSync } = require("child_process");
    const output = execSync(`ps -o rss= -p ${pid}`, { encoding: "utf-8", timeout: 2000 });
    const kb = parseInt(output.trim(), 10);
    if (!isNaN(kb)) return kb;
  } catch { /* */ }

  return null;
}

function sampleMemory(pids: number[], intervalMs: number): { stop: () => number } {
  let peakKB = 0;
  const timer = setInterval(() => {
    for (const pid of pids) {
      const kb = getProcessMemoryKB(pid);
      if (kb && kb > peakKB) peakKB = kb;
    }
  }, intervalMs);

  return {
    stop() {
      clearInterval(timer);
      for (const pid of pids) {
        const kb = getProcessMemoryKB(pid);
        if (kb && kb > peakKB) peakKB = kb;
      }
      return peakKB;
    },
  };
}

function formatMemory(kb: number): string {
  if (kb >= 1024 * 1024) return `${(kb / 1024 / 1024).toFixed(1)}GB`;
  if (kb >= 1024) return `${(kb / 1024).toFixed(1)}MB`;
  return `${kb}KB`;
}

// ---------------------------------------------------------------------------
// Socket / daemon helpers
// ---------------------------------------------------------------------------

function getSocketDir(): string {
  if (process.env.AGENT_BROWSER_SOCKET_DIR) {
    return process.env.AGENT_BROWSER_SOCKET_DIR;
  }
  if (process.env.XDG_RUNTIME_DIR) {
    return path.join(process.env.XDG_RUNTIME_DIR, "agent-browser");
  }
  const home = os.homedir();
  if (home) {
    return path.join(home, ".agent-browser");
  }
  return path.join(os.tmpdir(), "agent-browser");
}

function getSocketPath(session: string): string {
  return path.join(getSocketDir(), `${session}.sock`);
}

function getProjectRoot(): string {
  return path.resolve(__dirname, "../..");
}

function getNativeBinaryPath(): string {
  const root = getProjectRoot();
  const p = os.platform();
  const a = os.arch();

  const osKey =
    p === "darwin" ? "darwin" : p === "linux" ? "linux" : p === "win32" ? "win32" : null;
  const archKey =
    a === "x64" || a === "x86_64" ? "x64" : a === "arm64" || a === "aarch64" ? "arm64" : null;

  if (!osKey || !archKey) {
    throw new Error(`Unsupported platform: ${p}-${a}`);
  }

  const ext = p === "win32" ? ".exe" : "";
  const binName = `agent-browser-${osKey}-${archKey}${ext}`;

  const candidates = [
    path.join(root, "cli/target/release/agent-browser"),
    path.join(root, "cli/target/debug/agent-browser"),
    path.join(root, "bin", binName),
  ];

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) return candidate;
  }

  throw new Error(
    `Native binary not found. Tried:\n${candidates.map((c) => "  " + c).join("\n")}\n` +
      'Run "pnpm build:native" to build the native binary.',
  );
}

function sendCommand(session: string, cmd: BenchmarkCommand): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const socketPath = getSocketPath(session);
    const client = net.createConnection({ path: socketPath }, () => {
      client.write(JSON.stringify(cmd) + "\n");
    });

    let data = "";
    client.on("data", (chunk) => {
      data += chunk.toString();
      const newlineIdx = data.indexOf("\n");
      if (newlineIdx !== -1) {
        const line = data.slice(0, newlineIdx);
        client.destroy();
        try {
          resolve(JSON.parse(line));
        } catch {
          reject(new Error(`Invalid JSON response: ${line}`));
        }
      }
    });

    client.on("error", (err) => reject(err));
    client.on("timeout", () => {
      client.destroy();
      reject(new Error("Socket timeout"));
    });
    client.setTimeout(30_000);
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForSocket(session: string, timeoutMs = 15_000): Promise<void> {
  const start = Date.now();
  const socketPath = getSocketPath(session);
  while (Date.now() - start < timeoutMs) {
    if (fs.existsSync(socketPath)) {
      try {
        await new Promise<void>((resolve, reject) => {
          const c = net.createConnection({ path: socketPath }, () => {
            c.destroy();
            resolve();
          });
          c.on("error", reject);
          c.setTimeout(1000);
          c.on("timeout", () => {
            c.destroy();
            reject(new Error("timeout"));
          });
        });
        return;
      } catch {
        // not ready yet
      }
    }
    await sleep(100);
  }
  throw new Error(`Daemon '${session}' did not become ready within ${timeoutMs}ms`);
}

interface DaemonHandle {
  session: string;
  process: ChildProcess;
}

function spawnNodeDaemon(session: string): DaemonHandle {
  const daemonPath = path.join(getProjectRoot(), "dist/daemon.js");
  if (!fs.existsSync(daemonPath)) {
    throw new Error(`Node daemon not found at ${daemonPath}. Run "pnpm build" first.`);
  }

  const child = spawn("node", [daemonPath], {
    env: {
      ...process.env,
      AGENT_BROWSER_DAEMON: "1",
      AGENT_BROWSER_SESSION: session,
    },
    stdio: ["ignore", "ignore", "pipe"],
    detached: true,
  });

  child.stderr?.on("data", (chunk) => {
    const msg = chunk.toString().trim();
    if (msg && process.env.BENCH_DEBUG) {
      process.stderr.write(`[node-daemon] ${msg}\n`);
    }
  });

  return { session, process: child };
}

function spawnNativeDaemon(session: string, engine?: string): DaemonHandle {
  const binaryPath = getNativeBinaryPath();

  const env: Record<string, string> = {
    ...process.env as Record<string, string>,
    AGENT_BROWSER_DAEMON: "1",
    AGENT_BROWSER_SESSION: session,
  };
  if (engine) {
    env.AGENT_BROWSER_ENGINE = engine;
  }

  const child = spawn(binaryPath, [], {
    env,
    stdio: ["ignore", "ignore", "pipe"],
    detached: true,
  });

  const label = engine ? `native-${engine}` : "native-daemon";
  child.stderr?.on("data", (chunk) => {
    const msg = chunk.toString().trim();
    if (msg && process.env.BENCH_DEBUG) {
      process.stderr.write(`[${label}] ${msg}\n`);
    }
  });

  return { session, process: child };
}

async function closeDaemon(handle: DaemonHandle): Promise<void> {
  try {
    await sendCommand(handle.session, { id: "close", action: "close" });
  } catch {
    // daemon may already be gone
  }
  await sleep(200);
  try {
    handle.process.kill("SIGTERM");
  } catch {
    // already exited
  }
}

function cleanupSockets(): void {
  for (const session of [
    "bench-node",
    "bench-native",
    "bench-chrome",
    "bench-lightpanda",
  ]) {
    const sockPath = getSocketPath(session);
    const pidPath = sockPath.replace(/\.sock$/, ".pid");
    try {
      fs.unlinkSync(sockPath);
    } catch {
      /* */
    }
    try {
      fs.unlinkSync(pidPath);
    } catch {
      /* */
    }
  }
}

// ---------------------------------------------------------------------------
// Statistics (microsecond precision)
// ---------------------------------------------------------------------------

interface Stats {
  avgUs: number;
  minUs: number;
  maxUs: number;
  p50Us: number;
  p95Us: number;
}

function computeStats(timingsUs: number[]): Stats {
  const sorted = [...timingsUs].sort((a, b) => a - b);
  const sum = sorted.reduce((a, b) => a + b, 0);
  return {
    avgUs: Math.round(sum / sorted.length),
    minUs: sorted[0],
    maxUs: sorted[sorted.length - 1],
    p50Us: sorted[Math.floor(sorted.length * 0.5)],
    p95Us: sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))],
  };
}

function formatDuration(us: number): string {
  if (us >= 1_000_000) return `${(us / 1_000_000).toFixed(2)}s`;
  if (us >= 1_000) return `${(us / 1_000).toFixed(1)}ms`;
  return `${us}us`;
}

// ---------------------------------------------------------------------------
// Benchmark runner
// ---------------------------------------------------------------------------

async function runCommands(session: string, commands: BenchmarkCommand[]): Promise<void> {
  for (const cmd of commands) {
    const resp = await sendCommand(session, cmd);
    if (!(resp as { success?: boolean }).success) {
      throw new Error(
        `Command '${cmd.action}' failed on session '${session}': ${JSON.stringify(resp)}`,
      );
    }
  }
}

async function timeCommands(session: string, commands: BenchmarkCommand[]): Promise<number> {
  const start = process.hrtime.bigint();
  await runCommands(session, commands);
  const elapsedNs = process.hrtime.bigint() - start;
  return Number(elapsedNs / 1000n); // microseconds
}

interface ScenarioResult {
  name: string;
  nodeStats: Stats | null;
  nativeStats: Stats | null;
  chromeStats: Stats | null;
  lightpandaStats: Stats | null;
}

async function runScenario(
  scenario: Scenario,
  sessions: { node?: string; native?: string },
  iterations: number,
  warmup: number,
): Promise<ScenarioResult> {
  const result: ScenarioResult = {
    name: scenario.name,
    nodeStats: null,
    nativeStats: null,
    chromeStats: null,
    lightpandaStats: null,
  };

  for (const [label, session] of Object.entries(sessions)) {
    if (!session) continue;

    if (scenario.setup) {
      await runCommands(session, scenario.setup);
    }

    for (let i = 0; i < warmup; i++) {
      await timeCommands(session, scenario.commands);
    }

    const timings: number[] = [];
    for (let i = 0; i < iterations; i++) {
      timings.push(await timeCommands(session, scenario.commands));
    }

    if (scenario.teardown) {
      await runCommands(session, scenario.teardown);
    }

    const stats = computeStats(timings);
    if (label === "node") result.nodeStats = stats;
    else if (label === "native") result.nativeStats = stats;
    else if (label === "chrome") result.chromeStats = stats;
    else if (label === "lightpanda") result.lightpandaStats = stats;
  }

  return result;
}

async function runScenarioWithErrorTolerance(
  scenario: Scenario,
  sessions: Record<string, string>,
  iterations: number,
  warmup: number,
): Promise<ScenarioResult> {
  const result: ScenarioResult = {
    name: scenario.name,
    nodeStats: null,
    nativeStats: null,
    chromeStats: null,
    lightpandaStats: null,
  };

  for (const [label, session] of Object.entries(sessions)) {
    if (!session) continue;

    try {
      if (scenario.setup) {
        await runCommands(session, scenario.setup);
      }

      for (let i = 0; i < warmup; i++) {
        await timeCommands(session, scenario.commands);
      }

      const timings: number[] = [];
      for (let i = 0; i < iterations; i++) {
        timings.push(await timeCommands(session, scenario.commands));
      }

      if (scenario.teardown) {
        await runCommands(session, scenario.teardown);
      }

      const stats = computeStats(timings);
      if (label === "chrome") result.chromeStats = stats;
      else if (label === "lightpanda") result.lightpandaStats = stats;
      else if (label === "node") result.nodeStats = stats;
      else if (label === "native") result.nativeStats = stats;
    } catch (err) {
      if (process.env.BENCH_DEBUG) {
        const msg = err instanceof Error ? err.message : String(err);
        process.stderr.write(`  [${label}] scenario '${scenario.name}' failed: ${msg}\n`);
      }
    }
  }

  return result;
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

function pad(s: string, len: number): string {
  return s.padEnd(len);
}

function rpad(s: string, len: number): string {
  return s.padStart(len);
}

function formatSpeedup(baselineUs: number, candidateUs: number): string {
  if (candidateUs === 0 && baselineUs === 0) return "  --";
  if (candidateUs === 0) return "  >>>";
  const ratio = baselineUs / candidateUs;
  return `${ratio.toFixed(1)}x`;
}

type BenchmarkMode = "daemon" | "engine";

function printResults(
  results: ScenarioResult[],
  iterations: number,
  warmup: number,
  mode: BenchmarkMode = "daemon",
): void {
  console.log("");

  if (mode === "engine") {
    printEngineResults(results, iterations, warmup);
    return;
  }

  const bothPaths = results[0].nodeStats !== null && results[0].nativeStats !== null;

  const header = bothPaths
    ? `agent-browser benchmark: node vs native (${iterations} iterations, ${warmup} warmup)`
    : `agent-browser benchmark (${iterations} iterations, ${warmup} warmup)`;
  console.log(header);
  console.log("=".repeat(header.length));
  console.log("");

  if (bothPaths) {
    const nameW = 20;
    const colW = 14;

    console.log(
      pad("Scenario", nameW) +
        rpad("Node (avg)", colW) +
        rpad("Native (avg)", colW) +
        rpad("Speedup", 10),
    );
    console.log("-".repeat(nameW + colW * 2 + 10));

    let totalNodeUs = 0;
    let totalNativeUs = 0;
    let count = 0;

    for (const r of results) {
      if (!r.nodeStats || !r.nativeStats) continue;
      totalNodeUs += r.nodeStats.avgUs;
      totalNativeUs += r.nativeStats.avgUs;
      count++;

      console.log(
        pad(r.name, nameW) +
          rpad(formatDuration(r.nodeStats.avgUs), colW) +
          rpad(formatDuration(r.nativeStats.avgUs), colW) +
          rpad(formatSpeedup(r.nodeStats.avgUs, r.nativeStats.avgUs), 10),
      );
    }

    console.log("-".repeat(nameW + colW * 2 + 10));

    if (count > 0 && totalNativeUs > 0) {
      const overallSpeedup = totalNodeUs / totalNativeUs;
      const winner = overallSpeedup >= 1.0 ? "native is faster" : "node is faster";
      console.log(`Overall average speedup: ${overallSpeedup.toFixed(1)}x (${winner})`);
      console.log("");

      const allNativeFaster = results.every(
        (r) => !r.nodeStats || !r.nativeStats || r.nodeStats.avgUs >= r.nativeStats.avgUs,
      );
      if (allNativeFaster) {
        console.log("Result: PASS -- native is faster across all scenarios");
      } else {
        const slower = results
          .filter((r) => r.nodeStats && r.nativeStats && r.nodeStats.avgUs < r.nativeStats.avgUs)
          .map((r) => r.name);
        console.log(`Result: WARN -- native is slower in: ${slower.join(", ")}`);
      }
    }
  } else {
    const nameW = 20;
    const label = results[0].nodeStats ? "Node" : "Native";
    console.log(
      pad("Scenario", nameW) +
        rpad(`${label} avg`, 10) +
        rpad("min", 10) +
        rpad("max", 10) +
        rpad("p50", 10) +
        rpad("p95", 10),
    );
    console.log("-".repeat(nameW + 50));
    for (const r of results) {
      const s = r.nodeStats ?? r.nativeStats;
      if (!s) continue;
      console.log(
        pad(r.name, nameW) +
          rpad(formatDuration(s.avgUs), 10) +
          rpad(formatDuration(s.minUs), 10) +
          rpad(formatDuration(s.maxUs), 10) +
          rpad(formatDuration(s.p50Us), 10) +
          rpad(formatDuration(s.p95Us), 10),
      );
    }
  }

  console.log("");
}

function printEngineResults(
  results: ScenarioResult[],
  iterations: number,
  warmup: number,
): void {
  const header = `agent-browser benchmark: chrome vs lightpanda (${iterations} iterations, ${warmup} warmup)`;
  console.log(header);
  console.log("=".repeat(header.length));
  console.log("");

  const nameW = 22;
  const colW = 18;

  console.log(
    pad("Scenario", nameW) +
      rpad("Chrome (avg)", colW) +
      rpad("Lightpanda (avg)", colW) +
      rpad("Speedup", 10),
  );
  console.log("-".repeat(nameW + colW * 2 + 10));

  let totalChromeUs = 0;
  let totalLightpandaUs = 0;
  let comparableCount = 0;

  for (const r of results) {
    const chromeAvg = r.chromeStats ? formatDuration(r.chromeStats.avgUs) : "N/A";
    const lpAvg = r.lightpandaStats ? formatDuration(r.lightpandaStats.avgUs) : "N/A";
    let speedup = "  --";

    if (r.chromeStats && r.lightpandaStats) {
      totalChromeUs += r.chromeStats.avgUs;
      totalLightpandaUs += r.lightpandaStats.avgUs;
      comparableCount++;
      speedup = formatSpeedup(r.chromeStats.avgUs, r.lightpandaStats.avgUs);
    }

    console.log(
      pad(r.name, nameW) +
        rpad(chromeAvg, colW) +
        rpad(lpAvg, colW) +
        rpad(speedup, 10),
    );
  }

  console.log("-".repeat(nameW + colW * 2 + 10));

  if (comparableCount > 0 && totalLightpandaUs > 0) {
    const ratio = totalChromeUs / totalLightpandaUs;
    const winner = ratio >= 1.0
      ? `lightpanda ${ratio.toFixed(1)}x faster`
      : `chrome ${(1 / ratio).toFixed(1)}x faster`;
    console.log(`Overall: ${winner}`);
  }

  console.log("");
}

function writeJsonResults(
  results: ScenarioResult[],
  outputPath: string,
  mode: BenchmarkMode = "daemon",
): void {
  const toMs = (us: number) => +(us / 1000).toFixed(2);
  const statsToJson = (s: Stats) => ({
    avg_ms: toMs(s.avgUs),
    min_ms: toMs(s.minUs),
    max_ms: toMs(s.maxUs),
    p50_ms: toMs(s.p50Us),
    p95_ms: toMs(s.p95Us),
  });

  const json = results.map((r) => {
    if (mode === "engine") {
      return {
        scenario: r.name,
        chrome: r.chromeStats ? statsToJson(r.chromeStats) : null,
        lightpanda: r.lightpandaStats ? statsToJson(r.lightpandaStats) : null,
        speedup:
          r.chromeStats && r.lightpandaStats && r.lightpandaStats.avgUs > 0
            ? +(r.chromeStats.avgUs / r.lightpandaStats.avgUs).toFixed(2)
            : null,
      };
    }
    return {
      scenario: r.name,
      node: r.nodeStats ? statsToJson(r.nodeStats) : null,
      native: r.nativeStats ? statsToJson(r.nativeStats) : null,
      speedup:
        r.nodeStats && r.nativeStats && r.nativeStats.avgUs > 0
          ? +(r.nodeStats.avgUs / r.nativeStats.avgUs).toFixed(2)
          : null,
    };
  });
  fs.writeFileSync(outputPath, JSON.stringify(json, null, 2) + "\n");
  console.log(`JSON results written to ${outputPath}`);
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

interface CliArgs {
  iterations: number;
  warmup: number;
  nodeOnly: boolean;
  nativeOnly: boolean;
  engineMode: boolean;
  json: boolean;
}

function parseArgs(): CliArgs {
  const args = process.argv.slice(2);
  const result: CliArgs = {
    iterations: 10,
    warmup: 3,
    nodeOnly: false,
    nativeOnly: false,
    engineMode: false,
    json: false,
  };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--iterations":
        result.iterations = parseInt(args[++i], 10);
        break;
      case "--warmup":
        result.warmup = parseInt(args[++i], 10);
        break;
      case "--node-only":
        result.nodeOnly = true;
        break;
      case "--native-only":
        result.nativeOnly = true;
        break;
      case "--engine":
        result.engineMode = true;
        break;
      case "--json":
        result.json = true;
        break;
      default:
        console.error(`Unknown flag: ${args[i]}`);
        process.exit(1);
    }
  }

  return result;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function runDaemonBenchmark(args: CliArgs): Promise<void> {
  const runNode = !args.nativeOnly;
  const runNative = !args.nodeOnly;

  console.log("Starting benchmark daemons...");

  let nodeHandle: DaemonHandle | undefined;
  let nativeHandle: DaemonHandle | undefined;

  try {
    if (runNode) {
      nodeHandle = spawnNodeDaemon("bench-node");
      await waitForSocket("bench-node");
      console.log("  Node daemon ready");
    }

    if (runNative) {
      nativeHandle = spawnNativeDaemon("bench-native");
      await waitForSocket("bench-native");
      console.log("  Native daemon ready");
    }

    const sessions: { node?: string; native?: string } = {};
    if (runNode) sessions.node = "bench-node";
    if (runNative) sessions.native = "bench-native";

    for (const session of Object.values(sessions)) {
      const resp = await sendCommand(session, {
        id: "launch",
        action: "launch",
        headless: true,
      });
      if (!(resp as { success?: boolean }).success) {
        throw new Error(`Failed to launch browser on ${session}: ${JSON.stringify(resp)}`);
      }
    }
    console.log("  Browsers launched");
    console.log("");

    const results: ScenarioResult[] = [];
    for (const scenario of scenarios) {
      process.stdout.write(`  Running: ${scenario.name}...`);
      const result = await runScenario(scenario, sessions, args.iterations, args.warmup);
      results.push(result);

      if (result.nodeStats && result.nativeStats) {
        const speedup = formatSpeedup(result.nodeStats.avgUs, result.nativeStats.avgUs);
        process.stdout.write(
          ` node=${formatDuration(result.nodeStats.avgUs)} native=${formatDuration(result.nativeStats.avgUs)} (${speedup})\n`,
        );
      } else {
        const s = result.nodeStats ?? result.nativeStats;
        process.stdout.write(` avg=${s ? formatDuration(s.avgUs) : "??"}\n`);
      }
    }

    printResults(results, args.iterations, args.warmup, "daemon");

    if (args.json) {
      writeJsonResults(
        results,
        path.join(getProjectRoot(), "test/benchmarks/results.json"),
        "daemon",
      );
    }

    for (const session of Object.values(sessions)) {
      await sendCommand(session, { id: "close", action: "close" }).catch(() => {});
    }

    await sleep(300);

    if (runNode && runNative) {
      let totalNodeUs = 0;
      let totalNativeUs = 0;
      for (const r of results) {
        if (r.nodeStats && r.nativeStats) {
          totalNodeUs += r.nodeStats.avgUs;
          totalNativeUs += r.nativeStats.avgUs;
        }
      }
      if (totalNativeUs > 0 && totalNodeUs / totalNativeUs < 1.0) {
        process.exit(1);
      }
    }
  } finally {
    if (nodeHandle) await closeDaemon(nodeHandle);
    if (nativeHandle) await closeDaemon(nativeHandle);
  }
}

function buildHttpScenarios(baseUrl: string): Scenario[] {
  const pages = ["article.html", "dashboard.html", "ecommerce.html"];
  const httpScenarios: Scenario[] = [];

  for (const page of pages) {
    const label = page.replace(".html", "");
    httpScenarios.push({
      name: `http-${label}`,
      description: `Navigate to ${label} page over HTTP (full fetch + parse + layout)`,
      commands: [
        { id: "nav", action: "navigate", url: `${baseUrl}/${page}`, waitUntil: "load" },
      ],
    });
  }

  httpScenarios.push({
    name: "http-nav+snap",
    description: "Navigate to article over HTTP then snapshot",
    commands: [
      { id: "nav", action: "navigate", url: `${baseUrl}/article.html`, waitUntil: "load" },
      { id: "snap", action: "snapshot" },
    ],
  });

  // Multi-page throughput: cycle through all pages N times
  const multiPageCmds: BenchmarkCommand[] = [];
  for (let round = 0; round < 5; round++) {
    for (const page of pages) {
      multiPageCmds.push({
        id: `nav-${round}-${page}`,
        action: "navigate",
        url: `${baseUrl}/${page}`,
        waitUntil: "load",
      });
    }
  }
  httpScenarios.push({
    name: "http-multi-15pg",
    description: "Navigate 15 pages in sequence (5 rounds x 3 pages)",
    commands: multiPageCmds,
  });

  // Bulk navigation: 50 page loads of the article (closest to Lightpanda's 100-page benchmark)
  const bulkCmds: BenchmarkCommand[] = [];
  for (let i = 0; i < 50; i++) {
    bulkCmds.push({
      id: `bulk-${i}`,
      action: "navigate",
      url: `${baseUrl}/${pages[i % pages.length]}`,
      waitUntil: "load",
    });
  }
  httpScenarios.push({
    name: "http-bulk-50pg",
    description: "Navigate 50 pages sequentially (throughput test)",
    commands: bulkCmds,
  });

  return httpScenarios;
}

async function runEngineBenchmark(args: CliArgs): Promise<void> {
  console.log("Starting local file server...");
  const { server, port } = await startFileServer();
  const baseUrl = `http://127.0.0.1:${port}`;
  console.log(`  Serving pages at ${baseUrl}`);

  console.log("Starting engine benchmark daemons...");

  let chromeHandle: DaemonHandle | undefined;
  let lightpandaHandle: DaemonHandle | undefined;

  try {
    chromeHandle = spawnNativeDaemon("bench-chrome", "chrome");
    await waitForSocket("bench-chrome");
    console.log("  Chrome daemon ready");

    lightpandaHandle = spawnNativeDaemon("bench-lightpanda", "lightpanda");
    await waitForSocket("bench-lightpanda");
    console.log("  Lightpanda daemon ready");

    const sessions: Record<string, string> = {
      chrome: "bench-chrome",
      lightpanda: "bench-lightpanda",
    };

    for (const [label, session] of Object.entries(sessions)) {
      const resp = await sendCommand(session, {
        id: "launch",
        action: "launch",
        headless: true,
      });
      if (!(resp as { success?: boolean }).success) {
        throw new Error(
          `Failed to launch ${label} browser on ${session}: ${JSON.stringify(resp)}`,
        );
      }
    }
    console.log("  Browsers launched");

    // Collect PIDs for memory sampling
    const chromePid = chromeHandle.process.pid;
    const lpPid = lightpandaHandle.process.pid;
    const pidsToSample: number[] = [];
    if (chromePid) pidsToSample.push(chromePid);
    if (lpPid) pidsToSample.push(lpPid);

    const memSampler = pidsToSample.length > 0
      ? sampleMemory(pidsToSample, 500)
      : null;

    // Measure per-engine peak memory during the heavy scenarios
    const chromeMemPids = chromePid ? [chromePid] : [];
    const lpMemPids = lpPid ? [lpPid] : [];

    console.log("");

    const httpScenarios = buildHttpScenarios(baseUrl);
    const allScenarios = [...scenarios, ...engineScenarios, ...httpScenarios];
    const results: ScenarioResult[] = [];
    for (const scenario of allScenarios) {
      process.stdout.write(`  Running: ${scenario.name}...`);
      const result = await runScenarioWithErrorTolerance(
        scenario,
        sessions,
        args.iterations,
        args.warmup,
      );
      results.push(result);

      const chromeAvg = result.chromeStats
        ? formatDuration(result.chromeStats.avgUs)
        : "N/A";
      const lpAvg = result.lightpandaStats
        ? formatDuration(result.lightpandaStats.avgUs)
        : "N/A";

      if (result.chromeStats && result.lightpandaStats) {
        const speedup = formatSpeedup(
          result.chromeStats.avgUs,
          result.lightpandaStats.avgUs,
        );
        process.stdout.write(` chrome=${chromeAvg} lightpanda=${lpAvg} (${speedup})\n`);
      } else {
        process.stdout.write(` chrome=${chromeAvg} lightpanda=${lpAvg}\n`);
      }
    }

    // Final memory snapshot
    const chromeMemKB = chromeMemPids.length > 0 ? getProcessMemoryKB(chromeMemPids[0]) : null;
    const lpMemKB = lpMemPids.length > 0 ? getProcessMemoryKB(lpMemPids[0]) : null;
    if (memSampler) memSampler.stop();

    printResults(results, args.iterations, args.warmup, "engine");

    if (chromeMemKB || lpMemKB) {
      console.log("Memory (daemon RSS after benchmarks):");
      if (chromeMemKB) console.log(`  Chrome daemon:     ${formatMemory(chromeMemKB)}`);
      if (lpMemKB) console.log(`  Lightpanda daemon: ${formatMemory(lpMemKB)}`);
      if (chromeMemKB && lpMemKB && lpMemKB > 0) {
        const memRatio = chromeMemKB / lpMemKB;
        console.log(`  Ratio: chrome uses ${memRatio.toFixed(1)}x more memory`);
      }
      console.log("");
    }

    if (args.json) {
      writeJsonResults(
        results,
        path.join(getProjectRoot(), "test/benchmarks/results-engine.json"),
        "engine",
      );
    }

    for (const session of Object.values(sessions)) {
      await sendCommand(session, { id: "close", action: "close" }).catch(() => {});
    }

    await sleep(300);
  } finally {
    if (chromeHandle) await closeDaemon(chromeHandle);
    if (lightpandaHandle) await closeDaemon(lightpandaHandle);
    await stopFileServer(server);
  }
}

async function main(): Promise<void> {
  const args = parseArgs();

  cleanupSockets();

  try {
    if (args.engineMode) {
      await runEngineBenchmark(args);
    } else {
      await runDaemonBenchmark(args);
    }
  } finally {
    cleanupSockets();
  }
}

main().catch((err) => {
  console.error("Benchmark failed:", err.message || err);
  process.exit(2);
});
