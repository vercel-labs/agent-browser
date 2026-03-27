const DASHBOARD_PORT = 4848;

export interface ExecResult {
  success: boolean;
  exit_code: number | null;
  stdout: string;
  stderr: string;
}

export async function execCommand(args: string[]): Promise<ExecResult> {
  const resp = await fetch(`http://localhost:${DASHBOARD_PORT}/api/exec`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ args }),
  });
  return resp.json();
}

export function sessionArgs(session: string, ...args: string[]): string[] {
  return ["--session", session, ...args];
}

export async function killSession(session: string): Promise<{ success: boolean; killed_pid?: number }> {
  const resp = await fetch(`http://localhost:${DASHBOARD_PORT}/api/kill`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ session }),
  });
  return resp.json();
}
