import {
  buildShellCommand,
  createAgentBrowserCommandResult,
  defaultSessionName,
  quoteShellArg,
  resolveAgentBrowserInstallSpec,
  throwIfCommandFailed,
  type AgentBrowserArgs,
  type AgentBrowserCommandResult,
  type AgentBrowserInstallOptions,
  type BuildShellCommandOptions,
} from "./shared.js";

export {
  AgentBrowserCommandError,
  DEFAULT_AGENT_BROWSER_INSTALL_SPEC,
  buildAgentBrowserArgv,
  quoteShellArg,
  resolveAgentBrowserInstallSpec,
  type AgentBrowserCommandResult,
  type AgentBrowserInstallOptions,
} from "./index.js";

export interface EveSandboxCommandResult {
  readonly exitCode?: number;
  readonly stderr?: string;
  readonly stdout?: string;
}

export interface EveSandboxSession {
  readonly id: string;
  run(options: { readonly abortSignal?: AbortSignal; readonly command: string }): PromiseLike<EveSandboxCommandResult>;
}

export interface EveToolContext {
  getSandbox(): PromiseLike<EveSandboxSession | null>;
}

export interface EveInstallAgentBrowserOptions extends AgentBrowserInstallOptions {
  readonly abortSignal?: AbortSignal;
  readonly npmBinary?: string;
}

export interface EveRunAgentBrowserOptions extends Omit<BuildShellCommandOptions, "session"> {
  readonly abortSignal?: AbortSignal;
  readonly session?: string;
  readonly sessionPrefix?: string;
}

const APT_CHROMIUM_DEPENDENCIES: readonly (readonly [base: string, t64Variant?: string])[] = [
  ["libxcb-shm0"],
  ["libx11-xcb1"],
  ["libx11-6"],
  ["libxcb1"],
  ["libxext6"],
  ["libxrandr2"],
  ["libxcomposite1"],
  ["libxcursor1"],
  ["libxdamage1"],
  ["libxfixes3"],
  ["libxi6"],
  ["libgtk-3-0", "libgtk-3-0t64"],
  ["libglib2.0-0", "libglib2.0-0t64"],
  ["libpangocairo-1.0-0", "libpangocairo-1.0-0t64"],
  ["libpango-1.0-0", "libpango-1.0-0t64"],
  ["libatk1.0-0", "libatk1.0-0t64"],
  ["libcairo-gobject2", "libcairo-gobject2t64"],
  ["libcairo2", "libcairo2t64"],
  ["libgdk-pixbuf-2.0-0", "libgdk-pixbuf-2.0-0t64"],
  ["libxrender1"],
  ["libasound2", "libasound2t64"],
  ["libfreetype6"],
  ["libfontconfig1"],
  ["libdbus-1-3", "libdbus-1-3t64"],
  ["libnss3"],
  ["libnspr4"],
  ["libatk-bridge2.0-0", "libatk-bridge2.0-0t64"],
  ["libdrm2"],
  ["libxkbcommon0"],
  ["libatspi2.0-0", "libatspi2.0-0t64"],
  ["libcups2", "libcups2t64"],
  ["libxshmfence1"],
  ["libgbm1"],
  ["fonts-noto-color-emoji"],
  ["fonts-noto-cjk"],
  ["fonts-freefont-ttf"],
];

const DNF_CHROMIUM_DEPENDENCIES = [
  "glib2",
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

const EVE_BOOTSTRAP_REVISION = "3";

export function agentBrowserRevalidationKey(options: AgentBrowserInstallOptions = {}): string {
  return [
    "agent-browser",
    `bootstrap-${EVE_BOOTSTRAP_REVISION}`,
    resolveAgentBrowserInstallSpec(options),
    options.installBrowser === false ? "no-browser" : "browser",
    options.installSystemDependencies === false ? "no-system-deps" : "system-deps",
  ].join(":");
}

export async function installAgentBrowser(
  sandbox: EveSandboxSession,
  options: EveInstallAgentBrowserOptions = {},
): Promise<AgentBrowserCommandResult[]> {
  const npmBinary = options.npmBinary ?? "npm";
  const installSpec = resolveAgentBrowserInstallSpec(options);
  const commands = [];

  if (options.installSystemDependencies !== false) {
    commands.push(buildLinuxSystemDependenciesCommand());
  }

  commands.push(`${quoteShellArg(npmBinary)} install -g ${quoteShellArg(installSpec)}`);

  if (options.installBrowser !== false) {
    commands.push(buildShellCommand(["install"], { binary: "agent-browser", json: false }));
  }

  const results: AgentBrowserCommandResult[] = [];
  for (const command of commands) {
    const result = await sandbox.run({ abortSignal: options.abortSignal, command });
    results.push(
      throwIfCommandFailed(
        createAgentBrowserCommandResult({
          command,
          exitCode: result.exitCode,
          stderr: result.stderr,
          stdout: result.stdout,
        }),
      ),
    );
  }
  return results;
}

export async function runAgentBrowser<TJson = unknown>(
  ctx: EveToolContext,
  args: AgentBrowserArgs,
  options: EveRunAgentBrowserOptions = {},
): Promise<AgentBrowserCommandResult<TJson>> {
  const sandbox = await ctx.getSandbox();
  if (sandbox === null) {
    throw new Error("agent-browser requires an Eve sandbox. Configure agent/sandbox.ts first.");
  }

  const session = options.session ?? defaultSessionName(options.sessionPrefix ?? "eve", sandbox.id);
  const command = buildAgentBrowserCommand(args, { ...options, session });
  const result = await sandbox.run({ abortSignal: options.abortSignal, command });

  return throwIfCommandFailed(
    createAgentBrowserCommandResult<TJson>({
      command,
      exitCode: result.exitCode,
      stderr: result.stderr,
      stdout: result.stdout,
    }),
  );
}

export function buildAgentBrowserCommand(
  args: AgentBrowserArgs,
  options: EveRunAgentBrowserOptions = {},
): string {
  return buildShellCommand(args, options);
}

function buildLinuxSystemDependenciesCommand(): string {
  const aptPackages = APT_CHROMIUM_DEPENDENCIES.map(formatAptDependency).join(" ");
  const dnfPackages = DNF_CHROMIUM_DEPENDENCIES.map(quoteShellArg).join(" ");
  return [
    "if command -v apt-get >/dev/null 2>&1; then",
    `sudo apt-get update && sudo apt-get install -y --no-install-recommends ${aptPackages} && sudo ldconfig;`,
    "elif command -v dnf >/dev/null 2>&1; then",
    `sudo dnf clean all && sudo dnf install -y --skip-broken -- ${dnfPackages} && sudo ldconfig;`,
    "else echo 'No supported package manager found for browser system dependencies.' >&2; exit 1; fi",
  ].join(" ");
}

function formatAptDependency([base, t64Variant]: readonly [string, string?]): string {
  if (t64Variant === undefined) {
    return quoteShellArg(base);
  }
  return `$(if apt-cache show ${quoteShellArg(t64Variant)} >/dev/null 2>&1; then printf %s ${quoteShellArg(
    t64Variant,
  )}; else printf %s ${quoteShellArg(base)}; fi)`;
}
