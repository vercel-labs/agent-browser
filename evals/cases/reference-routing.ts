import { readFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";
import type { EvalCase } from "../lib/types.ts";

const __dirname = dirname(fileURLToPath(import.meta.url));
const CORE_PATH = resolve(__dirname, "../../skill-data/core/SKILL.md");

/** Simulates an agent that already ran `agent-browser skills get core`. */
export function coreContext(core = readFileSync(CORE_PATH, "utf-8")): string {
  return [
    "You already ran `agent-browser skills get core`. It printed:",
    "<skill-output>",
    core,
    "</skill-output>",
  ].join("\n");
}

const RUBRIC = `
1 - Guesses commands without loading the reference the task needs
2 - Loads every reference with --full instead of the one it needs
3 - Loads a related but wrong reference
4 - Loads the right reference with skills get core --ref
5 - Loads the right reference with --ref, then uses the commands it documents
`.trim();

const FULL_OR_PATH = ["skills get core[^\\n]*--full", "skills path"];

/** Matches `skills get core --ref <name>` in any form the CLI resolves: stem, file name, or path. */
function refPattern(stem: string): string {
  return `skills get core[^\\n]*--ref[ =]['"]?(references/)?${stem}(\\.md)?(?![\\w-])`;
}

export const cases: EvalCase[] = [
  {
    id: "rr-01",
    name: "Loads proxy reference for an authenticated proxy with a custom CA",
    category: "reference-routing",
    prompt:
      "Scrape https://intranet.example.com through our corporate proxy at proxy.corp:8080 with a username and password, trusting our internal CA certificate at ./corp-ca.pem",
    expectedPatterns: [refPattern("proxy-support")],
    forbiddenPatterns: FULL_OR_PATH,
    rubric: RUBRIC,
  },
  {
    id: "rr-02",
    name: "Loads video reference for a recording with cursor overlay",
    category: "reference-routing",
    prompt:
      "Record a video of logging into https://app.example.com with the cursor visible, then produce a contact sheet of the recording",
    expectedPatterns: [refPattern("video-recording")],
    forbiddenPatterns: FULL_OR_PATH,
    rubric: RUBRIC,
  },
  {
    id: "rr-03",
    name: "Loads authentication reference for a credential provider plugin",
    category: "reference-routing",
    prompt:
      "Log into https://dashboard.example.com using credentials from our 1Password credential provider plugin instead of typing the password",
    expectedPatterns: [refPattern("authentication")],
    forbiddenPatterns: FULL_OR_PATH,
    rubric: RUBRIC,
  },
  {
    id: "rr-04",
    name: "Loads WebGPU reference for a headless Linux CI screenshot",
    category: "reference-routing",
    prompt:
      "In our headless Linux CI job, take a screenshot of https://demo.example.com, a three.js page that renders with WebGPU",
    expectedPatterns: [refPattern("webgpu")],
    forbiddenPatterns: FULL_OR_PATH,
    rubric: RUBRIC,
  },
  {
    id: "rr-05",
    name: "Loads streaming reference to share a live viewport",
    category: "reference-routing",
    prompt:
      "Stream the live browser viewport of https://app.example.com so a teammate can watch and send remote input while you work",
    expectedPatterns: [refPattern("streaming")],
    forbiddenPatterns: FULL_OR_PATH,
    rubric: RUBRIC,
  },
  {
    id: "rr-06",
    name: "Loads profiling reference for a DevTools trace",
    category: "reference-routing",
    prompt:
      "Capture a Chrome DevTools performance trace while https://shop.example.com loads its product grid, and save the trace file",
    expectedPatterns: [refPattern("profiling")],
    forbiddenPatterns: FULL_OR_PATH,
    rubric: RUBRIC,
  },
  {
    id: "rr-07",
    name: "Loads no reference for a task core already covers",
    category: "reference-routing",
    prompt: "Take a screenshot of https://example.com and save it as example.png",
    expectedPatterns: ["agent-browser\\b[^\\n]*\\b(open|screenshot)\\b"],
    forbiddenPatterns: ["--ref", ...FULL_OR_PATH],
    rubric: RUBRIC,
  },
];

for (const c of cases) c.context = coreContext();
