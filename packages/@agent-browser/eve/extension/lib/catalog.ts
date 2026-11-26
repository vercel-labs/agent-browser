import { AGENT_BROWSER_SANDBOX_VERSION, defaultSessionName } from "@agent-browser/sandbox";
import catalog from "@agent-browser/sandbox/mcp-tools.json" with { type: "json" };
import { defineTool } from "eve/tools";

import extension from "../extension";
import { runMcpTool, type BrowserToolContext } from "./browser";

const PREFIX = "agent_browser_";

/**
 * Catalog arguments the model never sees: the ones the extension sets from its
 * config, and `extraArgs`, the raw CLI flag escape hatch, which repeats a long
 * description on every tool.
 */
const APP_OWNED_ARGUMENTS = new Set([
  "allowedDomains",
  "caCert",
  "clearCaCert",
  "extraArgs",
  "idleTimeout",
  "namespace",
  "restore",
  "restoreCheckFn",
  "restoreCheckText",
  "restoreCheckUrl",
  "restoreSave",
  "session",
  "useSystemCa",
]);

type JsonValue = string | number | boolean | null | readonly JsonValue[] | JsonObject;
interface JsonObject {
  readonly [key: string]: JsonValue;
}

interface CatalogTool {
  readonly description: string;
  readonly inputSchema: JsonObject & {
    readonly properties?: JsonObject;
    readonly required?: readonly string[];
  };
  readonly name: string;
}

const TOOLS = new Map(
  (catalog.tools as unknown as readonly CatalogTool[]).map((tool) => [tool.name.slice(PREFIX.length), tool]),
);

export interface CatalogToolOptions<TOutput> {
  /** Catalog arguments of this tool the model may not set, on top of the shared app-owned ones. */
  readonly hiddenArguments?: readonly string[];
  /** Keep the tool out of the model's tool list; the model finds it with `eve__search`. */
  readonly deferred?: boolean;
  /** Retry once after a transient CDP race, such as a snapshot taken mid-navigation. */
  readonly retryOnCdpError?: boolean;
  /** Shapes the `data` payload of the CLI response into the tool output. */
  readonly output?: (data: unknown, ctx: BrowserToolContext) => Promise<TOutput> | TOutput;
  /** What the model sees instead of the full output. */
  readonly toModelOutput?: (output: TOutput) => { type: "json"; value: unknown };
}

/** An eve tool for one agent-browser MCP tool, with its schema and description from the catalog. */
export function catalogTool<TOutput = unknown>(name: string, options: CatalogToolOptions<TOutput> = {}) {
  const tool = TOOLS.get(name);
  if (tool === undefined) {
    throw new Error(`agent-browser ${AGENT_BROWSER_SANDBOX_VERSION} has no MCP tool named ${PREFIX}${name}.`);
  }
  const hidden = new Set([...APP_OWNED_ARGUMENTS, ...(options.hiddenArguments ?? [])]);
  return defineTool({
    description: tool.description,
    inputSchema: modelSchema(tool.inputSchema, hidden),
    async execute(input: Record<string, unknown>, ctx) {
      const modelArguments = Object.fromEntries(
        Object.entries(input).filter(([key]) => !hidden.has(key)),
      );
      const args = { ...modelArguments, ...appArguments() };
      let data: unknown;
      try {
        data = await runMcpTool(ctx, tool.name, args);
      } catch (error) {
        if (!options.retryOnCdpError || !(error instanceof Error) || !error.message.includes("CDP error")) {
          throw error;
        }
        await new Promise((resolve) => setTimeout(resolve, 500));
        data = await runMcpTool(ctx, tool.name, args);
      }
      return (options.output ? await options.output(data, ctx) : data) as TOutput;
    },
    ...(options.deferred ? { deferred: true } : {}),
    ...(options.toModelOutput ? { toModelOutput: options.toModelOutput } : {}),
  });
}

function modelSchema(schema: CatalogTool["inputSchema"], hidden: ReadonlySet<string>): JsonObject {
  const properties = Object.fromEntries(
    Object.entries(schema.properties ?? {}).filter(([key]) => !hidden.has(key)),
  );
  const required = (schema.required ?? []).filter((key) => !hidden.has(key));
  return { ...schema, properties, required };
}

function appArguments(): Record<string, unknown> {
  const config = extension.config;
  if (config.caCert !== undefined && config.clearCaCert) {
    throw new Error("Cannot use caCert with clearCaCert");
  }
  const args: Record<string, unknown> = {
    session: config.session ?? defaultSessionName(config.sessionPrefix, "default"),
  };
  if (config.allowedDomains !== undefined && config.allowedDomains.length > 0) {
    args.allowedDomains = config.allowedDomains;
  }
  if (config.caCert !== undefined) args.caCert = config.caCert;
  if (config.clearCaCert) args.clearCaCert = true;
  if (config.useSystemCa !== undefined) args.useSystemCa = config.useSystemCa;
  return args;
}
