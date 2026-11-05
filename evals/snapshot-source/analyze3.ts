// Analysis of a v3 run directory, as pre-registered in PREREGISTRATION.md. Reads <dir>/runs/*.json,
// writes <dir>/summary.json and prints the tables.
//
//   bun run snapshot-source/analyze3.ts <dir> [summary-out.json]

import { readdirSync, readFileSync, writeFileSync } from "fs";
import { resolve } from "path";
import type { RunResult } from "./run3.ts";

const dir = resolve(process.argv[2]);
const runs: RunResult[] = readdirSync(resolve(dir, "runs")).filter((f) => f.endsWith(".json")).map((f) => JSON.parse(readFileSync(resolve(dir, "runs", f), "utf8")));
const pricing: Record<string, { input: number; output: number }> = JSON.parse(readFileSync(resolve(dir, "pricing.json"), "utf8"));
const VARIANTS = ["ax", "ax-deep", "apc", "hybrid"] as const;
const models = [...new Set(runs.map((r) => r.model))].sort();

// ---- validity -------------------------------------------------------------------------------
// A block, open failure or unavailable ground truth voids the unit (task, rep) for every model
// and variant. An infrastructure error voids (task, rep, model).
const voidUnit = new Set<string>();
const voidModelUnit = new Set<string>();
for (const r of runs) {
  if (!r.invalid) continue;
  if (r.invalid.kind === "infra") voidModelUnit.add(`${r.task}|${r.rep}|${r.model}`);
  else voidUnit.add(`${r.task}|${r.rep}`);
}
const valid = runs.filter((r) => !voidUnit.has(`${r.task}|${r.rep}`) && !voidModelUnit.has(`${r.task}|${r.rep}|${r.model}`) && !r.invalid);
// Keep only complete units (all four variants present) so comparisons are paired.
const key = (r: RunResult) => `${r.task}|${r.rep}|${r.model}`;
const byUnit = new Map<string, Map<string, RunResult>>();
for (const r of valid) {
  if (!byUnit.has(key(r))) byUnit.set(key(r), new Map());
  byUnit.get(key(r))!.set(r.variant, r);
}
const units = [...byUnit.entries()].filter(([, m]) => VARIANTS.every((v) => m.has(v))).map(([k, m]) => ({ k, task: k.split("|")[0], rep: Number(k.split("|")[1]), model: k.split("|")[2], m }));

const invalidBySite: Record<string, { runs: number; kinds: Record<string, number>; tasks: string[] }> = {};
for (const r of runs.filter((x) => x.invalid)) {
  const s = (invalidBySite[r.site] ??= { runs: 0, kinds: {}, tasks: [] });
  s.runs++;
  s.kinds[r.invalid!.kind] = (s.kinds[r.invalid!.kind] ?? 0) + 1;
  if (!s.tasks.includes(r.task)) s.tasks.push(r.task);
}

// ---- stats ----------------------------------------------------------------------------------
let seed = 20261105;
const rnd = () => {
  seed = (seed * 1664525 + 1013904223) % 4294967296;
  return seed / 4294967296;
};
const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
const q = (xs: number[], p: number) => {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.max(0, Math.floor(p * s.length)))];
};
type U = (typeof units)[number];
const B = 5000;

/** Task-clustered bootstrap: resample tasks with replacement, keep all their units. */
function boot(us: U[], stat: (sample: U[]) => number): [number, number] {
  const byTask = new Map<string, U[]>();
  for (const u of us) (byTask.get(u.task) ?? byTask.set(u.task, []).get(u.task)!).push(u);
  const ts = [...byTask.values()];
  if (ts.length === 0) return [NaN, NaN];
  const out: number[] = [];
  for (let b = 0; b < B; b++) {
    const sample: U[] = [];
    for (let i = 0; i < ts.length; i++) sample.push(...ts[Math.floor(rnd() * ts.length)]);
    const v = stat(sample);
    if (Number.isFinite(v)) out.push(v);
  }
  return [q(out, 0.025), q(out, 0.975)];
}
const rate = (us: U[], v: string) => mean(us.map((u) => (u.m.get(v)!.success ? 1 : 0)));
const tok = (us: U[], v: string) => mean(us.map((u) => u.m.get(v)!.snapshotTokens));
const diff = (us: U[], v: string) => rate(us, v) - rate(us, "ax-deep");
const ratio = (us: U[], v: string) => tok(us, v) / tok(us, "ax-deep");

/** Exact two-sided McNemar (binomial) on discordant pairs of v against ax-deep. */
function mcnemar(us: U[], v: string) {
  let lose = 0; // v failed, ax-deep succeeded
  let win = 0;
  for (const u of us) {
    const a = u.m.get(v)!.success;
    const d = u.m.get("ax-deep")!.success;
    if (!a && d) lose++;
    else if (a && !d) win++;
  }
  const n = lose + win;
  const k = Math.min(lose, win);
  let p = 1;
  if (n > 0) {
    let c = 0;
    let term = Math.pow(0.5, n);
    for (let i = 0; i <= k; i++) {
      c += term;
      term = (term * (n - i)) / (i + 1);
    }
    p = Math.min(1, 2 * c);
  }
  return { lose, win, p, significantLoss: p < 0.05 && lose > win };
}

function block(us: U[]) {
  const o: Record<string, any> = { units: us.length };
  for (const v of VARIANTS) {
    const rs = us.map((u) => u.m.get(v)!);
    const succ = rs.filter((r) => r.success).length;
    const row: Record<string, any> = {
      successes: succ,
      runs: rs.length,
      successRate: rate(us, v),
      successCI95: boot(us, (s) => rate(s, v)),
      snapshotTokensMean: tok(us, v),
      agentTokensMean: mean(rs.map((r) => r.agentTokens.prompt + r.agentTokens.completion)),
      stepsMean: mean(rs.map((r) => r.steps)),
    };
    if (v !== "ax-deep") {
      row.diffVsAxDeep = diff(us, v);
      row.diffVsAxDeepCI95 = boot(us, (s) => diff(s, v));
      row.tokenRatioVsAxDeep = ratio(us, v);
      row.tokenRatioVsAxDeepCI95 = boot(us, (s) => ratio(s, v));
      row.mcnemarVsAxDeep = mcnemar(us, v);
    }
    o[v] = row;
  }
  return o;
}

const perModel: Record<string, any> = {};
for (const m of models) perModel[m] = { all: block(units.filter((u) => u.model === m)) };
const pooled = block(units);
const isPublic = (u: U) => u.m.get("ax")!.public;
const split = {
  public: block(units.filter(isPublic)),
  local: block(units.filter((u) => !isPublic(u))),
  holdoutPlusPublic: block(units.filter((u) => isPublic(u) || u.m.get("ax")!.holdout)),
};
for (const m of models) {
  perModel[m].public = block(units.filter((u) => u.model === m && isPublic(u)));
  perModel[m].local = block(units.filter((u) => u.model === m && !isPublic(u)));
}

// ---- pre-registered rule --------------------------------------------------------------------
function rule(v: string) {
  const a = Object.fromEntries(models.map((m) => [m, perModel[m].all[v].successRate >= perModel[m].all["ax-deep"].successRate - 0.02]));
  const b = Object.fromEntries(models.map((m) => [m, perModel[m].all[v].snapshotTokensMean <= 0.75 * perModel[m].all["ax-deep"].snapshotTokensMean]));
  const hp = split.holdoutPlusPublic;
  const c = hp[v].successRate >= hp["ax-deep"].successRate - 0.02;
  const d = !pooled[v].mcnemarVsAxDeep.significantLoss;
  const A = Object.values(a).every(Boolean);
  const Bv = Object.values(b).every(Boolean);
  return { a, aHolds: A, b, bHolds: Bv, c, cDetail: { v: hp[v].successRate, axDeep: hp["ax-deep"].successRate }, d, dDetail: pooled[v].mcnemarVsAxDeep, survives: A && Bv && c && d };
}
const verdict: Record<string, any> = { apc: rule("apc"), hybrid: rule("hybrid") };

// ---- breakdowns -----------------------------------------------------------------------------
function groupBy(keyf: (u: U) => string) {
  const g = new Map<string, U[]>();
  for (const u of units) (g.get(keyf(u)) ?? g.set(keyf(u), []).get(keyf(u))!).push(u);
  return Object.fromEntries([...g.entries()].sort().map(([k, us]) => [k, Object.fromEntries([["units", us.length], ...VARIANTS.map((v) => [v, `${us.filter((u) => u.m.get(v)!.success).length}/${us.length}`]), ...VARIANTS.map((v) => [`tok_${v}`, Math.round(tok(us, v))])])]));
}
const byCategory = groupBy((u) => `${isPublic(u) ? "public" : "local"}:${u.m.get("ax")!.category}`);
const bySite = groupBy((u) => u.m.get("ax")!.site);
const byTask = groupBy((u) => u.task);
const losses = Object.entries(byTask)
  .map(([task, row]: [string, any]) => ({ task, apc: row.apc, axDeep: row["ax-deep"], hybrid: row.hybrid, ax: row.ax }))
  .filter((r) => Number(r.apc.split("/")[0]) < Number(r.axDeep.split("/")[0]));

// ---- cost -----------------------------------------------------------------------------------
const cost: Record<string, number> = {};
for (const r of runs) {
  const p = pricing[r.model];
  if (!p) continue;
  cost[r.model] = (cost[r.model] ?? 0) + r.agentTokens.prompt * p.input + r.agentTokens.completion * p.output;
}
const totalCost = Object.values(cost).reduce((a, b) => a + b, 0);

const summary = {
  dir,
  runsTotal: runs.length,
  runsInvalid: runs.filter((r) => r.invalid).length,
  invalidBySite,
  unitsPaired: units.length,
  models,
  perModel,
  pooled,
  split,
  verdict,
  byCategory,
  bySite,
  byTask,
  losses,
  costUsdUpperBound: { perModel: cost, total: totalCost, note: "prompt and completion tokens at gateway list price, no caching discount" },
};
writeFileSync(resolve(dir, "summary.json"), JSON.stringify(summary, null, 2));
if (process.argv[3]) writeFileSync(resolve(process.argv[3]), JSON.stringify(summary, null, 2));

const f = (x: number) => (x * 100).toFixed(1) + "%";
const ci = (c: number[], pct = true) => (pct ? `[${(c[0] * 100).toFixed(1)}, ${(c[1] * 100).toFixed(1)}]` : `[${c[0].toFixed(2)}, ${c[1].toFixed(2)}]`);
function print(name: string, b: any) {
  console.log(`\n## ${name} (units=${b.units})`);
  for (const v of VARIANTS) {
    const r = b[v];
    console.log(`${v.padEnd(8)} success ${r.successes}/${r.runs} ${f(r.successRate)} CI${ci(r.successCI95)}  snapTok ${Math.round(r.snapshotTokensMean)}  agentTok ${Math.round(r.agentTokensMean)}` + (r.diffVsAxDeep !== undefined ? `  diff ${(r.diffVsAxDeep * 100).toFixed(1)}pp CI${ci(r.diffVsAxDeepCI95)}  tokRatio ${r.tokenRatioVsAxDeep.toFixed(2)} CI${ci(r.tokenRatioVsAxDeepCI95, false)}  mcnemar lose=${r.mcnemarVsAxDeep.lose} win=${r.mcnemarVsAxDeep.win} p=${r.mcnemarVsAxDeep.p.toFixed(3)}` : ""));
  }
}
for (const m of models) {
  print(`${m} all`, perModel[m].all);
  print(`${m} public`, perModel[m].public);
  print(`${m} local`, perModel[m].local);
}
print("pooled", pooled);
print("pooled public", split.public);
print("pooled local", split.local);
print("pooled holdout+public", split.holdoutPlusPublic);
console.log("\nverdict", JSON.stringify(verdict, null, 1));
console.log("\ninvalid by site", JSON.stringify(invalidBySite));
console.log("\ncost upper bound USD", JSON.stringify(summary.costUsdUpperBound));
