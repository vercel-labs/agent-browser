import {
  buildAgentBrowserArgv,
  createAgentBrowserCommandResult,
  quoteShellArg,
  resolveAgentBrowserInstallSpec,
  throwIfCommandFailed,
  type AgentBrowserArgs,
  type AgentBrowserCommandResult,
  type AgentBrowserInstallOptions,
  type BuildAgentBrowserArgvOptions,
} from "./shared.js";

export {
  AgentBrowserCommandError,
  DEFAULT_AGENT_BROWSER_INSTALL_SPEC,
  buildAgentBrowserArgv,
  resolveAgentBrowserInstallSpec,
  type AgentBrowserCommandResult,
  type AgentBrowserInstallOptions,
} from "./index.js";

export const CHROMIUM_SYSTEM_DEPS = [
  "nss",
  "nspr",
  "libxkbcommon",
  "atk",
  "at-spi2-atk",
  "at-spi2-core",
  "libXcomposite",
  "libXdamage",
  "libXrandr",
  "libXfixes",
  "libXcursor",
  "libXi",
  "libXtst",
  "libXScrnSaver",
  "libXext",
  "mesa-libgbm",
  "libdrm",
  "mesa-libGL",
  "mesa-libEGL",
  "cups-libs",
  "alsa-lib",
  "pango",
  "cairo",
  "gtk3",
  "dbus-libs",
] as const;

const SAFE_DNF_PACKAGE_NAME = /^[A-Za-z0-9._+][A-Za-z0-9._+-]*$/;

export interface VercelSandboxCommand {
  readonly exitCode?: number;
  stderr(): Promise<string>;
  stdout(): Promise<string>;
}

export interface VercelSandboxSession {
  runCommand(command: string, args: readonly string[]): Promise<VercelSandboxCommand>;
  snapshot(): Promise<{ readonly snapshotId: string }>;
  stop(): Promise<void>;
}

export interface VercelSandboxConstructor {
  create(options: Record<string, unknown>): Promise<VercelSandboxSession>;
}

export interface SandboxStepEvent {
  readonly elapsed?: number;
  readonly status: "done" | "error" | "running";
  readonly step: string;
}

export type SandboxStepHandler = (event: SandboxStepEvent) => void;

export interface VercelSandboxCredentials {
  readonly projectId: string;
  readonly teamId: string;
  readonly token: string;
}

export interface VercelInstallAgentBrowserOptions extends AgentBrowserInstallOptions {
  readonly onStep?: SandboxStepHandler;
  readonly systemDependencies?: readonly string[];
}

export interface CreateAgentBrowserSandboxOptions {
  readonly Sandbox?: VercelSandboxConstructor;
  readonly bootstrap?: boolean;
  readonly createOptions?: Record<string, unknown>;
  readonly env?: Readonly<Record<string, string | undefined>>;
  readonly install?: VercelInstallAgentBrowserOptions;
  readonly onStep?: SandboxStepHandler;
  readonly runtime?: string;
  readonly snapshotId?: null | string;
  readonly timeout?: number;
}

export interface RunAgentBrowserCommandOptions extends BuildAgentBrowserArgvOptions {
  readonly onStep?: SandboxStepHandler;
  readonly stepLabel?: string;
}

export interface WithAgentBrowserSandboxOptions extends CreateAgentBrowserSandboxOptions {
  readonly stop?: boolean;
}

export function getSandboxCredentials(
  env: Readonly<Record<string, string | undefined>> = defaultEnv(),
): VercelSandboxCredentials | Record<string, never> {
  if (env.VERCEL_TOKEN && env.VERCEL_TEAM_ID && env.VERCEL_PROJECT_ID) {
    return {
      projectId: env.VERCEL_PROJECT_ID,
      teamId: env.VERCEL_TEAM_ID,
      token: env.VERCEL_TOKEN,
    };
  }
  return {};
}

export async function installAgentBrowserInVercelSandbox(
  sandbox: VercelSandboxSession,
  options: VercelInstallAgentBrowserOptions = {},
): Promise<AgentBrowserCommandResult[]> {
  const results: AgentBrowserCommandResult[] = [];
  const systemDependencies = options.systemDependencies ?? CHROMIUM_SYSTEM_DEPS;

  if (options.installSystemDependencies !== false && systemDependencies.length > 0) {
    const dnfPackages = systemDependencies.map(formatDnfPackageName).join(" ");
    results.push(
      await runVercelCommand(sandbox, "sh", [
        "-c",
        `sudo dnf clean all 2>&1 && sudo dnf install -y --skip-broken -- ${dnfPackages} 2>&1 && sudo ldconfig 2>&1`,
      ], options.onStep, "Installing system dependencies"),
    );
  }

  results.push(
    await runVercelCommand(
      sandbox,
      "npm",
      ["install", "-g", resolveAgentBrowserInstallSpec(options)],
      options.onStep,
      "Installing agent-browser",
    ),
  );

  if (options.installBrowser !== false) {
    results.push(
      await runVercelCommand(
        sandbox,
        "agent-browser",
        ["install"],
        options.onStep,
        "Installing Chrome",
      ),
    );
  }

  return results;
}

export async function createAgentBrowserSandbox(
  options: CreateAgentBrowserSandboxOptions = {},
): Promise<VercelSandboxSession> {
  const env = options.env ?? defaultEnv();
  const snapshotId =
    options.snapshotId === null ? undefined : options.snapshotId ?? env.AGENT_BROWSER_SNAPSHOT_ID;
  const Sandbox = options.Sandbox ?? (await loadVercelSandboxConstructor());
  const createOptions =
    snapshotId === undefined
      ? {
          ...getSandboxCredentials(env),
          runtime: options.runtime ?? "node24",
          timeout: options.timeout ?? 120_000,
          ...options.createOptions,
        }
      : {
          ...getSandboxCredentials(env),
          source: { type: "snapshot", snapshotId },
          timeout: options.timeout ?? 120_000,
          ...options.createOptions,
        };

  const sandbox = await runStep(
    snapshotId === undefined ? "Creating sandbox" : "Booting sandbox from snapshot",
    () => Sandbox.create(createOptions),
    options.onStep,
  );

  if (snapshotId === undefined && options.bootstrap !== false) {
    try {
      await installAgentBrowserInVercelSandbox(sandbox, {
        ...options.install,
        onStep: options.install?.onStep ?? options.onStep,
      });
    } catch (error) {
      try {
        await runStep("Stopping sandbox", () => sandbox.stop(), options.onStep);
      } catch {
        // Preserve the bootstrap failure, which is the actionable error for callers.
      }
      throw error;
    }
  }

  return sandbox;
}

export async function runAgentBrowserCommand<TJson = unknown>(
  sandbox: VercelSandboxSession,
  args: AgentBrowserArgs,
  options: RunAgentBrowserCommandOptions = {},
): Promise<AgentBrowserCommandResult<TJson>> {
  return runVercelCommand<TJson>(
    sandbox,
    "agent-browser",
    buildAgentBrowserArgv(args, options),
    options.onStep,
    options.stepLabel,
  );
}

export async function withAgentBrowserSandbox<T>(
  fn: (sandbox: VercelSandboxSession) => Promise<T>,
  options: WithAgentBrowserSandboxOptions = {},
): Promise<T> {
  const sandbox = await createAgentBrowserSandbox(options);
  let callbackFailed = false;
  try {
    return await fn(sandbox);
  } catch (error) {
    callbackFailed = true;
    throw error;
  } finally {
    if (options.stop !== false) {
      try {
        await runStep("Stopping sandbox", () => sandbox.stop(), options.onStep);
      } catch (error) {
        if (!callbackFailed) {
          throw error;
        }
      }
    }
  }
}

export async function createAgentBrowserSnapshot(
  options: Omit<CreateAgentBrowserSandboxOptions, "bootstrap" | "snapshotId"> = {},
): Promise<string> {
  const env = options.env ?? defaultEnv();
  const Sandbox = options.Sandbox ?? (await loadVercelSandboxConstructor());
  const sandbox = await runStep(
    "Creating sandbox",
    () =>
      Sandbox.create({
        ...getSandboxCredentials(env),
        runtime: options.runtime ?? "node24",
        timeout: options.timeout ?? 300_000,
        ...options.createOptions,
      }),
    options.onStep,
  );

  let snapshotFailed = false;
  try {
    await installAgentBrowserInVercelSandbox(sandbox, {
      ...options.install,
      onStep: options.install?.onStep ?? options.onStep,
    });
    const snapshot = await runStep("Creating snapshot", () => sandbox.snapshot(), options.onStep);
    return snapshot.snapshotId;
  } catch (error) {
    snapshotFailed = true;
    throw error;
  } finally {
    try {
      await runStep("Stopping sandbox", () => sandbox.stop(), options.onStep);
    } catch (error) {
      if (!snapshotFailed) {
        throw error;
      }
    }
  }
}

async function runVercelCommand<TJson = unknown>(
  sandbox: VercelSandboxSession,
  command: string,
  args: readonly string[],
  onStep?: SandboxStepHandler,
  stepLabel?: string,
): Promise<AgentBrowserCommandResult<TJson>> {
  const renderedCommand = [command, ...args].join(" ");
  return runStep(
    stepLabel ?? renderedCommand,
    async () => {
      const result = await sandbox.runCommand(command, args);
      return throwIfCommandFailed(
        createAgentBrowserCommandResult<TJson>({
          command: renderedCommand,
          exitCode: result.exitCode,
          stderr: await result.stderr(),
          stdout: await result.stdout(),
        }),
      );
    },
    onStep,
  );
}

async function runStep<T>(
  step: string,
  fn: () => Promise<T>,
  onStep: SandboxStepHandler | undefined,
): Promise<T> {
  const start = Date.now();
  onStep?.({ status: "running", step });
  try {
    const result = await fn();
    onStep?.({ elapsed: Date.now() - start, status: "done", step });
    return result;
  } catch (error) {
    onStep?.({ elapsed: Date.now() - start, status: "error", step });
    throw error;
  }
}

async function loadVercelSandboxConstructor(): Promise<VercelSandboxConstructor> {
  const mod = (await import("@vercel/sandbox").catch((error: unknown) => {
    throw new Error(
      `@agent-browser/sandbox/vercel requires @vercel/sandbox. Install it in your app to use this provider. ${String(
        error,
      )}`,
    );
  })) as Record<string, unknown>;
  const Sandbox = mod.Sandbox;
  if (typeof Sandbox !== "function" && typeof Sandbox !== "object") {
    throw new Error("@vercel/sandbox did not export Sandbox.");
  }
  return Sandbox as VercelSandboxConstructor;
}

function defaultEnv(): Readonly<Record<string, string | undefined>> {
  const globalWithProcess = globalThis as typeof globalThis & {
    readonly process?: { readonly env?: Readonly<Record<string, string | undefined>> };
  };
  return globalWithProcess.process?.env ?? {};
}

function formatDnfPackageName(name: string): string {
  if (!SAFE_DNF_PACKAGE_NAME.test(name)) {
    throw new Error(`Invalid system dependency name: ${JSON.stringify(name)}`);
  }
  return quoteShellArg(name);
}
