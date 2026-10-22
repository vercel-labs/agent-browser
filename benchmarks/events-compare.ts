/**
 * Compares event subscriptions, `wait --fn`, a promise-returning `eval`, and an
 * `eval` polling loop on the same delayed page signal. Run with
 * `pnpm --dir benchmarks exec tsx events-compare.ts`.
 */

import { spawn, spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

type JsonObject = Record<string, unknown>;
type Status = "completed" | "failed" | "unsupported";
type ApproachName = "events" | "wait-fn" | "eval-promise" | "polling";

interface CliResult {
  ok: boolean;
  json: JsonObject | null;
  error: string | null;
}

interface ApproachResult {
  status: Status;
  calls: number;
  latencyMs: number | null;
  cpuMs: number | null;
  error: string | null;
}

interface Scenario {
  name: string;
  path: string;
  events: string[];
}

interface PairRecord {
  scenario: string;
  approach: ApproachName;
  results: ApproachResult[];
}

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const FIXTURES = join(ROOT, "benchmarks", "fixtures");
const RESULTS = join(ROOT, "benchmarks", "events-compare-results.md");
const CAP_MS = 10_000;
const SESSION = `events-compare-${process.pid}`;
const BIN =
  process.env.AGENT_BROWSER_BIN ??
  join(ROOT, "cli", "target", "debug", "agent-browser");

const SCENARIOS: Scenario[] = [
  {
    name: "element appears",
    path: "/element.html",
    events: ["events", "start", "--selector", "#late", "--once"],
  },
  {
    name: "dialog opens",
    path: "/dialog.html",
    events: [
      "events",
      "start",
      "--method",
      "Page.javascriptDialogOpening",
      "--once",
    ],
  },
  {
    name: "navigation commits",
    path: "/navigation.html",
    events: [
      "events",
      "start",
      "--method",
      "Page.frameNavigated",
      "--once",
    ],
  },
];

const APPROACHES: ApproachName[] = [
  "events",
  "wait-fn",
  "eval-promise",
  "polling",
];

function numberArg(name: string, fallback: number): number {
  const index = process.argv.indexOf(name);
  if (index === -1) return fallback;
  const value = Number(process.argv[index + 1]);
  if (!Number.isInteger(value) || value <= 0) {
    console.error(`${name} expects a positive integer`);
    process.exit(1);
  }
  return value;
}

const runs = numberArg("--runs", 5);
const delay = numberArg("--delay", 1500);

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function parseJson(stdout: string): JsonObject | null {
  const lines = stdout.trim().split("\n");
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    const line = lines[index].trim();
    if (!line.startsWith("{")) continue;
    try {
      return JSON.parse(line) as JsonObject;
    } catch {}
  }
  return null;
}

function at(value: unknown, path: (string | number)[]): unknown {
  let current = value;
  for (const key of path) {
    if (typeof key === "number") {
      if (!Array.isArray(current)) return undefined;
      current = current[key];
    } else {
      if (typeof current !== "object" || current === null) return undefined;
      current = (current as JsonObject)[key];
    }
  }
  return current;
}

function asNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function cli(args: string[], timeoutMs = CAP_MS + 5_000): Promise<CliResult> {
  return new Promise((resolve) => {
    const child = spawn(
      BIN,
      ["--json", "--session", SESSION, ...args],
      {
        env: {
          ...process.env,
          NO_COLOR: "1",
        },
      },
    );
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", (error) => {
      clearTimeout(timer);
      resolve({ ok: false, json: null, error: error.message });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      const json = parseJson(stdout);
      if (timedOut) {
        resolve({
          ok: false,
          json,
          error: `timed out after ${timeoutMs}ms`,
        });
        return;
      }
      if (json?.success === false) {
        resolve({
          ok: false,
          json,
          error: String(json.error ?? "command failed"),
        });
        return;
      }
      if (code !== 0) {
        resolve({
          ok: false,
          json,
          error: stderr.trim() || `exit ${code}`,
        });
        return;
      }
      resolve({ ok: true, json, error: null });
    });
  });
}

function socketDir(): string {
  const explicit = process.env.AGENT_BROWSER_SOCKET_DIR;
  const base = explicit
    ? explicit
    : process.env.XDG_RUNTIME_DIR
      ? join(process.env.XDG_RUNTIME_DIR, "agent-browser")
      : join(homedir(), ".agent-browser");
  const namespace = process.env.AGENT_BROWSER_NAMESPACE;
  return namespace ? join(base, "namespaces", namespace, "run") : base;
}

function parseCpuTime(value: string): number | null {
  const parts = value.split(":").map(Number);
  if (parts.length === 0 || parts.some(Number.isNaN)) return null;
  if (parts.length === 2) return parts[0] * 60 + parts[1];
  if (parts.length === 3) return parts[0] * 3600 + parts[1] * 60 + parts[2];
  return null;
}

function daemonCpuSeconds(): number | null {
  let pid = "";
  try {
    pid = readFileSync(join(socketDir(), `${SESSION}.pid`), "utf8").trim();
  } catch {
    return null;
  }
  if (!pid) return null;
  const out = spawnSync("ps", ["-o", "cputime=", "-p", pid], {
    encoding: "utf8",
  });
  if (out.status !== 0) return null;
  return parseCpuTime(out.stdout.trim());
}

function cpuDelta(before: number | null, after: number | null): number | null {
  if (before === null || after === null) return null;
  return Math.max(0, (after - before) * 1000);
}

async function dismissDialog(): Promise<void> {
  await cli(["dialog", "dismiss"], 3_000);
}

async function readSignalAt(): Promise<number | null> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    const result = await cli(["eval", "window.__signalAt ?? null"], 2_000);
    const value = asNumber(at(result.json, ["data", "result"]));
    if (value !== null) return value;
    await sleep(50);
  }
  return null;
}

function cannotExpress(scenario: Scenario, error: string | null): Status {
  if (scenario.name === "dialog opens") return "unsupported";
  if (
    scenario.name === "navigation commits" &&
    error !== null &&
    /context|destroyed|navigat|target closed/i.test(error)
  ) {
    return "unsupported";
  }
  return "failed";
}

async function eventsApproach(scenario: Scenario): Promise<ApproachResult> {
  const cpuBefore = daemonCpuSeconds();
  let calls = 0;
  let id: string | null = null;
  try {
    const start = await cli(scenario.events);
    calls += 1;
    if (!start.ok) {
      return {
        status: "unsupported",
        calls,
        latencyMs: null,
        cpuMs: null,
        error: start.error ?? "events start failed",
      };
    }
    const started = at(start.json, ["data", "id"]);
    if (typeof started !== "string") {
      return {
        status: "unsupported",
        calls,
        latencyMs: null,
        cpuMs: null,
        error: "events start returned no subscription id",
      };
    }
    id = started;
    const cursor = String(at(start.json, ["data", "cursor"]) ?? "");
    const wait = await cli(
      ["events", "wait", id, "--since", cursor, "--timeout", String(CAP_MS)],
      CAP_MS + 5_000,
    );
    calls += 1;
    const returnedAt = Date.now();
    const cpuAfter = daemonCpuSeconds();
    await dismissDialog();
    const signalAt = await readSignalAt();
    const events = at(wait.json, ["data", "events"]);
    const waiting = at(wait.json, ["data", "waiting"]);
    const observed =
      Array.isArray(events) &&
      events.some((event) => !String(at(event, ["meta", "name"])).startsWith("subscription."));
    if (!wait.ok || waiting === true || !observed || signalAt === null) {
      return {
        status: wait.ok ? "failed" : "unsupported",
        calls,
        latencyMs: null,
        cpuMs: cpuDelta(cpuBefore, cpuAfter),
        error:
          wait.error ??
          (observed
            ? "signal timestamp was not readable"
            : "events wait returned no event"),
      };
    }
    return {
      status: "completed",
      calls,
      latencyMs: returnedAt - signalAt,
      cpuMs: cpuDelta(cpuBefore, cpuAfter),
      error: null,
    };
  } finally {
    if (id !== null) await cli(["events", "stop", id], 5_000);
  }
}

async function waitFnApproach(scenario: Scenario): Promise<ApproachResult> {
  const cpuBefore = daemonCpuSeconds();
  const wait = await cli(
    [
      "wait",
      "--fn",
      "window.__signalAt ?? 0",
      "--timeout",
      String(CAP_MS),
    ],
    CAP_MS + 5_000,
  );
  const returnedAt = Date.now();
  const cpuAfter = daemonCpuSeconds();
  await dismissDialog();
  const signalAt = wait.ok
    ? (asNumber(at(wait.json, ["data", "result"])) ?? (await readSignalAt()))
    : null;
  if (!wait.ok || signalAt === null) {
    return {
      status: cannotExpress(scenario, wait.error),
      calls: 1,
      latencyMs: null,
      cpuMs: cpuDelta(cpuBefore, cpuAfter),
      error: wait.error ?? "wait --fn returned no signal timestamp",
    };
  }
  return {
    status: "completed",
    calls: 1,
    latencyMs: returnedAt - signalAt,
    cpuMs: cpuDelta(cpuBefore, cpuAfter),
    error: null,
  };
}

async function evalPromiseApproach(scenario: Scenario): Promise<ApproachResult> {
  const cpuBefore = daemonCpuSeconds();
  const expression =
    "new Promise((resolve) => { const check = () => { if (window.__signalAt) resolve(window.__signalAt); else setTimeout(check, 50); }; check(); })";
  const result = await cli(["eval", expression], CAP_MS + 5_000);
  const returnedAt = Date.now();
  const cpuAfter = daemonCpuSeconds();
  await dismissDialog();
  const signalAt = result.ok
    ? (asNumber(at(result.json, ["data", "result"])) ?? (await readSignalAt()))
    : null;
  if (!result.ok || signalAt === null) {
    return {
      status: cannotExpress(scenario, result.error),
      calls: 1,
      latencyMs: null,
      cpuMs: cpuDelta(cpuBefore, cpuAfter),
      error: result.error ?? "eval returned no signal timestamp",
    };
  }
  return {
    status: "completed",
    calls: 1,
    latencyMs: returnedAt - signalAt,
    cpuMs: cpuDelta(cpuBefore, cpuAfter),
    error: null,
  };
}

async function pollingApproach(scenario: Scenario): Promise<ApproachResult> {
  const cpuBefore = daemonCpuSeconds();
  const startedAt = Date.now();
  let calls = 0;
  let lastError: string | null = null;
  while (Date.now() - startedAt < CAP_MS) {
    const remaining = CAP_MS - (Date.now() - startedAt);
    calls += 1;
    const result = await cli(
      ["eval", "window.__signalAt ?? null"],
      Math.max(1_000, remaining),
    );
    if (result.ok) {
      const value = asNumber(at(result.json, ["data", "result"]));
      if (value !== null) {
        const returnedAt = Date.now();
        return {
          status: "completed",
          calls,
          latencyMs: returnedAt - value,
          cpuMs: cpuDelta(cpuBefore, daemonCpuSeconds()),
          error: null,
        };
      }
    } else {
      lastError = result.error;
      if (result.error?.startsWith("timed out")) {
        await dismissDialog();
        return {
          status: cannotExpress(scenario, result.error),
          calls,
          latencyMs: null,
          cpuMs: cpuDelta(cpuBefore, daemonCpuSeconds()),
          error: result.error,
        };
      }
    }
    await sleep(250);
  }
  const cpuMs = cpuDelta(cpuBefore, daemonCpuSeconds());
  await dismissDialog();
  return {
    status: cannotExpress(scenario, lastError),
    calls,
    latencyMs: null,
    cpuMs,
    error: lastError ?? `signal not observed within ${CAP_MS}ms`,
  };
}

function runApproach(
  approach: ApproachName,
  scenario: Scenario,
): Promise<ApproachResult> {
  switch (approach) {
    case "events":
      return eventsApproach(scenario);
    case "wait-fn":
      return waitFnApproach(scenario);
    case "eval-promise":
      return evalPromiseApproach(scenario);
    case "polling":
      return pollingApproach(scenario);
  }
}

async function runOnce(
  scenario: Scenario,
  approach: ApproachName,
  baseUrl: string,
): Promise<ApproachResult> {
  await dismissDialog();
  const opened = await cli(
    ["open", `${baseUrl}${scenario.path}?delay=${delay}`],
    30_000,
  );
  if (!opened.ok) {
    throw new Error(`open failed: ${opened.error}`);
  }
  return runApproach(approach, scenario);
}

function median(values: number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0
    ? (sorted[middle - 1] + sorted[middle]) / 2
    : sorted[middle];
}

function cell(value: number | null): string {
  return value === null ? "-" : value.toFixed(1);
}

function escapeCell(value: string): string {
  return value.replace(/\s+/g, " ").replace(/\|/g, "\\|").trim();
}

function renderReport(records: PairRecord[]): string {
  const lines = [
    "# Events comparison",
    "",
    `Generated: ${new Date().toISOString()}`,
    "",
    `- Binary: \`${BIN}\``,
    `- Session: \`${SESSION}\``,
    `- Delay: ${delay} ms`,
    `- Runs per pair: ${runs}`,
    `- Signal cap: ${CAP_MS} ms`,
    "",
    "| Scenario | Approach | Completed | Extra calls | Latency median (ms) | Daemon CPU median (ms) | Notes |",
    "| --- | --- | --- | --- | --- | --- | --- |",
  ];
  for (const record of records) {
    const completed = record.results.filter(
      (result) => result.status === "completed",
    );
    const error = record.results.find((result) => result.error)?.error;
    lines.push(
      `| ${record.scenario} | ${record.approach} | ${completed.length}/${record.results.length} | ${cell(median(completed.map((result) => result.calls)))} | ${cell(median(completed.map((result) => result.latencyMs).filter((value): value is number => value !== null)))} | ${cell(median(completed.map((result) => result.cpuMs).filter((value): value is number => value !== null)))} | ${error ? escapeCell(error) : ""} |`,
    );
  }
  lines.push(
    "",
    "Extra agent calls counts CLI invocations after `open`. Events uses `events start` plus `events wait`. The signal timestamp read used to compute latency is measurement overhead and is not counted.",
    "",
    "Latency is the same-host wall clock difference between the page writing `window.__signalAt` and the approach command returning. Daemon CPU is the `ps` cputime delta of the session daemon across the approach command.",
    "",
  );
  return lines.join("\n");
}

async function main(): Promise<void> {
  if (!existsSync(BIN)) {
    console.error(
      `agent-browser binary not found at ${BIN}. Build cli/target/debug/agent-browser or set AGENT_BROWSER_BIN.`,
    );
    process.exit(1);
  }

  const server = createServer((request, response) => {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    const file = join(FIXTURES, url.pathname.slice(1));
    try {
      const body = readFileSync(file);
      response.writeHead(200, {
        "Content-Type": "text/html; charset=utf-8",
        "Cache-Control": "no-store",
      });
      response.end(body);
    } catch {
      response.writeHead(404, { "Content-Type": "text/plain" });
      response.end("not found");
    }
  });
  await new Promise<void>((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve());
  });
  const { port } = server.address() as AddressInfo;
  const baseUrl = `http://127.0.0.1:${port}`;
  const records: PairRecord[] = [];

  try {
    for (const scenario of SCENARIOS) {
      for (const approach of APPROACHES) {
        const results: ApproachResult[] = [];
        for (let run = 0; run < runs; run += 1) {
          try {
            const result = await runOnce(scenario, approach, baseUrl);
            results.push(result);
            if (result.status === "unsupported") break;
          } catch (error) {
            results.push({
              status: "failed",
              calls: 0,
              latencyMs: null,
              cpuMs: null,
              error: error instanceof Error ? error.message : String(error),
            });
            break;
          }
        }
        records.push({ scenario: scenario.name, approach, results });
        const last = results[results.length - 1];
        console.log(
          `${scenario.name} / ${approach}: ${last?.status ?? "no result"}${last?.error ? ` (${last.error})` : ""}`,
        );
      }
    }
    writeFileSync(RESULTS, renderReport(records));
    console.log(`Wrote ${RESULTS}`);
  } finally {
    await dismissDialog();
    await cli(["close"], 15_000);
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});
