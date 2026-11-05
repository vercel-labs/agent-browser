// v3 runner for the snapshot-source A/B eval: local fixture tasks plus public-page tasks, four
// variants (ax, ax-deep, apc, hybrid), several models. Pre-registered in PREREGISTRATION.md.
//
//   bun run snapshot-source/run3.ts --models anthropic/claude-haiku-4.5,openai/gpt-4.1-mini --out <dir>
//
// Environment: AI_GATEWAY_API_KEY, AGENT_BROWSER_BIN (optional), TIKTOKEN_MODULE (optional).
// Runs are resumable: a run whose JSON file exists in <out>/runs is loaded instead of repeated.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";
import { startFixtureServer } from "./fixtures.ts";
import { tasks as localTasks, type Task } from "./tasks.ts";
import { publicTasks, type PublicTask } from "./public-tasks.ts";
import { cliEnv, guardCommand } from "./guard";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "../..");
const GATEWAY = "https://ai-gateway.vercel.sh/v1/chat/completions";

export type Variant = "ax" | "ax-deep" | "apc" | "hybrid";

interface Args {
  variants: Variant[];
  repsLocal: number;
  repsPublic: number;
  models: string[];
  only: string[];
  concurrency: number;
  maxSteps: number;
  delayMs: number;
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
  if (!bin) throw new Error("No agent-browser binary found.");
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  return {
    variants: (get("variants", "ax,ax-deep,apc,hybrid") as string).split(",") as Variant[],
    repsLocal: Number(get("reps-local", "3")),
    repsPublic: Number(get("reps-public", "3")),
    models: (get("models", "anthropic/claude-haiku-4.5") as string).split(","),
    only: (get("tasks", "") as string).split(",").filter(Boolean),
    concurrency: Number(get("concurrency", "2")),
    maxSteps: Number(get("max-steps", "25")),
    delayMs: Number(get("delay-ms", "2000")),
    out: resolve(get("out", resolve(__dirname, `../results/snapshot-source/v3/${stamp}`)) as string),
    bin,
  };
}

// ---- tokens ---------------------------------------------------------------------------------

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

// ---- shell ----------------------------------------------------------------------------------

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

async function exec(bin: string, args: string[], session: string, variant: Variant, timeoutMs = 60_000): Promise<{ out: string; code: number }> {
  const env = cliEnv({ AGENT_BROWSER_SESSION: session });
  if (variant === "ax-deep") env.AGENT_BROWSER_AX_DEEP = "1";
  else delete env.AGENT_BROWSER_AX_DEEP;
  // hybrid is not a public source; the CLI accepts it only with this variable.
  if (variant === "hybrid") env.AGENT_BROWSER_SNAPSHOT_HYBRID = "1";
  else delete env.AGENT_BROWSER_SNAPSHOT_HYBRID;
  const proc = Bun.spawn([bin, ...args], { stdout: "pipe", stderr: "pipe", env });
  const timer = setTimeout(() => proc.kill(), timeoutMs);
  const [stdout, stderr, code] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited]);
  clearTimeout(timer);
  return { out: (stdout + (stderr ? `\n${stderr}` : "")).trim(), code };
}

/** Force the snapshot source and interactive mode onto any snapshot the agent asks for. */
function normalize(cmd: string[], variant: Variant): string[] {
  if (cmd[0] !== "snapshot") return cmd;
  const rest: string[] = [];
  for (let i = 1; i < cmd.length; i++) {
    if (cmd[i] === "--source") i++;
    else if (cmd[i] === "-i" || cmd[i] === "--interactive" || cmd[i] === "--viewport") continue;
    else rest.push(cmd[i]);
  }
  return ["snapshot", "-i", "--source", variant === "ax-deep" ? "ax" : variant, ...rest];
}

// ---- agent ----------------------------------------------------------------------------------

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

Act on the page until the task is done, then call the done tool. If the task asks a question, put the answer in the summary of the done tool. Do not ask questions.`;

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
      description: "Call when the task is complete (or you cannot continue). Put the answer to any question in the summary.",
      parameters: { type: "object", properties: { summary: { type: "string" } }, required: ["summary"] },
    },
  },
];

interface Step {
  call: string;
  snapshotTokens: number;
  outputChars: number;
  error?: boolean;
  /** First part of every snapshot output, kept for the failure analysis. */
  snapshotHead?: string;
}

export interface RunResult {
  task: string;
  model: string;
  category: string;
  site: string;
  public: boolean;
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
  /** Set when the run cannot be scored: blocked by the site, truth unavailable, infrastructure error. */
  invalid?: { kind: "blocked" | "truth" | "open" | "infra"; why: string };
  warnings: number;
  notes: number;
  holdout: boolean;
  answer?: string;
  truth?: unknown;
  truthAfter?: unknown;
  finalUrl?: string;
  events: unknown;
  transcript: Step[];
  startedAt: string;
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
    if (attempt < 4 && (res.status === 429 || res.status >= 500)) {
      await new Promise((r) => setTimeout(r, 3000 * (attempt + 1)));
      continue;
    }
    throw new Error(`gateway ${res.status}: ${body.slice(0, 300)}`);
  }
}

const BLOCK = /captcha|are you (a )?(human|robot)|verify you are (a )?human|access denied|too many requests|rate limit(ed)?|just a moment|unusual traffic|attention required|request blocked|error 1020|403 forbidden|429/i;

/** A block or captcha page is short and says so in its title or first lines. */
async function detectBlock(bin: string, session: string, variant: Variant): Promise<string | undefined> {
  const r = await exec(bin, ["eval", "JSON.stringify({t:document.title,b:(document.body?document.body.innerText:'').slice(0,300),n:(document.body?document.body.innerText.length:0)})"], session, variant, 20_000);
  try {
    let v: any = JSON.parse(r.out.split("\n")[0]);
    if (typeof v === "string") v = JSON.parse(v);
    const head = `${v.t}\n${v.b}`;
    if (BLOCK.test(head) && v.n < 1500) return head.replace(/\s+/g, " ").slice(0, 160);
  } catch {
    // an unreadable page is not by itself a block
  }
  return undefined;
}

interface Job {
  task: Task | PublicTask;
  isPublic: boolean;
  site: string;
  model: string;
  variant: Variant;
  rep: number;
}

const isPub = (t: Task | PublicTask): t is PublicTask => "site" in t;

async function runOne(args: Args, server: ReturnType<typeof startFixtureServer>, job: Job): Promise<RunResult> {
  const { task, variant, rep, model } = job;
  const run = `${task.id}-${variant}-${rep}-${Math.random().toString(36).slice(2, 7)}`;
  const session = `abv3-${run}`.replace(/[^A-Za-z0-9_-]/g, "-").slice(0, 60);
  const started = performance.now();
  const result: RunResult = {
    task: task.id,
    model,
    category: task.category,
    site: job.site,
    public: job.isPublic,
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
    holdout: !isPub(task) && task.holdout === true,
    events: [],
    transcript: [],
    startedAt: new Date().toISOString(),
  };
  const x = (cmd: string[], t?: number) => exec(args.bin, cmd, session, variant, t);
  try {
    let url: string;
    if (isPub(task)) {
      url = task.url;
      if (task.truth) {
        try {
          result.truth = await task.truth();
        } catch (err) {
          result.invalid = { kind: "truth", why: String(err instanceof Error ? err.message : err).slice(0, 160) };
          return result;
        }
      }
    } else {
      url = `http://localhost:${server.port}/r/${run}/${task.page}`;
    }
    const opened = await x(["open", url], 90_000);
    if (opened.code !== 0) {
      result.invalid = { kind: "open", why: opened.out.slice(0, 160) };
      return result;
    }
    if (isPub(task)) {
      const blocked = await detectBlock(args.bin, session, variant);
      if (blocked) {
        result.invalid = { kind: "blocked", why: `at start: ${blocked}` };
        return result;
      }
    }
    const messages: any[] = [
      { role: "system", content: SYSTEM },
      { role: "user", content: `Task: ${task.prompt}` },
    ];
    let finished = false;
    while (!finished && result.steps < args.maxSteps) {
      const reply = await chat(model, messages);
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
        result.answer = String(msg.content ?? "");
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
          result.answer = String(parsed.summary ?? "");
          result.steps++;
          result.transcript.push({ call: `done ${parsed.summary ?? ""}`.slice(0, 300), snapshotTokens: 0, outputChars: 0 });
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
          const r = await x(cmd);
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
        result.transcript.push({
          call: cmd.join(" ").slice(0, 200),
          snapshotTokens: snapTokens,
          outputChars: output.length,
          error: isError || undefined,
          snapshotHead: cmd[0] === "snapshot" ? output.slice(0, 6000) : undefined,
        });
        messages.push({ role: "tool", tool_call_id: call.id, content: output });
      }
    }

    // Scoring. The agent may have left an iframe active, so return to the main frame first.
    await x(["frame", "main"], 20_000);
    const current = await x(["get", "url"], 20_000);
    const finalUrl = current.out.split("\n")[0].trim();
    result.finalUrl = finalUrl;
    if (isPub(task)) {
      const blocked = await detectBlock(args.bin, session, variant);
      if (blocked) {
        result.invalid = { kind: "blocked", why: `at end: ${blocked}` };
        return result;
      }
      let truthAfter: unknown;
      if (task.truth) {
        try {
          truthAfter = await task.truth();
        } catch {
          truthAfter = undefined;
        }
      }
      result.truthAfter = truthAfter;
      const ev = async (js: string) => {
        const r = await x(["eval", js], 30_000);
        const line = r.out.trim();
        try {
          return JSON.parse(line);
        } catch {
          return line;
        }
      };
      result.success = Boolean(await task.check({ url: finalUrl, answer: result.answer ?? "", truth: result.truth, truthAfter, ev }));
    } else {
      const events = server.events(run);
      result.events = events;
      result.success = task.check({ events, url: finalUrl });
    }
  } catch (err) {
    result.finishedBy = "error";
    result.error = err instanceof Error ? err.message : String(err);
  } finally {
    await x(["close"], 20_000).catch(() => undefined);
    result.wallMs = Math.round(performance.now() - started);
  }
  return result;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  await loadTokenizer();
  mkdirSync(resolve(args.out, "runs"), { recursive: true });
  const server = startFixtureServer();

  // Pricing snapshot for the cost estimate.
  const pricing: Record<string, { input: number; output: number }> = {};
  try {
    const res = await fetch("https://ai-gateway.vercel.sh/v1/models", { headers: { Authorization: `Bearer ${process.env.AI_GATEWAY_API_KEY}` } });
    const list = (await res.json()).data as any[];
    for (const m of list) if (args.models.includes(m.id)) pricing[m.id] = { input: Number(m.pricing?.input ?? 0), output: Number(m.pricing?.output ?? 0) };
  } catch {
    // cost stays unknown
  }
  writeFileSync(resolve(args.out, "pricing.json"), JSON.stringify(pricing, null, 2));

  const all: (Task | PublicTask)[] = [...localTasks, ...publicTasks];
  const selected = all.filter((t) => args.only.length === 0 || args.only.includes(t.id));
  // Units are (task, rep). Within a unit every model and variant runs back to back with the
  // variant order rotated, so drift on live pages reaches every variant equally.
  const jobs: Job[] = [];
  const maxRep = Math.max(args.repsLocal, args.repsPublic);
  for (let rep = 0; rep < maxRep; rep++) {
    selected.forEach((task, ti) => {
      const pub = isPub(task);
      if (rep >= (pub ? args.repsPublic : args.repsLocal)) return;
      const order = args.variants.map((_, i) => args.variants[(i + rep + ti) % args.variants.length]);
      for (const model of args.models) for (const variant of order) jobs.push({ task, isPublic: pub, site: pub ? (task as PublicTask).site : "local", model, variant, rep });
    });
  }
  console.error(`models=${args.models.join(",")} bin=${args.bin} tokenizer=${tokenizer} jobs=${jobs.length} out=${args.out}`);
  const file = (j: Job) => resolve(args.out, "runs", `${j.task.id}.${j.model.replace("/", "_")}.${j.variant}.${j.rep}.json`);

  const results: RunResult[] = [];
  const pending = jobs.filter((j) => {
    if (existsSync(file(j))) {
      results.push(JSON.parse(readFileSync(file(j), "utf8")));
      return false;
    }
    return true;
  });
  console.error(`resumed=${results.length} pending=${pending.length}`);

  const busySites = new Set<string>();
  const lastEnd = new Map<string, number>();
  const take = (): Job | undefined => {
    const now = Date.now();
    const i = pending.findIndex((j) => j.site === "local" || (!busySites.has(j.site) && now - (lastEnd.get(j.site) ?? 0) >= args.delayMs));
    return i >= 0 ? pending.splice(i, 1)[0] : undefined;
  };
  const worker = async () => {
    while (pending.length > 0) {
      const job = take();
      if (!job) {
        await new Promise((r) => setTimeout(r, 300));
        continue;
      }
      busySites.add(job.site);
      let r = await runOne(args, server, job);
      for (let retry = 0; retry < 2 && r.finishedBy === "error" && !r.invalid; retry++) {
        await new Promise((res) => setTimeout(res, 5000));
        r = await runOne(args, server, job);
      }
      if (r.finishedBy === "error" && !r.invalid) r.invalid = { kind: "infra", why: String(r.error).slice(0, 160) };
      lastEnd.set(job.site, Date.now());
      busySites.delete(job.site);
      results.push(r);
      writeFileSync(file(job), JSON.stringify(r, null, 2));
      console.error(`${r.invalid ? `INVALID(${r.invalid.kind})` : r.success ? "PASS" : "FAIL"} ${r.model.split("/")[1]} ${r.task} ${r.variant} rep${r.rep} steps=${r.steps} snapTok=${r.snapshotTokens} ${r.finishedBy}${r.invalid ? ` ${r.invalid.why.slice(0, 60)}` : ""}`);
    }
  };
  await Promise.all(Array.from({ length: args.concurrency }, worker));
  server.stop();
  writeFileSync(resolve(args.out, "results.json"), JSON.stringify(results, null, 2));
  writeFileSync(resolve(args.out, "meta.json"), JSON.stringify({ models: args.models, variants: args.variants, repsLocal: args.repsLocal, repsPublic: args.repsPublic, maxSteps: args.maxSteps, tokenizer, binary: args.bin, finished: new Date().toISOString() }, null, 2));
}

await main();
