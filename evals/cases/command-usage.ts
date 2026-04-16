import type { EvalCase } from "../lib/types.ts";

const RUBRIC = `
1 - Agent does not produce valid agent-browser commands
2 - Agent uses agent-browser but with wrong commands or missing steps
3 - Agent uses correct commands but skips the snapshot-interact workflow
4 - Agent follows the correct workflow with appropriate commands
5 - Agent follows the optimal workflow: navigate, snapshot, interact with refs, re-snapshot as needed
`.trim();

const COMMAND_CONTEXT = `You already ran \`agent-browser skills get agent-browser\` and loaded these commands:
- agent-browser open <url> (navigate to a page)
- agent-browser snapshot -i (get interactive elements with refs like @e1, @e2)
- agent-browser click @ref (click element)
- agent-browser fill @ref "text" (clear and type)
- agent-browser type @ref "text" (type without clearing)
- agent-browser select @ref "option" (select dropdown)
- agent-browser screenshot (screenshot to temp dir)
- agent-browser screenshot --full (full page screenshot)
- agent-browser diff url <url1> <url2> (compare two pages)
- agent-browser diff snapshot (compare current vs last snapshot)
- agent-browser state save ./file.json (save auth state)
- agent-browser state load ./file.json (restore auth state)
- agent-browser get text @ref (get element text)
- agent-browser wait <selector|ms> (wait for element or time)
- agent-browser --session-name <name> open <url> (named session with auto-save)

Workflow: open -> snapshot -i -> interact with refs -> re-snapshot after changes.`;

export const cases: EvalCase[] = [
  {
    id: "cu-01",
    name: "Navigate and screenshot workflow",
    category: "command-usage",
    prompt: "Open example.com and take a screenshot",
    context: COMMAND_CONTEXT,
    expectedPatterns: [
      "agent-browser\\s+(open|goto|navigate)",
      "agent-browser\\s+screenshot",
    ],
    rubric: RUBRIC,
  },
  {
    id: "cu-02",
    name: "Form filling workflow",
    category: "command-usage",
    prompt:
      "Go to example.com/signup, fill in name as 'Jane Doe' and email as 'jane@test.com', then submit",
    context: COMMAND_CONTEXT,
    expectedPatterns: [
      "agent-browser\\s+(open|goto|navigate)",
      "agent-browser\\s+snapshot",
      "agent-browser\\s+(fill|type)",
      "agent-browser\\s+(click|press|key)",
    ],
    rubric: RUBRIC,
  },
  {
    id: "cu-03",
    name: "Snapshot with element refs",
    category: "command-usage",
    prompt: "Get all interactive elements on example.com",
    context: COMMAND_CONTEXT,
    expectedPatterns: [
      "agent-browser\\s+(open|goto|navigate)",
      "agent-browser\\s+snapshot",
    ],
    rubric: RUBRIC,
  },
  {
    id: "cu-04",
    name: "Diff comparison workflow",
    category: "command-usage",
    prompt:
      "Compare the homepage of staging.example.com and prod.example.com",
    context: COMMAND_CONTEXT,
    expectedPatterns: [
      "agent-browser\\s+diff|staging\\.example\\.com.*prod\\.example\\.com",
    ],
    rubric: RUBRIC,
  },
  {
    id: "cu-05",
    name: "Authentication with state persistence",
    category: "command-usage",
    prompt:
      "Log into app.example.com, then save the auth state for future sessions",
    context: COMMAND_CONTEXT,
    expectedPatterns: [
      "agent-browser\\s+(open|goto|navigate)",
      "state\\s+save|--session-name|auth\\s+save",
    ],
    rubric: RUBRIC,
  },
  {
    id: "cu-06",
    name: "Data extraction workflow",
    category: "command-usage",
    prompt:
      "Extract the text content of the main heading on example.com",
    context: COMMAND_CONTEXT,
    expectedPatterns: [
      "agent-browser\\s+(open|goto|navigate)",
      "snapshot|get\\s+text",
    ],
    rubric: RUBRIC,
  },
  {
    id: "cu-07",
    name: "Full-page screenshot",
    category: "command-usage",
    prompt: "Take a full-page screenshot of example.com",
    context: COMMAND_CONTEXT,
    expectedPatterns: [
      "agent-browser\\s+(open|goto|navigate|screenshot)",
      "screenshot.*--full",
    ],
    rubric: RUBRIC,
  },
];
