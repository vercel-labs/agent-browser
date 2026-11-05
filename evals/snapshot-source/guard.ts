import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Restricts what an eval agent can run. Model output becomes CLI arguments, and
// task pages are untrusted, so only page-level commands are allowed: no host
// file access (upload, download, screenshot or pdf paths, state files), no
// navigation outside the task flow, and no global flags.

const ALLOWED = new Set([
  "snapshot",
  "click",
  "fill",
  "type",
  "select",
  "check",
  "uncheck",
  "press",
  "scroll",
  "wait",
  "get",
  "eval",
  "frame",
  "back",
]);

const ALLOWED_GET = new Set(["url", "title", "text"]);

export type GuardResult = { ok: true; cmd: string[] } | { ok: false; reason: string };

/** Validate a parsed agent command before it reaches the CLI. */
export function guardCommand(cmd: string[]): GuardResult {
  if (cmd.length === 0) return { ok: false, reason: "empty command" };
  const [op, ...rest] = cmd;
  if (!ALLOWED.has(op)) return { ok: false, reason: `command "${op}" is not available in this eval` };
  if (op === "get" && !ALLOWED_GET.has(rest[0] ?? "")) return { ok: false, reason: `get ${rest[0] ?? ""} is not available in this eval` };
  if (op !== "snapshot" && rest.some((t) => t.startsWith("-"))) return { ok: false, reason: "flags are not available in this eval" };
  if (op === "snapshot" && rest.some((t) => t.startsWith("-") && !["-i", "--interactive", "--source", "--viewport", "ax", "apc", "hybrid"].includes(t))) {
    return { ok: false, reason: "snapshot flags are not available in this eval" };
  }
  return { ok: true, cmd };
}


const PASSTHROUGH_ENV = ["PATH", "HOME", "TMPDIR", "LANG", "USER", "SHELL"];

// Downloads a page triggers land in a throwaway directory, not the user's Downloads folder.
const DOWNLOAD_DIR = mkdtempSync(join(tmpdir(), "ab-eval-downloads-"));

/** Environment for the CLI: only basic process variables plus the ones the runner sets. Inherited AGENT_BROWSER_* settings (profiles, file access, providers) never reach it. */
export function cliEnv(extra: Record<string, string>): Record<string, string> {
  const env: Record<string, string> = {};
  for (const k of PASSTHROUGH_ENV) {
    const v = process.env[k];
    if (v !== undefined) env[k] = v;
  }
  return { ...env, NO_COLOR: "1", AGENT_BROWSER_DOWNLOAD_PATH: DOWNLOAD_DIR, ...extra };
}
