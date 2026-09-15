import type { BridgeTab, Scope } from "../../src/protocol";

export type SessionView = {
  id: string;
  port: number;
  state: "connecting" | "waiting" | "authorizing" | "authorized" | "controlled" | "stopped";
  scope?: Scope;
  tab?: BridgeTab;
  reason?: string;
};

export type SetupStatus = {
  sessions: SessionView[];
  tabs: Array<BridgeTab & { busy: boolean }>;
};

export function parsePairingCode(value: unknown): { port: number; token: string } {
  if (typeof value !== "string") throw new Error("Paste the pairing code from setup.");
  const match = /^(\d{1,5}):([A-Za-z0-9_-]{43,128})$/.exec(value.trim());
  const port = Number(match?.[1]);
  if (!match || port < 1 || port > 65535) {
    throw new Error("Use the complete pairing code from setup: port:token.");
  }
  return { port, token: match[2] };
}

export function isAllowedPage(url: string): boolean {
  if (url === "about:blank") return true;
  try {
    const parsed = new URL(url);
    return (
      (parsed.protocol === "http:" || parsed.protocol === "https:") &&
      parsed.hostname !== "chromewebstore.google.com" &&
      !(parsed.hostname === "chrome.google.com" && parsed.pathname.startsWith("/webstore"))
    );
  } catch {
    return false;
  }
}
