import {
  AgentBrowserCommandError,
  createAgentBrowserCommandResult,
  defaultSessionName,
  quoteShellArg,
} from "@agent-browser/sandbox";
import {
  buildAgentBrowserCommand,
  installAgentBrowser,
  type EveSandboxSession,
} from "@agent-browser/sandbox/eve";

import extension from "../extension";

/** Structural subset of eve's tool context — every eve `ctx` satisfies it. */
export interface BrowserToolContext {
  readonly abortSignal?: AbortSignal;
  getSandbox(): PromiseLike<EveSandboxSession | null | undefined>;
}

interface CommandEnvelope<TData> {
  readonly data: TData | null;
  readonly error: string | null;
  readonly success: boolean;
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

const pendingInstalls = new Map<string, Promise<void>>();

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

/**
 * Run an agent-browser command in the eve sandbox and return the parsed
 * `data` payload of its `--json` envelope. Throws when the sandbox is
 * unavailable, the command exits non-zero, or the envelope reports failure.
 */
export async function runBrowser<TData = unknown>(
  ctx: BrowserToolContext,
  args: readonly string[],
): Promise<TData> {
  const sandbox = await requireSandbox(ctx);
  await ensureInstalled(sandbox, ctx.abortSignal);

  const config = extension.config;
  const command = buildAgentBrowserCommand([...args, ...configArgs()], {
    binary: config.binary,
    session: config.session ?? defaultSessionName(config.sessionPrefix, sandbox.id),
  });
  const raw = await withDeadline(
    sandbox.run({ abortSignal: ctx.abortSignal, command }),
    "The browser command",
  );
  const result = createAgentBrowserCommandResult<CommandEnvelope<TData>>({
    command,
    exitCode: raw.exitCode,
    stderr: raw.stderr,
    stdout: raw.stdout,
  });

  const envelope = result.json;
  if (envelope !== null && envelope.success === false) {
    throw new Error(`agent-browser ${args[0] ?? ""} failed: ${envelope.error ?? "unknown error"}`);
  }
  if (result.exitCode !== 0) {
    throw new AgentBrowserCommandError(result);
  }
  return (envelope?.data ?? null) as TData;
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
  if (!extension.config.autoInstall) {
    return;
  }
  let pending = pendingInstalls.get(sandbox.id);
  if (pending === undefined) {
    pending = installIfMissing(sandbox, abortSignal);
    pendingInstalls.set(sandbox.id, pending);
    // Let the next tool call retry a failed install instead of replaying the rejection.
    pending.catch(() => pendingInstalls.delete(sandbox.id));
  }
  await pending;
}

async function installIfMissing(sandbox: EveSandboxSession, abortSignal?: AbortSignal): Promise<void> {
  const config = extension.config;
  const probe = await sandbox.run({
    abortSignal,
    command: `command -v ${quoteShellArg(config.binary)} >/dev/null 2>&1`,
  });
  if ((probe.exitCode ?? 0) === 0) {
    return;
  }
  await installAgentBrowser(sandbox, {
    abortSignal,
    installBrowser: config.installBrowser,
    installSpec: config.installSpec,
    installSystemDependencies: config.installSystemDependencies,
  });
}

function configArgs(): string[] {
  const config = extension.config;
  const args: string[] = [];
  if (config.allowedDomains !== undefined && config.allowedDomains.length > 0) {
    args.push("--allowed-domains", config.allowedDomains.join(","));
  }
  if (config.contentBoundaries) {
    args.push("--content-boundaries");
  }
  if (config.maxOutputChars !== undefined) {
    args.push("--max-output", String(config.maxOutputChars));
  }
  if (config.proxy !== undefined) {
    args.push("--proxy", config.proxy);
  }
  return args;
}
