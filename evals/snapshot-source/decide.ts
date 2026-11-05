// Applies the v2 survival rule across models: a variant survives only if (a), (b) and (c) hold on
// every model's summary.json.
//
//   bun run snapshot-source/decide.ts <summary.json> [<summary.json> ...]

import { readFileSync } from "fs";

const files = process.argv.slice(2);
if (files.length === 0) throw new Error("pass one summary.json per model");
const summaries = files.map((f) => JSON.parse(readFileSync(f, "utf8")));
const variants = Object.keys(summaries[0].v2Rule);
const out: Record<string, unknown> = {};
for (const v of variants) {
  out[v] = {
    perModel: summaries.map((s) => ({ model: s.model, ...s.v2Rule[v] })),
    survives: summaries.every((s) => s.v2Rule[v].holds),
  };
}
console.log(JSON.stringify(out, null, 2));
