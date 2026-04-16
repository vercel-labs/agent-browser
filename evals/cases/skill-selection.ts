import type { EvalCase } from "../lib/types.ts";

const RUBRIC = `
1 - Agent does not load any skill or loads a completely wrong one
2 - Agent loads the generic agent-browser skill when a specialized one exists
3 - Agent loads a related but suboptimal skill
4 - Agent loads the correct specialized skill
5 - Agent loads the correct skill and explains why it chose it
`.trim();

export const cases: EvalCase[] = [
  {
    id: "ss-01",
    name: "Selects slack skill for Slack tasks",
    category: "skill-selection",
    prompt: "Check my Slack unreads and summarize any messages mentioning me",
    expectedPatterns: [
      "skills get slack",
    ],
    rubric: RUBRIC,
  },
  {
    id: "ss-02",
    name: "Selects electron skill for VS Code automation",
    category: "skill-selection",
    prompt: "Automate VS Code to open a project and run a terminal command",
    expectedPatterns: [
      "skills get electron",
    ],
    rubric: RUBRIC,
  },
  {
    id: "ss-03",
    name: "Selects dogfood skill for QA/testing",
    category: "skill-selection",
    prompt: "QA test http://localhost:3000 and find any bugs or UX issues",
    expectedPatterns: [
      "skills get dogfood",
    ],
    rubric: RUBRIC,
  },
  {
    id: "ss-04",
    name: "Selects agentcore skill for AWS cloud browsers",
    category: "skill-selection",
    prompt:
      "Run browser automation on AWS using AgentCore cloud browsers",
    expectedPatterns: [
      "skills get agentcore",
    ],
    rubric: RUBRIC,
  },
  {
    id: "ss-05",
    name: "Selects vercel-sandbox skill for Vercel environments",
    category: "skill-selection",
    prompt:
      "Run headless Chrome inside a Vercel Sandbox microVM to test my deployed Next.js app",
    expectedPatterns: [
      "skills get vercel-sandbox",
    ],
    rubric: RUBRIC,
  },
  {
    id: "ss-06",
    name: "Selects electron skill for Discord automation",
    category: "skill-selection",
    prompt: "Automate the Discord desktop app to send a message in a channel",
    expectedPatterns: [
      "skills get electron",
    ],
    rubric: RUBRIC,
  },
  {
    id: "ss-07",
    name: "Selects dogfood skill for exploratory testing",
    category: "skill-selection",
    prompt: "Dogfood vercel.com and write up a bug report",
    expectedPatterns: [
      "skills get dogfood",
    ],
    rubric: RUBRIC,
  },
  {
    id: "ss-08",
    name: "Selects agent-browser skill for general browser tasks",
    category: "skill-selection",
    prompt: "Navigate to hacker news and screenshot the front page",
    expectedPatterns: [
      "skills get agent-browser",
    ],
    rubric: RUBRIC,
  },
];
