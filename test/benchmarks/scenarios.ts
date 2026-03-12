export interface BenchmarkCommand {
  id: string;
  action: string;
  [key: string]: unknown;
}

export interface Scenario {
  name: string;
  description: string;
  /** Commands to run once before measured iterations (e.g. navigate to a page). */
  setup?: BenchmarkCommand[];
  /** The commands whose total execution time is measured per iteration. */
  commands: BenchmarkCommand[];
  /** Commands to run once after measured iterations (e.g. cleanup). */
  teardown?: BenchmarkCommand[];
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

const INJECT_FORM: BenchmarkCommand = {
  id: "inject",
  action: "evaluate",
  script: `document.open(); document.write(${JSON.stringify(FORM_HTML)}); document.close(); 'ok'`,
};

const SETUP_PAGE: BenchmarkCommand[] = [
  { id: "setup-nav", action: "navigate", url: "about:blank", waitUntil: "domcontentloaded" },
  INJECT_FORM,
];

export const scenarios: Scenario[] = [
  {
    name: "navigate",
    description: "Page navigation (about:blank round-trip)",
    commands: [
      { id: "nav", action: "navigate", url: "about:blank", waitUntil: "domcontentloaded" },
    ],
  },
  {
    name: "snapshot",
    description: "DOM snapshot (accessibility tree)",
    setup: SETUP_PAGE,
    commands: [
      { id: "snap", action: "snapshot" },
    ],
  },
  {
    name: "screenshot",
    description: "Screenshot capture",
    setup: SETUP_PAGE,
    commands: [
      { id: "ss", action: "screenshot" },
    ],
  },
  {
    name: "evaluate",
    description: "JavaScript evaluation",
    setup: SETUP_PAGE,
    commands: [
      { id: "eval", action: "evaluate", script: "document.title + ' ' + document.querySelectorAll('li').length" },
    ],
  },
  {
    name: "click",
    description: "Element click interaction",
    setup: SETUP_PAGE,
    commands: [
      { id: "clk", action: "click", selector: "#link" },
    ],
  },
  {
    name: "fill",
    description: "Form field fill",
    setup: SETUP_PAGE,
    commands: [
      { id: "fill", action: "fill", selector: "#name", value: "Benchmark User" },
    ],
  },
  {
    name: "tabs",
    description: "Tab new + list + switch",
    commands: [
      { id: "tnew", action: "tab_new", url: "about:blank" },
      { id: "tlist", action: "tab_list" },
      { id: "tswitch", action: "tab_switch", index: 0 },
    ],
    teardown: [
      { id: "tclose", action: "tab_close", index: 1 },
    ],
  },
  {
    name: "full-workflow",
    description: "Realistic agent workflow: navigate, snapshot, click, fill, evaluate, screenshot",
    commands: [
      { id: "w-nav", action: "navigate", url: "about:blank", waitUntil: "domcontentloaded" },
      INJECT_FORM,
      { id: "w-snap", action: "snapshot" },
      { id: "w-click", action: "click", selector: "#link" },
      { id: "w-fill", action: "fill", selector: "#name", value: "Agent User" },
      { id: "w-eval", action: "evaluate", script: "document.getElementById('name').value" },
      { id: "w-ss", action: "screenshot" },
    ],
  },
];
