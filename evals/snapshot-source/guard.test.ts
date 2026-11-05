import { describe, expect, test } from "bun:test";
import { cliEnv, guardCommand } from "./guard";

describe("guardCommand", () => {
  test("allows page-level commands", () => {
    for (const cmd of [["snapshot", "-i", "--source", "apc"], ["click", "@e3"], ["fill", "@e2", "hello"], ["get", "text", "body"], ["eval", "document.title"], ["frame", "main"], ["back"]]) {
      expect(guardCommand(cmd).ok).toBe(true);
    }
  });
  test("rejects host-file and unlisted commands", () => {
    for (const cmd of [["upload", "@e1", "/etc/passwd"], ["download", "@e1", "/tmp/x"], ["screenshot", "/tmp/x.png"], ["pdf", "/tmp/x.pdf"], ["state", "save", "/tmp/s.json"], ["open", "file:///etc/passwd"], ["install"], ["get", "cookies"]]) {
      expect(guardCommand(cmd).ok).toBe(false);
    }
  });
  test("rejects global and per-command flags", () => {
    for (const cmd of [["click", "@e1", "--allow-file-access"], ["eval", "1", "--profile", "/tmp/p"], ["snapshot", "-i", "--executable-path", "/bin/sh"], ["click", "@e1", "-p", "ios"], ["get", "text", "body", "-q"], ["wait", "-v"]]) {
      expect(guardCommand(cmd).ok).toBe(false);
    }
  });
});

describe("cliEnv", () => {
  test("drops inherited agent-browser settings", () => {
    process.env.AGENT_BROWSER_ALLOW_FILE_ACCESS = "1";
    process.env.AGENT_BROWSER_PROFILE = "/tmp/profile";
    const env = cliEnv({ AGENT_BROWSER_SESSION: "s" });
    expect(env.AGENT_BROWSER_ALLOW_FILE_ACCESS).toBeUndefined();
    expect(env.AGENT_BROWSER_PROFILE).toBeUndefined();
    expect(env.AGENT_BROWSER_SESSION).toBe("s");
    expect(env.AGENT_BROWSER_DOWNLOAD_PATH).toContain("ab-eval-downloads-");
  });
});
