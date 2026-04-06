export const DASHBOARD_PORT = 4848;

function getDashboardBaseUrl(): string {
  if (typeof window === "undefined") {
    return `http://127.0.0.1:${DASHBOARD_PORT}`;
  }

  const { protocol, hostname } = window.location;
  return `${protocol}//${hostname}:${DASHBOARD_PORT}`;
}

export interface ExecResult {
  success: boolean;
  exit_code: number | null;
  stdout: string;
  stderr: string;
}

export async function execCommand(args: string[]): Promise<ExecResult> {
  try {
    const resp = await fetch(`${getDashboardBaseUrl()}/api/exec`, {
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
    const resp = await fetch(`${getDashboardBaseUrl()}/api/kill`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ session }),
    });
    return resp.json();
  } catch {
    return { success: false };
  }
}

export { getDashboardBaseUrl };
