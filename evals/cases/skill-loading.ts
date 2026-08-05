import type { EvalCase } from "../lib/types.ts";

const RUBRIC = `
1 - Agent does not mention agent-browser skills or load any skill
2 - Agent mentions skills but does not run the skills get command
3 - Agent runs skills get but for the wrong skill or at the wrong time
4 - Agent runs skills get for the correct skill before using agent-browser
5 - Agent runs skills get first, then follows the loaded skill's workflow correctly
`.trim();

export const cases: EvalCase[] = [
  {
    id: "sl-01",
    name: "Loads skill before opening a page",
    category: "skill-loading",
    prompt: "Open example.com and take a screenshot",
    expectedPatterns: [
      "agent-browser skills get",
    ],
    rubric: RUBRIC,
  },
  {
    id: "sl-02",
    name: "Loads skill before form interaction",
    category: "skill-loading",
    prompt:
      "Go to example.com/contact and fill out the contact form with test data",
    expectedPatterns: [
      "agent-browser skills get",
    ],
    rubric: RUBRIC,
  },
  {
    id: "sl-03",
    name: "Loads skill before data extraction",
    category: "skill-loading",
    prompt:
      "Scrape all product names and prices from shop.example.com",
    expectedPatterns: [
      "agent-browser skills get",
    ],
    rubric: RUBRIC,
  },
  {
    id: "sl-04",
    name: "Loads skill before authentication task",
    category: "skill-loading",
    prompt:
      "Log into my GitHub account and check my notifications",
    expectedPatterns: [
      "agent-browser skills get",
    ],
    rubric: RUBRIC,
  },
  {
    id: "sl-05",
    name: "Uses skills list to discover available skills",
    category: "skill-loading",
    prompt:
      "I need to automate some browser tasks. What skills are available for agent-browser?",
    expectedPatterns: [
      "agent-browser skills (list|get)",
    ],
    rubric: RUBRIC,
  },
];
