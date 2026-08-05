export interface ExecResult {
  success: boolean;
  exit_code: number | null;
  stdout: string;
  stderr: string;
}

export async function execCommand(args: string[]): Promise<ExecResult> {
  try {
    const resp = await fetch("/api/exec", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ args }),
    });
    return resp.json();
  } catch {
    return {
      success: false,
      exit_code: null,
      stdout: "",
      stderr: "Network error: dashboard server unreachable",
    };
  }
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
    return resp.json();
  } catch {
    return { success: false };
  }
}
