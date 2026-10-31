import { AGENT_BROWSER_SANDBOX_VERSION } from "./version.js";

export { AGENT_BROWSER_SANDBOX_VERSION };

export type AgentBrowserArgs = readonly string[];

export interface AgentBrowserInstallOptions {
  readonly installBrowser?: boolean;
  readonly installSpec?: string;
  /** Install Chromium system libraries before installing agent-browser. Defaults to true. */
  readonly installSystemDependencies?: boolean;
}

export interface AgentBrowserRunOptions {
  readonly binary?: string;
  readonly json?: boolean;
  readonly session?: string;
}

export interface AgentBrowserCommandResult<TJson = unknown> {
  readonly command: string;
  readonly exitCode: number;
  readonly json: TJson | null;
  readonly stderr: string;
  readonly stdout: string;
}

export interface BuildAgentBrowserArgvOptions {
  readonly json?: boolean;
  readonly session?: string;
}

export interface BuildShellCommandOptions extends BuildAgentBrowserArgvOptions {
  readonly binary?: string;
  readonly env?: Readonly<Record<string, string | undefined>>;
}

export const DEFAULT_AGENT_BROWSER_INSTALL_SPEC = `agent-browser@${AGENT_BROWSER_SANDBOX_VERSION}`;

const SAFE_SHELL_ARG = /^[A-Za-z0-9_/:=.,@%+-]+$/;
const SAFE_ENV_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/;
const MAX_DEFAULT_SESSION_NAME_LENGTH = 48;

export class AgentBrowserCommandError extends Error {
  readonly command: string;
  readonly exitCode: number;
  readonly stderr: string;
  readonly stdout: string;

  constructor(result: Pick<AgentBrowserCommandResult, "command" | "exitCode" | "stderr" | "stdout">) {
    const detail = result.stderr.trim() || result.stdout.trim() || `exit ${result.exitCode}`;
    super(`agent-browser command failed: ${result.command}\n${detail}`);
    this.name = "AgentBrowserCommandError";
    this.command = result.command;
    this.exitCode = result.exitCode;
    this.stderr = result.stderr;
    this.stdout = result.stdout;
  }
}

export function resolveAgentBrowserInstallSpec(options: AgentBrowserInstallOptions = {}): string {
  const spec = options.installSpec?.trim();
  return spec && spec.length > 0 ? spec : DEFAULT_AGENT_BROWSER_INSTALL_SPEC;
}

export function quoteShellArg(value: string): string {
  if (value.length > 0 && SAFE_SHELL_ARG.test(value)) {
    return value;
  }
  return `'${value.replaceAll("'", "'\\''")}'`;
}

export function buildAgentBrowserArgv(
  args: AgentBrowserArgs,
  options: BuildAgentBrowserArgvOptions = {},
): string[] {
  const argv: string[] = [];
  if (options.session !== undefined && options.session.length > 0) {
    argv.push("--session", options.session);
  }
  argv.push(...args);
  if (options.json !== false && !argv.includes("--json")) {
    argv.push("--json");
  }
  return argv;
}

export function buildShellCommand(args: readonly string[], options: BuildShellCommandOptions = {}): string {
  const binary = options.binary ?? "agent-browser";
  const argv = buildAgentBrowserArgv(args, options);
  const command = [binary, ...argv].map(quoteShellArg).join(" ");
  const env = formatShellEnv(options.env);
  return env.length > 0 ? `${env} ${command}` : command;
}

export function createAgentBrowserCommandResult<TJson = unknown>(input: {
  readonly command: string;
  readonly exitCode?: number;
  readonly stderr?: string;
  readonly stdout?: string;
}): AgentBrowserCommandResult<TJson> {
  const stdout = input.stdout ?? "";
  return {
    command: input.command,
    exitCode: input.exitCode ?? 0,
    json: parseJson<TJson>(stdout),
    stderr: input.stderr ?? "",
    stdout,
  };
}

export function throwIfCommandFailed<TJson>(
  result: AgentBrowserCommandResult<TJson>,
): AgentBrowserCommandResult<TJson> {
  if (result.exitCode !== 0) {
    throw new AgentBrowserCommandError(result);
  }
  return result;
}

export function defaultSessionName(prefix: string, id: string): string {
  const safePrefix = sanitizeSessionPart(prefix) || "agent-browser";
  const safeId = sanitizeSessionPart(id) || "default";
  return truncateSessionName(`${safePrefix}-${safeId}`);
}

function formatShellEnv(env: Readonly<Record<string, string | undefined>> | undefined): string {
  if (env === undefined) return "";
  return Object.entries(env)
    .filter((entry): entry is [string, string] => entry[1] !== undefined)
    .map(([key, value]) => {
      if (!SAFE_ENV_KEY.test(key)) {
        throw new Error(`Invalid environment variable name: ${key}`);
      }
      return `${key}=${quoteShellArg(value)}`;
    })
    .join(" ");
}

function parseJson<TJson>(value: string): TJson | null {
  try {
    return JSON.parse(value) as TJson;
  } catch {
    return null;
  }
}

function sanitizeSessionPart(value: string): string {
  return value.trim().replaceAll(/[^A-Za-z0-9_-]+/g, "-").replaceAll(/^-+|-+$/g, "");
}

function truncateSessionName(value: string): string {
  if (value.length <= MAX_DEFAULT_SESSION_NAME_LENGTH) {
    return value;
  }

  const hash = hashSessionName(value);
  const suffix = `-${hash}`;
  const prefixLength = MAX_DEFAULT_SESSION_NAME_LENGTH - suffix.length;
  const prefix = value.slice(0, prefixLength).replaceAll(/[-_]+$/g, "");
  return `${prefix || "agent-browser"}${suffix}`;
}

function hashSessionName(value: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < value.length; i += 1) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}
