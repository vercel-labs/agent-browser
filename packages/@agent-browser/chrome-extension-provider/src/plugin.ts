#!/usr/bin/env node
import { existsSync, realpathSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { requestScope, stateDirectory } from "./config.js";
import { ensureDaemon, readLiveDaemon, relayRequest } from "./launcher.js";
import { PLUGIN_NAME, PLUGIN_PROTOCOL, type LeaseResult, type ScopeStatus, type SetupResult } from "./protocol.js";

type Request = { protocol: string; type: string; request?: Record<string, unknown> };
const success = (fields: Record<string, unknown>) => ({ protocol: PLUGIN_PROTOCOL, success: true, ...fields });
const failure = (error: string) => ({ protocol: PLUGIN_PROTOCOL, success: false, error });

/** One request, one JSON response, then exit; browser actions stay in the native CLI. */
export async function handlePluginRequest(input: Request) {
  if (input.protocol !== PLUGIN_PROTOCOL) return failure("Unsupported plugin protocol");
  if (input.type === "plugin.manifest") return success({ manifest: {
    name: PLUGIN_NAME,
    capabilities: ["browser.provider", "command.run"],
    description: "Control a Chrome tab explicitly authorized in the optional extension",
  } });
  const request = input.request ?? {};
  const directory = stateDirectory();
  if (input.type === "chrome-extension.setup") {
    const extensionPath = packagedExtensionPath();
    const record = await ensureDaemon(directory);
    const scope = requestScope(request);
    const setup = await relayRequest<SetupResult>(record, "/setup", { method: "POST", body: JSON.stringify(scope) });
    return success({ data: { ...setup, scope, extensionPath,
      instructions: ["Open chrome://extensions and enable Developer mode.",
        "Choose Load unpacked and select extensionPath.",
        "Open the Agent Browser extension, paste pairingCode, and select the exact tab to authorize.",
        "Run your normal agent-browser commands with --provider chrome-extension and the same session and namespace."],
    } });
  }
  if (input.type === "chrome-extension.status") {
    const scope = requestScope(request);
    const record = await readLiveDaemon(directory);
    const status = record ? await relayRequest<ScopeStatus>(record,
      `/status?${new URLSearchParams(scope).toString()}`) : { state: "offline", scope };
    return success({ data: { ...status, extensionInstalled: existsSync(new URL("../.output/chrome-mv3/manifest.json", import.meta.url)),
      nextStep: status.state === "connected" ? "Use the CLI, or stop control in the extension."
        : status.state === "authorized" ? "Run a browser command with the same provider, session and namespace."
        : "Run chrome-extension.setup with an explicit session payload, then authorize a tab in the extension.",
    } });
  }
  if (input.type === "browser.launch") {
    const record = await readLiveDaemon(directory);
    if (!record) return failure("Run chrome-extension.setup and authorize a tab first");
    const scope = requestScope(request);
    const lease = await relayRequest<LeaseResult>(record, "/lease", { method: "POST", body: JSON.stringify(scope) });
    return success({ browser: { cdpUrl: lease.cdpUrl, directPage: false, existingBrowser: true,
      cleanup: { instanceId: record.instanceId, leaseId: lease.leaseId },
    } });
  }
  if (input.type === "browser.close") {
    const record = await readLiveDaemon(directory);
    if (record && request.instanceId === record.instanceId && typeof request.leaseId === "string") {
      await relayRequest(record, `/lease/${encodeURIComponent(request.leaseId)}`, { method: "DELETE" });
    }
    return success({ data: { released: true } });
  }
  return failure("Unsupported plugin request type");
}

function packagedExtensionPath(): string {
  const path = fileURLToPath(new URL("../.output/chrome-mv3", import.meta.url));
  if (!existsSync(new URL("../.output/chrome-mv3/manifest.json", import.meta.url))) {
    throw new Error("Built extension assets are missing; install the complete provider package or run pnpm build");
  }
  return path;
}

async function main() {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of process.stdin) {
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    size += bytes.length;
    if (size > 65_536) throw new Error("Plugin request is too large");
    chunks.push(bytes);
  }
  let value: unknown;
  try { value = JSON.parse(Buffer.concat(chunks).toString("utf8")); }
  catch { throw new Error("Invalid plugin request JSON"); }
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid plugin request");
  process.stdout.write(JSON.stringify(await handlePluginRequest(value as Request)));
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  main().catch(error => process.stdout.write(JSON.stringify(failure(error instanceof Error ? error.message : "Plugin failed"))));
}
