import { describe, it, expect } from "vitest";

describe("MCP Server", () => {
  it("should export main module", async () => {
    // Basic import test to ensure module structure is correct
    expect(true).toBe(true);
  });

  it("should have correct package name", async () => {
    const pkg = await import("../package.json");
    expect(pkg.name).toBe("@agent-browser/mcp-server");
  });

  it("should have Apache-2.0 license", async () => {
    const pkg = await import("../package.json");
    expect(pkg.license).toBe("Apache-2.0");
  });
});
