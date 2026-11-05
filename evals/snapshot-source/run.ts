// A/B eval for `snapshot --source`: does an agent complete browser tasks better, or with fewer
// snapshot tokens, when the snapshot comes from the accessibility tree (ax), Annotated Page
// Content (apc) or the hybrid? Success is decided by the harness from page events and the URL.
//
//   bun run snapshot-source/run.ts --model anthropic/claude-haiku-4.5 --reps 2
//
// Environment: AI_GATEWAY_API_KEY. AGENT_BROWSER_BIN selects the binary (default
// ../cli/target/ci/agent-browser, then cli/target/debug/agent-browser). Tokens are counted with
// o200k_base from js-tiktoken, resolved from the TIKTOKEN_MODULE path or the package name; if it
// cannot be loaded, chars/4 is used and recorded as the tokenizer.

import { existsSync, mkdirSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";
import { startFixtureServer } from "./fixtures.ts";
import { tasks, type Task } from "./tasks.ts";
import { cliEnv, guardCommand } from "./guard";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "../..");
const GATEWAY = "https://ai-gateway.vercel.sh/v1/chat/completions";

type Variant = "ax" | "apc" | "hybrid";

interface Args {
  variants: Variant[];
  reps: number;
  model: string;
  only: string[];
  concurrency: number;
  maxSteps: number;
  out: string;
  bin: string;
}

function parseArgs(argv: string[]): Args {
  const get = (name: string, fallback?: string) => {
    const i = argv.indexOf(`--${name}`);
    return i >= 0 ? argv[i + 1] : fallback;
  };
  const candidates = [resolve(repoRoot, "cli/target/ci/agent-browser"), resolve(repoRoot, "cli/target/release/agent-browser"), resolve(repoRoot, "cli/target/debug/agent-browser")];
  const bin = process.env.AGENT_BROWSER_BIN ?? candidates.find((c) => existsSync(c));
  if (!bin) throw new Error("No agent-browser binary found. Build with cargo or set AGENT_BROWSER_BIN.");
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  return {
    variants: (get("variants", "ax,apc,hybrid") as string).split(",") as Variant[],
    reps: Number(get("reps", "2")),
    model: get("model", "anthropic/claude-haiku-4.5") as string,
    only: (get("tasks", "") as string).split(",").filter(Boolean),
    concurrency: Number(get("concurrency", "4")),
    maxSteps: Number(get("max-steps", "25")),
    out: resolve(get("out", resolve(__dirname, `../results/snapshot-source/${stamp}`)) as string),
    bin,
  };
}

// ---- tokens -------------------------------------------------------------------------------

let countTokens = (s: string) => Math.ceil(s.length / 4);
let tokenizer = "chars/4";
async function loadTokenizer() {
  const spec = process.env.TIKTOKEN_MODULE ?? "js-tiktoken";
  try {
    const mod = await import(spec);
    const enc = mod.getEncoding("o200k_base");
    countTokens = (s: string) => enc.encode(s).length;
    tokenizer = "o200k_base";
  } catch {
    // keep chars/4
  }
}

// ---- shell ---------------------------------------------------------------------------------

function shellSplit(line: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quote: string | null = null;
  let has = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (quote) {
      if (c === quote) quote = null;
      else if (c === "\\" && quote === '"' && i + 1 < line.length) cur += line[++i];
      else cur += c;
    } else if (c === '"' || c === "'") {
      quote = c;
      has = true;
    } else if (/\s/.test(c)) {
      if (cur || has) out.push(cur);
      cur = "";
      has = false;
    } else if (c === "\\" && i + 1 < line.length) {
      cur += line[++i];
    } else cur += c;
  }
  if (cur || has) out.push(cur);
  return out;
}

async function exec(bin: string, args: string[], session: string, timeoutMs = 60_000): Promise<{ out: string; code: number }> {
  // hybrid is not a public source; the CLI accepts it only with AGENT_BROWSER_SNAPSHOT_HYBRID.
  const proc = Bun.spawn([bin, ...args], {
    stdout: "pipe",
    stderr: "pipe",
    env: cliEnv({ AGENT_BROWSER_SESSION: session, AGENT_BROWSER_SNAPSHOT_HYBRID: "1" }),
  });
  const timer = setTimeout(() => proc.kill(), timeoutMs);
  const [stdout, stderr, code] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited]);
  clearTimeout(timer);
  return { out: (stdout + (stderr ? `\n${stderr}` : "")).trim(), code };
}

/** Force the variant and interactive mode onto any snapshot the agent asks for. */
function normalize(cmd: string[], variant: Variant): string[] {
  if (cmd[0] !== "snapshot") return cmd;
  const rest: string[] = [];
  for (let i = 1; i < cmd.length; i++) {
    if (cmd[i] === "--source") i++;
    else if (cmd[i] === "-i" || cmd[i] === "--interactive" || cmd[i] === "--viewport") continue;
    else rest.push(cmd[i]);
  }
  return ["snapshot", "-i", "--source", variant, ...rest];
}

// ---- agent ---------------------------------------------------------------------------------

const SYSTEM = `You control a web browser through the agent_browser tool, which runs agent-browser CLI commands. The task page is already open.

Commands (pass them as the args string, without the program name):
- snapshot: list the interactive elements with refs such as @e3. Refs can go stale after the page changes, so snapshot again when an action fails or the page changes.
- click @e3
- fill @e3 "text" (clears the field, then types)
- type @e3 "text" (types without clearing)
- select @e3 "value or visible label"
- check @e3 / uncheck @e3
- press Enter
- scroll down 500
- wait 500
- get url / get title
- get text body (read the page text; snapshots list only interactive elements, so use this to confirm results)
- eval "javascript expression"
- frame @e5 (work inside an iframe) / frame main (back to the main frame)

Act on the page until the task is done, then call the done tool. Do not ask questions.`;

const TOOLS = [
  {
    type: "function",
    function: {
      name: "agent_browser",
      description: "Run an agent-browser command, for example: snapshot, click @e3, fill @e2 \"hello\".",
      parameters: { type: "object", properties: { args: { type: "string", description: "Command and arguments" } }, required: ["args"] },
    },
  },
  {
    type: "function",
    function: {
      name: "done",
      description: "Call when the task is complete (or you cannot continue).",
      parameters: { type: "object", properties: { summary: { type: "string" } }, required: ["summary"] },
    },
  },
];

interface Step {
  call: string;
  snapshotTokens: number;
  outputChars: number;
  error?: boolean;
}

interface RunResult {
  task: string;
  category: string;
  variant: Variant;
  rep: number;
  success: boolean;
  steps: number;
  snapshots: number;
  snapshotTokens: number;
  agentTokens: { prompt: number; completion: number };
  wallMs: number;
  finishedBy: "done" | "max-steps" | "error";
  error?: string;
  warnings: number;
  notes: number;
  holdout: boolean;
  events: unknown;
  transcript: Step[];
}

async function chat(model: string, messages: unknown[]): Promise<any> {
  const key = process.env.AI_GATEWAY_API_KEY;
  if (!key) throw new Error("AI_GATEWAY_API_KEY is not set");
  for (let attempt = 0; ; attempt++) {
    const res = await fetch(GATEWAY, {
      method: "POST",
      headers: { Authorization: `Bearer ${key}`, "Content-Type": "application/json" },
      body: JSON.stringify({ model, messages, tools: TOOLS, tool_choice: "auto", temperature: 0 }),
    });
    if (res.ok) return res.json();
    const body = await res.text();
    if (attempt < 3 && (res.status === 429 || res.status >= 500)) {
      await new Promise((r) => setTimeout(r, 2000 * (attempt + 1)));
      continue;
    }
    throw new Error(`gateway ${res.status}: ${body.slice(0, 300)}`);
  }
}

async function runOne(args: Args, server: ReturnType<typeof startFixtureServer>, task: Task, variant: Variant, rep: number): Promise<RunResult> {
  const run = `${task.id}-${variant}-${rep}-${Math.random().toString(36).slice(2, 7)}`;
  const session = `abapc-${run}`.slice(0, 60);
  const url = `http://localhost:${server.port}/r/${run}/${task.page}`;
  const started = performance.now();
  const result: RunResult = {
    task: task.id,
    category: task.category,
    variant,
    rep,
    success: false,
    steps: 0,
    snapshots: 0,
    snapshotTokens: 0,
    agentTokens: { prompt: 0, completion: 0 },
    wallMs: 0,
    finishedBy: "max-steps",
    warnings: 0,
    notes: 0,
    holdout: task.holdout === true,
    events: [],
    transcript: [],
  };
  try {
    const opened = await exec(args.bin, ["open", url], session);
    if (opened.code !== 0) throw new Error(`open failed: ${opened.out}`);
    const messages: any[] = [
      { role: "system", content: SYSTEM },
      { role: "user", content: `Task: ${task.prompt}` },
    ];
    let finished = false;
    while (!finished && result.steps < args.maxSteps) {
      const reply = await chat(args.model, messages);
      const usage = reply.usage ?? {};
      result.agentTokens.prompt += usage.prompt_tokens ?? 0;
      result.agentTokens.completion += usage.completion_tokens ?? 0;
      const msg = reply.choices?.[0]?.message;
      if (!msg) throw new Error("empty completion");
      messages.push(msg);
      const calls = msg.tool_calls ?? [];
      if (calls.length === 0) {
        finished = true;
        result.finishedBy = "done";
        break;
      }
      for (const call of calls) {
        if (result.steps >= args.maxSteps) break;
        let parsed: any = {};
        try {
          parsed = JSON.parse(call.function.arguments || "{}");
        } catch {
          parsed = {};
        }
        if (call.function.name === "done") {
          messages.push({ role: "tool", tool_call_id: call.id, content: "ok" });
          finished = true;
          result.finishedBy = "done";
          result.steps++;
          result.transcript.push({ call: `done ${parsed.summary ?? ""}`.slice(0, 200), snapshotTokens: 0, outputChars: 0 });
          break;
        }
        const cmd = normalize(shellSplit(String(parsed.args ?? "")).filter((t, i) => !(i === 0 && t === "agent-browser")), variant);
        result.steps++;
        let output: string;
        let isError = false;
        const guarded = guardCommand(cmd);
        if (!guarded.ok) {
          output = `Not available in this eval: ${guarded.reason}.`;
          isError = true;
        } else {
          const r = await exec(args.bin, cmd, session);
          output = r.out || "(no output)";
          isError = r.code !== 0;
        }
        let snapTokens = 0;
        if (cmd[0] === "snapshot") {
          snapTokens = countTokens(output);
          result.snapshots++;
          result.snapshotTokens += snapTokens;
          if (output.includes("# warning:")) result.warnings++;
          if (output.includes("# note:")) result.notes++;
        }
        result.transcript.push({ call: cmd.join(" ").slice(0, 200), snapshotTokens: snapTokens, outputChars: output.length, error: isError || undefined });
        messages.push({ role: "tool", tool_call_id: call.id, content: output });
      }
    }
    const events = server.events(run);
    const current = await exec(args.bin, ["get", "url"], session);
    result.events = events;
    result.success = task.check({ events, url: current.out.split("\n")[0].trim() });
  } catch (err) {
    result.finishedBy = "error";
    result.error = err instanceof Error ? err.message : String(err);
  } finally {
    await exec(args.bin, ["close"], session, 20_000).catch(() => undefined);
    result.wallMs = Math.round(performance.now() - started);
  }
  return result;
}

// ---- reporting -----------------------------------------------------------------------------

const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
const sd = (xs: number[]) => {
  if (xs.length < 2) return 0;
  const m = mean(xs);
  return Math.sqrt(mean(xs.map((x) => (x - m) ** 2)));
};

function summarize(results: RunResult[], variants: Variant[], reps: number) {
  const byVariant: Record<string, any> = {};
  for (const v of variants) {
    const rs = results.filter((r) => r.variant === v);
    const ok = rs.filter((r) => r.success);
    // Per-repetition success rates give the variance across repetitions.
    const repRates = Array.from({ length: reps }, (_, k) => {
      const inRep = rs.filter((r) => r.rep === k);
      return inRep.length ? inRep.filter((r) => r.success).length / inRep.length : 0;
    });
    byVariant[v] = {
      runs: rs.length,
      successes: ok.length,
      successRate: rs.length ? ok.length / rs.length : 0,
      successRateByRep: repRates,
      successRateSd: sd(repRates),
      snapshotTokensMean: mean(rs.map((r) => r.snapshotTokens)),
      snapshotTokensMeanOnSuccess: mean(ok.map((r) => r.snapshotTokens)),
      snapshotTokensTotal: rs.reduce((a, r) => a + r.snapshotTokens, 0),
      agentTokensMean: mean(rs.map((r) => r.agentTokens.prompt + r.agentTokens.completion)),
      agentPromptTokensMean: mean(rs.map((r) => r.agentTokens.prompt)),
      stepsMean: mean(rs.map((r) => r.steps)),
      wallSecondsMean: mean(rs.map((r) => r.wallMs / 1000)),
      fallbackWarnings: rs.reduce((a, r) => a + r.warnings, 0),
      frameNotes: rs.reduce((a, r) => a + r.notes, 0),
      iframeSuccesses: rs.filter((r) => r.category === "iframe" && r.success).length,
      iframeRuns: rs.filter((r) => r.category === "iframe").length,
      holdoutSuccesses: rs.filter((r) => r.holdout && r.success).length,
      holdoutRuns: rs.filter((r) => r.holdout).length,
      errors: rs.filter((r) => r.finishedBy === "error").length,
    };
  }
  const taskIds = [...new Set(results.map((r) => r.task))];
  const byTask = taskIds.map((id) => {
    const row: Record<string, unknown> = { task: id, category: results.find((r) => r.task === id)?.category };
    for (const v of variants) {
      const rs = results.filter((r) => r.task === id && r.variant === v);
      row[v] = { success: `${rs.filter((r) => r.success).length}/${rs.length}`, snapshotTokens: Math.round(mean(rs.map((r) => r.snapshotTokens))), steps: Math.round(mean(rs.map((r) => r.steps)) * 10) / 10 };
    }
    return row;
  });
  const categories = [...new Set(results.map((r) => r.category))];
  const byCategory = categories.map((c) => {
    const row: Record<string, unknown> = { category: c };
    for (const v of variants) {
      const rs = results.filter((r) => r.category === c && r.variant === v);
      row[v] = { success: `${rs.filter((r) => r.success).length}/${rs.length}`, snapshotTokensMean: Math.round(mean(rs.map((r) => r.snapshotTokens))) };
    }
    return row;
  });

  // Decision rule, fixed before the run: a variant survives if it improves task success over the
  // baseline, or cuts snapshot tokens by at least 25% at equal success.
  const base = byVariant.ax;
  const verdicts: Record<string, any> = {};
  for (const v of variants.filter((x) => x !== "ax")) {
    const cur = byVariant[v];
    const improvesSuccess = cur.successes > base.successes;
    const equalSuccess = cur.successes >= base.successes;
    const tokenCut = base.snapshotTokensTotal ? 1 - cur.snapshotTokensTotal / base.snapshotTokensTotal : 0;
    const tokenCutOnSuccess = base.snapshotTokensMeanOnSuccess ? 1 - cur.snapshotTokensMeanOnSuccess / base.snapshotTokensMeanOnSuccess : 0;
    verdicts[v] = {
      improvesSuccess,
      equalOrBetterSuccess: equalSuccess,
      snapshotTokenReductionVsAx: tokenCut,
      snapshotTokenReductionOnSuccessVsAx: tokenCutOnSuccess,
      survives: improvesSuccess || (equalSuccess && tokenCut >= 0.25),
    };
  }
  const killRuleFired = !Object.values(verdicts).some((v: any) => v.survives);

  // v2 survival rule, fixed before the v2 run and applied per model over all tasks:
  // (a) success >= ax, (b) mean snapshot tokens <= 0.75 x ax, (c) iframe success >= ax iframe success - 1 run.
  // A variant survives overall only when (a), (b) and (c) hold on every model; see decide.ts.
  const v2: Record<string, any> = {};
  for (const v of variants.filter((x) => x !== "ax")) {
    const cur = byVariant[v];
    const a = cur.successes >= base.successes;
    const b = cur.snapshotTokensMean <= 0.75 * base.snapshotTokensMean;
    const c = cur.iframeSuccesses >= base.iframeSuccesses - 1;
    v2[v] = { a, b, c, holds: a && b && c, tokenRatioVsAx: base.snapshotTokensMean ? cur.snapshotTokensMean / base.snapshotTokensMean : null };
  }
  return { byVariant, byCategory, byTask, verdicts, killRuleFired, v2Rule: v2 };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  await loadTokenizer();
  mkdirSync(resolve(args.out, "runs"), { recursive: true });
  const server = startFixtureServer();
  const selected = tasks.filter((t) => args.only.length === 0 || args.only.includes(t.id));
  const jobs: { task: Task; variant: Variant; rep: number }[] = [];
  for (let rep = 0; rep < args.reps; rep++) for (const task of selected) for (const variant of args.variants) jobs.push({ task, variant, rep });
  console.error(`model=${args.model} bin=${args.bin} tokenizer=${tokenizer} jobs=${jobs.length} out=${args.out}`);

  const results: RunResult[] = [];
  let next = 0;
  const worker = async () => {
    while (next < jobs.length) {
      const job = jobs[next++];
      const r = await runOne(args, server, job.task, job.variant, job.rep);
      results.push(r);
      writeFileSync(resolve(args.out, "runs", `${r.task}.${r.variant}.${r.rep}.json`), JSON.stringify(r, null, 2));
      console.error(`${r.success ? "PASS" : "FAIL"} ${r.task} ${r.variant} rep${r.rep} steps=${r.steps} snapTok=${r.snapshotTokens} ${r.finishedBy}${r.error ? ` (${r.error.slice(0, 80)})` : ""}`);
    }
  };
  await Promise.all(Array.from({ length: args.concurrency }, worker));
  server.stop();

  const summary = {
    model: args.model,
    tokenizer,
    reps: args.reps,
    maxSteps: args.maxSteps,
    tasks: selected.length,
    binary: args.bin,
    ...summarize(results, args.variants, args.reps),
  };
  writeFileSync(resolve(args.out, "summary.json"), JSON.stringify(summary, null, 2));
  writeFileSync(resolve(args.out, "results.json"), JSON.stringify(results, null, 2));
  console.log(JSON.stringify(summary, null, 2));
}

await main();
