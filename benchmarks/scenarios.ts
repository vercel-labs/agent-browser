/**
 * Benchmark scenarios for comparing Node.js daemon vs Rust native daemon.
 *
 * Each scenario defines CLI commands run via `sandbox.runCommand("agent-browser", args)`.
 * Setup/teardown commands run once and are not timed.
 * The `commands` array is timed over N iterations.
 */

export interface Scenario {
  name: string;
  description: string;
  setup?: string[][];
  commands: string[][];
  teardown?: string[][];
}

const FORM_HTML = [
  "<html><head><title>Bench</title></head><body>",
  "<h1>Benchmark Page</h1>",
  "<input id='name' type='text' placeholder='Name'>",
  "<input id='email' type='email' placeholder='Email'>",
  "<select id='color'><option value='red'>Red</option><option value='blue'>Blue</option></select>",
  "<input id='agree' type='checkbox'>",
  "<textarea id='bio' placeholder='Bio'></textarea>",
  "<button id='submit'>Submit</button>",
  "<p id='status'>Ready</p>",
  "<a id='link' href='javascript:void(0)' onclick=\"document.getElementById('status').textContent='Clicked'\">Click me</a>",
  "<ul>",
  ...Array.from({ length: 20 }, (_, i) => `<li class='item'>Item ${i + 1}</li>`),
  "</ul>",
  "</body></html>",
].join("");

const INJECT_FORM_SCRIPT = `document.open(); document.write(${JSON.stringify(FORM_HTML)}); document.close(); 'ok'`;

const SETUP_PAGE: string[][] = [
  ["open", "about:blank"],
  ["eval", INJECT_FORM_SCRIPT],
];

export const scenarios: Scenario[] = [
  {
    name: "navigate",
    description: "Page navigation (about:blank round-trip)",
    commands: [["open", "about:blank"]],
  },
  {
    name: "snapshot",
    description: "DOM snapshot (accessibility tree)",
    setup: SETUP_PAGE,
    commands: [["snapshot"]],
  },
  {
    name: "screenshot",
    description: "Screenshot capture",
    setup: SETUP_PAGE,
    commands: [["screenshot"]],
  },
  {
    name: "evaluate",
    description: "JavaScript evaluation",
    setup: SETUP_PAGE,
    commands: [
      [
        "eval",
        "document.title + ' ' + document.querySelectorAll('li').length",
      ],
    ],
  },
  {
    name: "click",
    description: "Element click interaction",
    setup: SETUP_PAGE,
    commands: [["click", "#link"]],
  },
  {
    name: "fill",
    description: "Form field fill",
    setup: SETUP_PAGE,
    commands: [["fill", "#name", "Benchmark User"]],
  },
  {
    name: "agent-loop",
    description: "AI agent loop: snapshot -> click -> snapshot (typical agent cycle)",
    setup: SETUP_PAGE,
    commands: [["snapshot"], ["click", "#link"], ["snapshot"]],
  },
  {
    name: "full-workflow",
    description:
      "Realistic workflow: navigate, inject form, snapshot, click, fill, evaluate, screenshot",
    commands: [
      ["open", "about:blank"],
      ["eval", INJECT_FORM_SCRIPT],
      ["snapshot"],
      ["click", "#link"],
      ["fill", "#name", "Agent User"],
      [
        "eval",
        "document.getElementById('name').value",
      ],
      ["screenshot"],
    ],
  },
];
