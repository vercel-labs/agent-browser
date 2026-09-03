import { existsSync } from "node:fs";
import process from "node:process";
import { runAgentBrowserCommand, withAgentBrowserSandbox } from "@agent-browser/sandbox/vercel";

if (existsSync(".env.local") && typeof process.loadEnvFile === "function") {
  process.loadEnvFile(".env.local");
}

const url = process.argv[2] ?? "https://example.com";

const snapshot = await withAgentBrowserSandbox(async (sandbox) => {
  await runAgentBrowserCommand(sandbox, ["open", url]);
  const result = await runAgentBrowserCommand(sandbox, ["snapshot", "-i", "-c"], {
    json: false,
  });
  await runAgentBrowserCommand(sandbox, ["close"], { json: false });
  return result.stdout;
});

console.log(snapshot);
