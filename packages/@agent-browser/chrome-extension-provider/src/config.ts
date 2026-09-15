import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { constants } from "node:fs";
import { chmod, lstat, mkdir, open, rename } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import { promisify } from "node:util";
import type { Scope } from "./protocol.js";

export function stateDirectory(env: NodeJS.ProcessEnv = process.env): string {
  const explicit = env.AGENT_BROWSER_CHROME_EXTENSION_DIR;
  if (explicit && !isAbsolute(explicit)) throw new Error("AGENT_BROWSER_CHROME_EXTENSION_DIR must be absolute");
  return explicit || join(homedir(), ".agent-browser", "chrome-extension");
}

/** Scope labels locate grants; the independently generated tokens authorize them. */
export function requestScope(request: Record<string, unknown>, env: NodeJS.ProcessEnv = process.env): Scope {
  const session = request.session ?? "default";
  const namespace = env.AGENT_BROWSER_NAMESPACE ?? "";
  if (typeof session !== "string" || !session || session.length > 128 || /[\x00-\x1f\x7f]/.test(session)) {
    throw new Error("session must be a nonempty label of at most 128 characters");
  }
  if (namespace.length > 256 || /[\x00-\x1f\x7f]/.test(namespace)) throw new Error("Invalid namespace");
  return { namespace, session };
}

/** State remains local to this OS user, including on Windows. */
export async function ensurePrivateDirectory(path: string): Promise<void> {
  const existed = await lstat(path).then(() => true).catch(error => {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
    throw error;
  });
  await mkdir(path, { recursive: true, mode: 0o700 });
  const info = await lstat(path);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error("Provider state must be a real directory");
  if (process.platform === "win32") {
    await privateWindowsPath(path, !existed);
  } else {
    if (info.uid !== process.getuid?.()) throw new Error("Provider state directory belongs to another user");
    await chmod(path, 0o700);
  }
}

export async function readPrivateJson(path: string): Promise<Record<string, unknown> | undefined> {
  let handle;
  try {
    handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
    const info = await handle.stat();
    if (!info.isFile() || info.size > 16_384) throw new Error("Invalid provider state file");
    if (process.platform !== "win32" && (info.uid !== process.getuid?.() || (info.mode & 0o077) !== 0)) {
      throw new Error("Provider state file must be private to its owner");
    }
    if (process.platform === "win32") await privateWindowsPath(path, false);
    let value: unknown;
    try { value = JSON.parse(await handle.readFile("utf8")); }
    catch { throw new Error("Invalid provider state JSON"); }
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid provider state");
    return value as Record<string, unknown>;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  } finally { await handle?.close(); }
}

/** Inspect ownership and complete ACLs; an owner can rewrite an otherwise private DACL. */
async function privateWindowsPath(path: string, initialize: boolean): Promise<void> {
  const script = `$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$path = $env:AGENT_BROWSER_PRIVATE_PATH
${initialize ? `$acl = New-Object System.Security.AccessControl.DirectorySecurity
$acl.SetAccessRuleProtection($true, $false)
$acl.SetOwner($sid)
$rule = New-Object System.Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
$acl.AddAccessRule($rule)
Set-Acl -LiteralPath $path -AclObject $acl` : ""}
$acl = Get-Acl -LiteralPath $path
$allowed = @($sid.Value, 'S-1-5-18', 'S-1-5-32-544')
if ($allowed -notcontains $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value) { throw 'State owner is not trusted' }
foreach ($rule in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) {
  if ($rule.AccessControlType -eq 'Allow' -and $allowed -notcontains $rule.IdentityReference.Value) { throw 'State access is not private' }
}`;
  try {
    await promisify(execFile)("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], {
      windowsHide: true, timeout: 5000, env: { ...process.env, AGENT_BROWSER_PRIVATE_PATH: path },
    });
  } catch { throw new Error("Provider state grants access to another account; choose a new private state directory"); }
}

export async function writePrivateJson(path: string, value: unknown): Promise<void> {
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  const handle = await open(temporary, "wx", 0o600);
  try { await handle.writeFile(JSON.stringify(value)); } finally { await handle.close(); }
  await rename(temporary, path);
}
