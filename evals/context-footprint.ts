import { existsSync, mkdirSync, readFileSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";

interface Metric {
  name: string;
  bytes: number;
  chars: number;
  approxTokens: number;
  notes: string;
}

interface McpPage {
  cursor?: string;
  nextCursor?: string;
  toolCount: number;
  payload: unknown;
}

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "..");
const localBinary = resolve(repoRoot, "cli/target/debug/agent-browser");
const agentBrowserBin = process.env.AGENT_BROWSER_BIN ||
  (existsSync(localBinary) ? localBinary : "agent-browser");
const outputPath = resolve(__dirname, "results/context-footprint.json");

function approxTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

function metric(name: string, text: string, notes: string): Metric {
  return {
    name,
    bytes: new TextEncoder().encode(text).length,
    chars: text.length,
    approxTokens: approxTokens(text),
    notes,
  };
}

async function runAgentBrowser(args: string[]): Promise<string> {
  const proc = Bun.spawn([agentBrowserBin, ...args], {
    stdout: "pipe",
    stderr: "pipe",
    env: { ...(process.env as Record<string, string>), NO_COLOR: "1" },
  });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);

  if (exitCode !== 0) {
    throw new Error(
      `${agentBrowserBin} ${args.join(" ")} failed with ${exitCode}: ${stderr}`,
    );
  }

  return stdout.trimEnd();
}

async function mcpRequest(
  method: string,
  params: Record<string, unknown> = {},
  mcpArgs: string[] = [],
) {
  const proc = Bun.spawn([agentBrowserBin, "mcp", ...mcpArgs], {
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
    env: { ...(process.env as Record<string, string>), NO_COLOR: "1" },
  });

  proc.stdin.write(
    JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) + "\n",
  );
  proc.stdin.end();

  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);

  if (exitCode !== 0) {
    throw new Error(`MCP ${method} failed with ${exitCode}: ${stderr}`);
  }

  const response = JSON.parse(stdout.trim());
  if (response.error) {
    throw new Error(`MCP ${method} error: ${JSON.stringify(response.error)}`);
  }

  return response.result;
}

async function listAllMcpTools(mcpArgs: string[] = []): Promise<McpPage[]> {
  const pages: McpPage[] = [];
  let cursor: string | undefined;

  for (;;) {
    const result = await mcpRequest(
      "tools/list",
      cursor ? { cursor } : {},
      mcpArgs,
    );
    const tools = Array.isArray(result.tools) ? result.tools : [];
    pages.push({
      cursor,
      nextCursor: result.nextCursor,
      toolCount: tools.length,
      payload: result,
    });

    if (!result.nextCursor) break;
    cursor = result.nextCursor;
  }

  return pages;
}

function printTable(metrics: Metric[]): void {
  console.log("\nContext Footprint Eval");
  console.log("=".repeat(78));
  console.log(`agent-browser: ${agentBrowserBin}`);
  console.log("");
  console.log(
    `${"Surface".padEnd(34)} ${"Bytes".padStart(10)} ${"Approx tokens".padStart(14)}  Notes`,
  );
  console.log("-".repeat(78));
  for (const item of metrics) {
    console.log(
      `${item.name.padEnd(34)} ${String(item.bytes).padStart(10)} ${String(item.approxTokens).padStart(14)}  ${item.notes}`,
    );
  }
}

async function main(): Promise<void> {
  const thinSkill = readFileSync(
    resolve(repoRoot, "skills/agent-browser/SKILL.md"),
    "utf-8",
  );
  const skillsList = await runAgentBrowser(["skills", "list"]);
  const coreGuide = await runAgentBrowser(["skills", "get", "core"]);
  const coreFull = await runAgentBrowser(["skills", "get", "core", "--full"]);

  const initialize = await mcpRequest("initialize", {});
  const mcpDefaultPages = await listAllMcpTools();
  const mcpAllPages = await listAllMcpTools(["--tools", "all"]);
  const mcpDefaultTools = {
    initialize,
    pages: mcpDefaultPages.map((page) => page.payload),
  };
  const mcpAllTools = {
    initialize,
    pages: mcpAllPages.map((page) => page.payload),
  };

  const cliThinText = [
    "<installed-skill>",
    thinSkill,
    "</installed-skill>",
  ].join("\n");
  const cliRecommendedText = [
    cliThinText,
    "$ agent-browser skills list",
    skillsList,
    "$ agent-browser skills get core",
    coreGuide,
  ].join("\n\n");
  const cliFullText = [
    cliThinText,
    "$ agent-browser skills list",
    skillsList,
    "$ agent-browser skills get core --full",
    coreFull,
  ].join("\n\n");
  const mcpInitializeText = JSON.stringify(initialize, null, 2);
  const mcpDefaultText = JSON.stringify(mcpDefaultTools, null, 2);
  const mcpAllToolsText = JSON.stringify(mcpAllTools, null, 2);

  const metrics = [
    metric("CLI thin skill", cliThinText, "Installed discovery stub only"),
    metric("CLI skills list", skillsList, "Live command for available skills"),
    metric(
      "CLI recommended context",
      cliRecommendedText,
      "Thin skill plus skills list plus core guide",
    ),
    metric(
      "CLI full command context",
      cliFullText,
      "Thin skill plus skills list plus core --full",
    ),
    metric("MCP initialize", mcpInitializeText, "Protocol and server metadata"),
    metric(
      "MCP default core profile",
      mcpDefaultText,
      "Initialize plus default core tools/list response",
    ),
    metric(
      "MCP all tools profile",
      mcpAllToolsText,
      `Initialize plus ${mcpAllPages.length} tools/list page(s) from --tools all`,
    ),
  ];

  const defaultToolCount = mcpDefaultPages.reduce(
    (sum, page) => sum + page.toolCount,
    0,
  );
  const allToolCount = mcpAllPages.reduce((sum, page) => sum + page.toolCount, 0);
  const pass = defaultToolCount < allToolCount &&
    mcpAllPages.length > 1 &&
    cliRecommendedText.includes("agent-browser skills get core") &&
    cliFullText.includes("agent-browser skills get core --full");

  const report = {
    pass,
    generatedAt: new Date().toISOString(),
    agentBrowserBin,
    mcp: {
      protocolVersion: initialize.protocolVersion,
      defaultPages: mcpDefaultPages.length,
      defaultToolCount,
      allPages: mcpAllPages.length,
      allToolCount,
    },
    metrics,
    ratios: {
      mcpDefaultToCliRecommended:
        metric("", mcpDefaultText, "").approxTokens /
        metric("", cliRecommendedText, "").approxTokens,
      mcpAllPagesToCliFull:
        metric("", mcpAllToolsText, "").approxTokens /
        metric("", cliFullText, "").approxTokens,
    },
  };

  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, JSON.stringify(report, null, 2) + "\n", "utf-8");

  printTable(metrics);
  console.log("-".repeat(78));
  console.log(
    `MCP default profile: ${mcpDefaultPages.length} page(s), ${defaultToolCount} tool(s)`,
  );
  console.log(
    `MCP all profile: ${mcpAllPages.length} page(s), ${allToolCount} tool(s)`,
  );
  console.log(
    `MCP default / CLI recommended tokens: ${report.ratios.mcpDefaultToCliRecommended.toFixed(2)}x`,
  );
  console.log(
    `MCP all pages / CLI full command tokens: ${report.ratios.mcpAllPagesToCliFull.toFixed(2)}x`,
  );
  console.log(`Report: ${outputPath}`);
  console.log(pass ? "PASS" : "FAIL");

  process.exit(pass ? 0 : 1);
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : String(err));
  process.exit(1);
});
