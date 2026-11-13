export interface ExecResult {
  success: boolean;
  exit_code: number | null;
  stdout: string;
  stderr: string;
}

function failedResult(stderr: string): ExecResult {
  return { success: false, exit_code: null, stdout: "", stderr };
}

function isExecResult(value: unknown): value is ExecResult {
  if (!value || typeof value !== "object") return false;
  const result = value as Record<string, unknown>;
  return (
    typeof result.success === "boolean" &&
    (result.exit_code === null ||
      (typeof result.exit_code === "number" && Number.isFinite(result.exit_code))) &&
    typeof result.stdout === "string" &&
    typeof result.stderr === "string"
  );
}

export async function execCommand(args: string[]): Promise<ExecResult> {
  let resp: Response;
  try {
    resp = await fetch("/api/exec", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ args }),
    });
  } catch {
    return failedResult("Network error: dashboard server unreachable");
  }

  let result: unknown;
  try {
    result = await resp.json();
  } catch {
    return failedResult(
      resp.ok
        ? "Invalid response from dashboard server"
        : `Dashboard request failed (HTTP ${resp.status})`,
    );
  }
  if (!isExecResult(result)) {
    return failedResult(
      resp.ok
        ? "Invalid response from dashboard server"
        : `Dashboard request failed (HTTP ${resp.status})`,
    );
  }
  if (!resp.ok) {
    return {
      ...result,
      success: false,
      stderr: result.stderr || `Dashboard request failed (HTTP ${resp.status})`,
    };
  }
  return result;
}

export function sessionArgs(session: string, ...args: string[]): string[] {
  return ["--session", session, ...args];
}

export async function killSession(session: string): Promise<{ success: boolean; killed_pid?: number }> {
  try {
    const resp = await fetch("/api/kill", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ session }),
    });
    const result: unknown = await resp.json();
    if (!resp.ok || !result || typeof result !== "object") {
      return { success: false };
    }
    const payload = result as Record<string, unknown>;
    if (
      typeof payload.success !== "boolean" ||
      (payload.killed_pid !== undefined &&
        (typeof payload.killed_pid !== "number" || !Number.isFinite(payload.killed_pid)))
    ) {
      return { success: false };
    }
    return {
      success: payload.success,
      ...(typeof payload.killed_pid === "number" ? { killed_pid: payload.killed_pid } : {}),
    };
  } catch {
    return { success: false };
  }
}
