import { AGENT_BROWSER_SANDBOX_VERSION, quoteShellArg } from "@agent-browser/sandbox";
import { installAgentBrowser, type EveSandboxSession } from "./sandbox";

import extension from "../extension";

/** Structural subset of eve's tool context — every eve `ctx` satisfies it. */
export interface BrowserToolContext {
  readonly abortSignal?: AbortSignal;
  getSandbox(): PromiseLike<EveSandboxSession | null | undefined>;
}

export const SELECTOR_HINT =
  'Element selector: a ref from the snapshot tool like "@e12" (most reliable), a CSS selector like "#login .submit", "text=Sign in", or "xpath=//button[1]".';

/**
 * Upper bound on acquiring the sandbox session and on a single command run.
 * The sandbox layer can occasionally wedge while (re)opening a session; an
 * unbounded await leaves the whole turn stuck on a running tool forever.
 * Generous enough for a cold template-backed session create.
 */
const SANDBOX_DEADLINE_MS = 180_000;

async function withDeadline<T>(work: PromiseLike<T>, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      work,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => {
          reject(
            new Error(
              `${label} did not respond within ${SANDBOX_DEADLINE_MS / 1000}s. The browser sandbox may be starting up or unavailable — try again shortly.`,
            ),
          );
        }, SANDBOX_DEADLINE_MS);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

interface McpCallResult {
  readonly _meta?: { readonly agentBrowserVersion?: string };
  readonly content?: readonly { readonly text?: string; readonly type: string }[];
  readonly error?: { readonly message: string };
  readonly isError?: boolean;
  readonly structuredContent?: { readonly response?: { readonly data?: unknown } | null };
}

/**
 * Run one agent-browser MCP tool in the eve sandbox with `agent-browser mcp
 * call` and return the `data` payload of its CLI response. Each call is its
 * own process, so nothing outlives the tool call; browser state lives in the
 * session daemon. Throws when the sandbox is unavailable, the installed CLI
 * does not match this package, or the tool reports an error.
 */
export async function runMcpTool<TData = unknown>(
  ctx: BrowserToolContext,
  tool: string,
  args: Record<string, unknown>,
): Promise<TData> {
  const sandbox = await requireSandbox(ctx);
  await ensureInstalled(sandbox, ctx.abortSignal);

  const config = extension.config;
  const command = [
    ...configEnv(),
    quoteShellArg(config.binary),
    "mcp",
    "call",
    quoteShellArg(tool),
    quoteShellArg(JSON.stringify(args)),
  ].join(" ");
  const raw = await withDeadline(
    sandbox.run({ abortSignal: ctx.abortSignal, command }),
    "The browser command",
  );

  let result: McpCallResult;
  try {
    result = JSON.parse((raw.stdout ?? "").trim().split("\n").at(-1) ?? "") as McpCallResult;
  } catch {
    throw new Error(
      `agent-browser in the sandbox does not support \`mcp call\`. Install agent-browser@${AGENT_BROWSER_SANDBOX_VERSION} or set installSpec to it.\n${
        (raw.stderr ?? "").trim() || (raw.stdout ?? "").trim()
      }`,
    );
  }
  assertMatchingVersion(result._meta?.agentBrowserVersion);
  if (result.error !== undefined) {
    throw new Error(`agent-browser ${tool} failed: ${result.error.message}`);
  }
  if (result.isError === true) {
    const text = result.content?.find((part) => part.type === "text")?.text ?? "unknown error";
    throw new Error(`agent-browser ${tool} failed: ${text}`);
  }
  return (result.structuredContent?.response?.data ?? null) as TData;
}

/** The catalog this package ships describes one agent-browser minor; another minor may take different arguments. */
function assertMatchingVersion(installed: string | undefined): void {
  if (installed !== undefined && !matchesPackageMinor(installed)) {
    throw versionMismatchError(installed);
  }
}

function versionMismatchError(installed: string): Error {
  return new Error(
    `agent-browser ${installed} in the sandbox does not match @agent-browser/eve ${AGENT_BROWSER_SANDBOX_VERSION}. Install agent-browser@${AGENT_BROWSER_SANDBOX_VERSION} or set installSpec to it.`,
  );
}

function matchesPackageMinor(version: string): boolean {
  const minor = (v: string) => v.split(".").slice(0, 2).join(".");
  return minor(version) === minor(AGENT_BROWSER_SANDBOX_VERSION);
}

/** Server-level settings that MCP tool arguments cannot carry. */
function configEnv(): string[] {
  const config = extension.config;
  const env: string[] = [];
  if (config.proxy !== undefined) env.push(`AGENT_BROWSER_PROXY=${quoteShellArg(config.proxy)}`);
  if (config.contentBoundaries) env.push("AGENT_BROWSER_CONTENT_BOUNDARIES=1");
  if (config.maxOutputChars !== undefined) {
    env.push(`AGENT_BROWSER_MAX_OUTPUT=${config.maxOutputChars}`);
  }
  return env;
}

async function requireSandbox(ctx: BrowserToolContext): Promise<EveSandboxSession> {
  const sandbox = await withDeadline(ctx.getSandbox(), "The sandbox session");
  if (sandbox === null || sandbox === undefined) {
    throw new Error(
      "The browser tools require an eve sandbox. Configure agent/sandbox.ts in the consuming agent.",
    );
  }
  return sandbox;
}

async function ensureInstalled(sandbox: EveSandboxSession, abortSignal?: AbortSignal): Promise<void> {
  // eve sandbox sessions have no stable identity to cache by, so probe on every call.
  if (extension.config.autoInstall) {
    await installIfMissingOrStale(sandbox, abortSignal);
  }
}

async function installIfMissingOrStale(sandbox: EveSandboxSession, abortSignal?: AbortSignal): Promise<void> {
  const config = extension.config;
  const installed = await probeVersion(sandbox, abortSignal);
  if (installed !== undefined && matchesPackageMinor(installed)) {
    return;
  }
  // Reinstalling a custom spec or binary would repeat on every call, so report the mismatch instead.
  const userOwned = config.installSpec !== undefined || config.binary !== "agent-browser";
  if (installed !== undefined && userOwned) {
    throw versionMismatchError(installed);
  }
  await withDeadline(
    installAgentBrowser(sandbox, {
      abortSignal,
      installBrowser: config.installBrowser,
      installSpec: config.installSpec,
      installSystemDependencies: config.installSystemDependencies,
    }),
    "Installing agent-browser",
  );
  const reinstalled = await probeVersion(sandbox, abortSignal);
  if (reinstalled === undefined || !matchesPackageMinor(reinstalled)) {
    throw versionMismatchError(reinstalled ?? "missing");
  }
}

/** The installed CLI version, or undefined when the binary is missing. */
async function probeVersion(
  sandbox: EveSandboxSession,
  abortSignal?: AbortSignal,
): Promise<string | undefined> {
  const probe = await withDeadline(
    sandbox.run({
      abortSignal,
      command: `${quoteShellArg(extension.config.binary)} --version 2>/dev/null`,
    }),
    "The browser install probe",
  );
  if ((probe.exitCode ?? 0) !== 0) {
    return undefined;
  }
  return (probe.stdout ?? "").trim().split(/\s+/).at(-1) ?? "";
}
