/** Compares four waiting approaches using delayed page signals. */
import { execFile, spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

type Data = { id?: string; cursor?: string; result?: unknown; waiting?: boolean; events?: { meta?: { name?: string } }[] };
type Result = { status: string; calls: number; latency: number | null; cpu: number | null; error: string };
const bin = process.env.AGENT_BROWSER_BIN ?? fileURLToPath(new URL("../cli/target/debug/agent-browser", import.meta.url));
const session = `events-compare-${process.pid}`;
const cap = 10_000;
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const isSignal = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value);
function numberArg(name: string, fallback: number): number {
  const index = process.argv.indexOf(name);
  const value = index < 0 ? fallback : Number(process.argv[index + 1]);
  if (!Number.isInteger(value) || value <= 0) throw new Error(`${name} expects a positive integer`);
  return value;
}
function cli(args: string[], timeout = cap + 5_000): Promise<Data> {
  return new Promise((resolve, reject) => {
    execFile(bin, ["--json", "--session", session, ...args], { timeout, killSignal: "SIGKILL", env: { ...process.env, NO_COLOR: "1" } }, (error, stdout, stderr) => {
      if (error?.code === "ENOENT") return reject(error);
      if (error?.killed) return reject(new Error(`timed out after ${timeout}ms`));
      for (const line of stdout.trim().split("\n").reverse()) {
        try {
          const json = JSON.parse(line);
          if (typeof json?.success !== "boolean") continue;
          return json.success && !error ? resolve(json.data ?? {}) : reject(new Error(json.error || stderr.trim() || error?.message || "command failed"));
        } catch {}
      }
      reject(new Error(stderr.trim() || error?.message || "command returned no JSON"));
    });
  });
}
function cpu(): number | null {
  try {
    let directory = process.env.AGENT_BROWSER_SOCKET_DIR ?? (process.env.XDG_RUNTIME_DIR ? join(process.env.XDG_RUNTIME_DIR, "agent-browser") : join(homedir(), ".agent-browser"));
    if (process.env.AGENT_BROWSER_NAMESPACE) directory = join(directory, "namespaces", process.env.AGENT_BROWSER_NAMESPACE, "run");
    const pid = readFileSync(join(directory, `${session}.pid`), "utf8").trim();
    const output = spawnSync("ps", ["-o", "cputime=", "-p", pid], { encoding: "utf8" });
    const parts = output.stdout.trim().split(":").map(Number);
    return output.status === 0 && [2, 3].includes(parts.length) && parts.every(Number.isFinite) ? parts.reduce((total, part) => total * 60 + part, 0) * 1000 : null;
  } catch { return null; }
}
function median(values: (number | null)[]): string {
  const sorted = values.filter(isSignal).sort((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length ? ((sorted[middle] + sorted[Math.floor((sorted.length - 1) / 2)]) / 2).toFixed(1) : "-";
}
async function measure(scenario: string, events: string[], approach: string, base: string, delay: number): Promise<Result> {
  const sample: Result = { status: "failed", calls: 0, latency: null, cpu: null, error: "" };
  let id: string | undefined, signal: unknown, returnedAt = 0, unsupported = false;
  const call = (args: string[], timeout?: number) => { sample.calls += 1; return cli(args, timeout); };
  try {
    await cli(["open", `${base}/${scenario}.html?delay=${delay}`], 30_000);
    const before = cpu();
    try {
      if (approach === "events") {
        unsupported = true;
        const started = await call(["events", "start", ...events, "--once"]);
        if (typeof started.id !== "string") throw new Error("events start returned no subscription id");
        id = started.id;
        const waited = await call(["events", "wait", id, "--since", String(started.cursor ?? ""), "--timeout", String(cap)]);
        unsupported = false;
        if (waited.waiting || !Array.isArray(waited.events) || !waited.events.some((event) => !String(event?.meta?.name).startsWith("subscription."))) throw new Error("events wait returned no event");
      } else if (approach === "polling") {
        const deadline = Date.now() + cap;
        let lastError: unknown = new Error(`signal not observed within ${cap}ms`);
        while (Date.now() < deadline) {
          try { signal = (await call(["eval", "window.__signalAt ?? null"], Math.max(1_000, deadline - Date.now()))).result; }
          catch (error) { lastError = error; if (String(error).includes("timed out")) throw error; }
          if (isSignal(signal)) break;
          await sleep(250);
        }
        if (!isSignal(signal)) throw lastError;
      } else {
        const args = approach === "wait-fn" ? ["wait", "--fn", "window.__signalAt ?? 0", "--timeout", String(cap)] : ["eval", "new Promise(resolve => { const check = () => window.__signalAt ? resolve(window.__signalAt) : setTimeout(check, 50); check(); })"];
        signal = (await call(args)).result;
      }
    } finally {
      returnedAt = Date.now();
      const after = cpu();
      sample.cpu = before === null || after === null ? null : Math.max(0, after - before);
      if (scenario === "dialog") await cli(["dialog", "dismiss"], 3_000).catch(() => {});
    }
    const deadline = Date.now() + 2_000;
    while (!isSignal(signal) && Date.now() < deadline) {
      try { signal = (await cli(["eval", "window.__signalAt ?? null"], 2_000)).result; } catch {}
      if (!isSignal(signal)) await sleep(50);
    }
    if (!isSignal(signal)) throw new Error("signal timestamp was not readable");
    sample.latency = returnedAt - signal;
    sample.status = "completed";
  } catch (error) {
    if ((error as NodeJS.ErrnoException)?.code === "ENOENT") throw new Error(`agent-browser binary not found at ${bin}. Build cli/target/debug/agent-browser or set AGENT_BROWSER_BIN.`);
    sample.error = error instanceof Error ? error.message : String(error);
    if (sample.calls && (approach === "events" ? unsupported : scenario === "dialog" || scenario === "navigation" && /context|destroyed|navigat|target closed/i.test(sample.error))) sample.status = "unsupported";
  } finally {
    if (id !== undefined) await cli(["events", "stop", id], 5_000).catch(() => {});
  }
  return sample;
}
async function main(): Promise<void> {
  const runs = numberArg("--runs", 5);
  const delay = numberArg("--delay", 1500);
  const pages: Record<string, string> = {
    "/element.html": `<p id="status">waiting</p><div id="late" style="display:none">late</div><script>setTimeout(() => { window.__signalAt = Date.now(); document.getElementById("late").style.display = "block"; document.getElementById("status").textContent = "ready"; }, ${delay});</script>`,
    "/dialog.html": `<p id="status">waiting</p><script>setTimeout(() => { window.__signalAt = Date.now(); alert("late dialog"); }, ${delay});</script>`,
    "/navigation.html": `<p id="status">waiting</p><script>setTimeout(() => { location.href = "/navigation-target.html"; }, ${delay});</script>`,
    "/navigation-target.html": `<script>window.__signalAt = Date.now();</script><p id="arrived">arrived</p>`,
  };
  const server = createServer((request, response) => {
    const body = pages[new URL(request.url ?? "/", "http://127.0.0.1").pathname];
    response.writeHead(body ? 200 : 404, { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store" });
    response.end(body ?? "not found");
  });
  await new Promise<void>((resolve, reject) => server.once("error", reject).listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  const scenarios: [string, string, string[]][] = [
    ["element", "element appears", ["--selector", "#late"]],
    ["dialog", "dialog opens", ["--method", "Page.javascriptDialogOpening"]],
    ["navigation", "navigation commits", ["--method", "Page.frameNavigated"]],
  ];
  const rows: string[] = [];
  try {
    for (const [scenario, name, events] of scenarios) {
      for (const approach of ["events", "wait-fn", "eval-promise", "polling"]) {
        const samples: Result[] = [];
        for (let run = 0; run < runs; run += 1) {
          const sample = await measure(scenario, events, approach, base, delay);
          samples.push(sample);
          if (sample.status === "unsupported" || !sample.calls) break;
        }
        const completed = samples.filter((sample) => sample.status === "completed");
        const last = samples[samples.length - 1];
        const notes = (samples.find((sample) => sample.error)?.error ?? "").replace(/\s+/g, " ").replace(/\|/g, "\\|").trim();
        const medians = (["calls", "latency", "cpu"] as const).map((key) => median(completed.map((sample) => sample[key])));
        rows.push(`| ${name} | ${approach} | ${completed.length}/${samples.length} | ${medians.join(" | ")} | ${notes} |`);
        console.log(`${name} / ${approach}: ${last.status}${last.error ? ` (${last.error})` : ""}`);
      }
    }
    const results = new URL("events-compare-results.md", import.meta.url);
    writeFileSync(results, `# Events comparison

Generated: ${new Date().toISOString()}

- Binary: \`${bin}\`
- Session: \`${session}\`
- Delay: ${delay} ms
- Runs per pair: ${runs}
- Signal cap: ${cap} ms

| Scenario | Approach | Completed | Extra calls | Latency median (ms) | Daemon CPU median (ms) | Notes |
| --- | --- | --- | --- | --- | --- | --- |
${rows.join("\n")}

Extra agent calls counts CLI invocations after \`open\`. Events uses \`events start\` plus \`events wait\`. The signal timestamp read used to compute latency is measurement overhead and is not counted.

Latency is the same-host wall clock difference between the page writing \`window.__signalAt\` and the approach command returning. Daemon CPU is the \`ps\` cputime delta of the session daemon across the approach command.
`);
    console.log(`Wrote ${fileURLToPath(results)}`);
  } finally {
    await cli(["close"], 15_000).catch(() => {});
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}
main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
