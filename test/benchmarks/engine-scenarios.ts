import type { BenchmarkCommand, Scenario } from "./scenarios.js";

// ---------------------------------------------------------------------------
// HTML generators for realistic pages with complex DOM structures
// ---------------------------------------------------------------------------

function generateArticlePage(): string {
  const paragraphs = Array.from({ length: 30 }, (_, i) => {
    const words = Array.from(
      { length: 40 + (i % 5) * 10 },
      (_, w) => ["the", "quick", "browser", "engine", "renders", "content", "across", "multiple", "layout", "passes", "while", "handling", "style", "recalculations", "and", "DOM", "mutations"][w % 17],
    ).join(" ");
    return `<p class="article-p">${words}</p>`;
  });

  const comments = Array.from(
    { length: 40 },
    (_, i) =>
      `<div class="comment" data-id="${i}">` +
      `<div class="comment-header"><span class="author">User ${i}</span><time>2025-01-${String(i % 28 + 1).padStart(2, "0")}</time></div>` +
      `<div class="comment-body"><p>This is comment number ${i + 1} with some discussion text.</p></div>` +
      `<div class="comment-actions"><button class="reply-btn">Reply</button><button class="like-btn">Like</button></div>` +
      `</div>`,
  );

  const sidebar = Array.from(
    { length: 20 },
    (_, i) =>
      `<li class="sidebar-item"><a href="#section-${i}">Related Article ${i + 1}: A Longer Title Here</a></li>`,
  );

  return [
    "<html><head><title>Benchmark Article</title>",
    "<style>",
    "body{font-family:system-ui;margin:0;padding:0;display:grid;grid-template-columns:1fr 300px;gap:20px;max-width:1200px;margin:0 auto}",
    ".article{padding:20px}.sidebar{padding:20px;border-left:1px solid #ddd}",
    ".comment{border:1px solid #eee;padding:12px;margin:8px 0;border-radius:4px}",
    ".comment-header{display:flex;justify-content:space-between;font-size:14px;color:#666}",
    ".nav{display:flex;gap:16px;padding:12px 20px;background:#f5f5f5;grid-column:1/-1}",
    ".tag{display:inline-block;padding:2px 8px;background:#e0e7ff;border-radius:12px;font-size:12px;margin:2px}",
    "</style></head><body>",
    `<nav class="nav">${Array.from({ length: 8 }, (_, i) => `<a href="#nav-${i}">Section ${i + 1}</a>`).join("")}</nav>`,
    '<div class="article">',
    "<h1>Understanding Modern Browser Engine Architecture</h1>",
    '<div class="meta"><span class="author">Dr. Smith</span> | <time>2025-03-15</time> | <span>15 min read</span></div>',
    `<div class="tags">${Array.from({ length: 6 }, (_, i) => `<span class="tag">tag-${i + 1}</span>`).join("")}</div>`,
    "<h2>Introduction</h2>",
    ...paragraphs.slice(0, 5),
    "<h2>Core Concepts</h2>",
    ...paragraphs.slice(5, 12),
    '<blockquote>"Performance is not just about speed, it is about efficiency." - Anonymous</blockquote>',
    "<h2>Implementation Details</h2>",
    ...paragraphs.slice(12, 20),
    "<h3>Subsection A</h3>",
    ...paragraphs.slice(20, 25),
    "<h3>Subsection B</h3>",
    ...paragraphs.slice(25),
    "<h2>Comments</h2>",
    '<div class="comments">',
    ...comments,
    "</div></div>",
    '<div class="sidebar">',
    "<h3>Related Articles</h3>",
    `<ul>${sidebar.join("")}</ul>`,
    "<h3>Archives</h3>",
    `<ul>${Array.from({ length: 12 }, (_, i) => `<li><a href="#month-${i}">Month ${i + 1}, 2025</a></li>`).join("")}</ul>`,
    "</div>",
    "</body></html>",
  ].join("");
}

function generateDataTablePage(): string {
  const headerCells = [
    "ID", "Name", "Email", "Department", "Role", "Status", "Joined", "Last Active",
  ];
  const header = `<tr>${headerCells.map((h) => `<th>${h}</th>`).join("")}</tr>`;

  const rows = Array.from({ length: 200 }, (_, i) => {
    const dept = ["Engineering", "Design", "Marketing", "Sales", "Support"][i % 5];
    const role = ["Admin", "Manager", "Member", "Viewer"][i % 4];
    const status = ["Active", "Inactive", "Pending"][i % 3];
    return (
      `<tr data-row="${i}">` +
      `<td>${i + 1}</td>` +
      `<td><a href="#user-${i}">User ${i + 1}</a></td>` +
      `<td>user${i + 1}@example.com</td>` +
      `<td>${dept}</td>` +
      `<td><span class="badge badge-${role.toLowerCase()}">${role}</span></td>` +
      `<td><span class="status status-${status.toLowerCase()}">${status}</span></td>` +
      `<td>2024-${String((i % 12) + 1).padStart(2, "0")}-${String((i % 28) + 1).padStart(2, "0")}</td>` +
      `<td>${i % 3 === 0 ? "Today" : i % 3 === 1 ? "Yesterday" : "Last week"}</td>` +
      `</tr>`
    );
  });

  return [
    "<html><head><title>Benchmark Table</title>",
    "<style>",
    "body{font-family:system-ui;margin:20px}",
    "table{width:100%;border-collapse:collapse}",
    "th,td{padding:8px 12px;border:1px solid #ddd;text-align:left}",
    "th{background:#f5f5f5;font-weight:600;position:sticky;top:0}",
    "tr:nth-child(even){background:#fafafa}",
    ".badge{padding:2px 8px;border-radius:4px;font-size:12px}",
    ".toolbar{display:flex;gap:12px;margin-bottom:16px;align-items:center}",
    "input,select,button{padding:6px 12px;border:1px solid #ccc;border-radius:4px}",
    "</style></head><body>",
    "<h1>User Management Dashboard</h1>",
    '<div class="toolbar">',
    '<input id="search" type="text" placeholder="Search users...">',
    '<select id="dept-filter"><option value="">All Departments</option><option value="eng">Engineering</option><option value="des">Design</option></select>',
    '<select id="status-filter"><option value="">All Statuses</option><option value="active">Active</option><option value="inactive">Inactive</option></select>',
    '<button id="add-user">Add User</button>',
    '<span id="count">Showing 200 users</span>',
    "</div>",
    `<table><thead>${header}</thead><tbody>${rows.join("")}</tbody></table>`,
    '<div class="pagination">',
    ...Array.from({ length: 10 }, (_, i) => `<button class="page-btn" data-page="${i + 1}">${i + 1}</button>`),
    "</div>",
    "</body></html>",
  ].join("");
}

function generateNestedPage(): string {
  function nest(depth: number, breadth: number, prefix: string): string {
    if (depth === 0) {
      return `<span class="leaf" data-path="${prefix}">Leaf node at ${prefix}</span>`;
    }
    const children = Array.from(
      { length: breadth },
      (_, i) =>
        `<div class="node depth-${depth}" data-depth="${depth}" data-idx="${i}">` +
        `<div class="node-header"><strong>Section ${prefix}.${i + 1}</strong> <em>(depth ${depth})</em></div>` +
        `<div class="node-content">${nest(depth - 1, Math.max(2, breadth - 1), `${prefix}.${i + 1}`)}</div>` +
        `</div>`,
    );
    return children.join("");
  }

  return [
    "<html><head><title>Benchmark Nested</title>",
    "<style>",
    "body{font-family:system-ui;margin:20px}",
    ".node{border-left:2px solid #ddd;padding-left:16px;margin:4px 0}",
    ".node-header{padding:4px 0;cursor:pointer}",
    ".leaf{display:block;padding:2px 8px;background:#f0f9ff;margin:2px 0;border-radius:2px}",
    "</style></head><body>",
    "<h1>Deeply Nested Document Structure</h1>",
    nest(7, 3, "root"),
    "</body></html>",
  ].join("");
}

function generateDashboardPage(): string {
  const cards = Array.from(
    { length: 12 },
    (_, i) =>
      `<div class="card" data-card="${i}">` +
      `<div class="card-title">Metric ${i + 1}</div>` +
      `<div class="card-value">${Math.floor(Math.random() * 10000)}</div>` +
      `<div class="card-trend ${i % 2 === 0 ? "up" : "down"}">${i % 2 === 0 ? "+" : "-"}${(Math.random() * 20).toFixed(1)}%</div>` +
      `</div>`,
  );

  const chartBars = Array.from(
    { length: 24 },
    (_, i) => {
      const h = 20 + (i * 7 + 13) % 80;
      return `<div class="bar" style="height:${h}%" data-hour="${i}"><span class="bar-label">${String(i).padStart(2, "0")}:00</span></div>`;
    },
  );

  const logRows = Array.from(
    { length: 100 },
    (_, i) => {
      const level = ["INFO", "WARN", "ERROR", "DEBUG"][i % 4];
      return (
        `<tr class="log-${level.toLowerCase()}" data-log="${i}">` +
        `<td>${new Date(2025, 0, 1, i % 24, i % 60).toISOString()}</td>` +
        `<td><span class="level level-${level.toLowerCase()}">${level}</span></td>` +
        `<td>Service ${["auth", "api", "worker", "cache", "db"][i % 5]}</td>` +
        `<td>Log message number ${i + 1}: operation completed in ${(Math.random() * 1000).toFixed(0)}ms</td>` +
        `</tr>`
      );
    },
  );

  return [
    "<html><head><title>Benchmark Dashboard</title>",
    "<style>",
    "body{font-family:system-ui;margin:0;background:#f5f5f5}",
    ".header{background:#1a1a2e;color:white;padding:12px 24px;display:flex;justify-content:space-between;align-items:center}",
    ".grid{display:grid;grid-template-columns:repeat(4,1fr);gap:16px;padding:24px}",
    ".card{background:white;padding:20px;border-radius:8px;box-shadow:0 1px 3px rgba(0,0,0,.1)}",
    ".card-value{font-size:28px;font-weight:700;margin:8px 0}",
    ".card-trend.up{color:#16a34a}.card-trend.down{color:#dc2626}",
    ".chart-area{background:white;margin:0 24px;padding:20px;border-radius:8px;box-shadow:0 1px 3px rgba(0,0,0,.1)}",
    ".bars{display:flex;align-items:flex-end;gap:4px;height:200px}",
    ".bar{background:#3b82f6;flex:1;border-radius:2px 2px 0 0;position:relative;min-width:8px}",
    ".log-table{margin:24px;background:white;border-radius:8px;box-shadow:0 1px 3px rgba(0,0,0,.1);overflow:hidden}",
    "table{width:100%;border-collapse:collapse;font-size:13px}",
    "th,td{padding:6px 12px;border-bottom:1px solid #eee;text-align:left}",
    "th{background:#f9fafb;font-weight:600}",
    ".tabs{display:flex;gap:0;margin:24px 24px 0}",
    ".tab{padding:8px 20px;background:#e5e7eb;cursor:pointer;border-radius:6px 6px 0 0}",
    ".tab.active{background:white}",
    "</style></head><body>",
    '<div class="header"><h1>Operations Dashboard</h1><div><input id="dash-search" placeholder="Search..." type="text"><button id="refresh">Refresh</button></div></div>',
    `<div class="grid">${cards.join("")}</div>`,
    '<div class="tabs"><div class="tab active">Hourly</div><div class="tab">Daily</div><div class="tab">Weekly</div></div>',
    `<div class="chart-area"><h3>Request Volume</h3><div class="bars">${chartBars.join("")}</div></div>`,
    '<div class="log-table">',
    "<h3 style='padding:16px 12px 0'>Recent Logs</h3>",
    `<table><thead><tr><th>Timestamp</th><th>Level</th><th>Service</th><th>Message</th></tr></thead><tbody>${logRows.join("")}</tbody></table>`,
    "</div>",
    "</body></html>",
  ].join("");
}

// ---------------------------------------------------------------------------
// Pre-build HTML strings and injection commands
// ---------------------------------------------------------------------------

const ARTICLE_HTML = generateArticlePage();
const TABLE_HTML = generateDataTablePage();
const NESTED_HTML = generateNestedPage();
const DASHBOARD_HTML = generateDashboardPage();

function injectCmd(id: string, html: string): BenchmarkCommand {
  return {
    id,
    action: "evaluate",
    script: `document.open(); document.write(${JSON.stringify(html)}); document.close(); 'ok'`,
  };
}

function setupPage(html: string, tag: string): BenchmarkCommand[] {
  return [
    { id: `${tag}-nav`, action: "navigate", url: "about:blank", waitUntil: "domcontentloaded" },
    injectCmd(`${tag}-inject`, html),
  ];
}

// ---------------------------------------------------------------------------
// Engine-specific scenarios: complex pages that stress real-world workloads
// ---------------------------------------------------------------------------

export const engineScenarios: Scenario[] = [
  {
    name: "article-snapshot",
    description: "Snapshot a realistic article page (~800 DOM nodes, 30 paragraphs, 40 comments)",
    setup: setupPage(ARTICLE_HTML, "art"),
    commands: [{ id: "snap", action: "snapshot" }],
  },
  {
    name: "table-snapshot",
    description: "Snapshot a data table with 200 rows and 8 columns",
    setup: setupPage(TABLE_HTML, "tbl"),
    commands: [{ id: "snap", action: "snapshot" }],
  },
  {
    name: "nested-snapshot",
    description: "Snapshot a deeply nested DOM tree (7 levels, ~3000 nodes)",
    setup: setupPage(NESTED_HTML, "nest"),
    commands: [{ id: "snap", action: "snapshot" }],
  },
  {
    name: "dashboard-snap",
    description: "Snapshot an operations dashboard with cards, chart, and 100 log rows",
    setup: setupPage(DASHBOARD_HTML, "dash"),
    commands: [{ id: "snap", action: "snapshot" }],
  },
  {
    name: "article-inject",
    description: "Write a full article page into the DOM (measures parse + layout)",
    setup: [
      { id: "ai-nav", action: "navigate", url: "about:blank", waitUntil: "domcontentloaded" },
    ],
    commands: [injectCmd("ai-write", ARTICLE_HTML)],
  },
  {
    name: "table-query",
    description: "Evaluate a querySelectorAll across a large table",
    setup: setupPage(TABLE_HTML, "tq"),
    commands: [
      {
        id: "query",
        action: "evaluate",
        script: "document.querySelectorAll('tr[data-row]').length + ' rows, ' + document.querySelectorAll('td').length + ' cells'",
      },
    ],
  },
  {
    name: "dashboard-workflow",
    description: "Full agent workflow on complex dashboard: snapshot, click, fill, eval, screenshot",
    setup: setupPage(DASHBOARD_HTML, "dw"),
    commands: [
      { id: "dw-snap", action: "snapshot" },
      { id: "dw-fill", action: "fill", selector: "#dash-search", value: "error logs" },
      { id: "dw-click", action: "click", selector: "#refresh" },
      { id: "dw-eval", action: "evaluate", script: "document.querySelectorAll('.card').length + ' cards'" },
      { id: "dw-ss", action: "screenshot" },
    ],
  },
  {
    name: "nested-eval",
    description: "Recursive DOM traversal via evaluate on deeply nested tree",
    setup: setupPage(NESTED_HTML, "ne"),
    commands: [
      {
        id: "walk",
        action: "evaluate",
        script: "(function(){let c=0;const w=n=>{c++;for(const ch of n.children)w(ch);};w(document.body);return c+' nodes';})()",
      },
    ],
  },
];
