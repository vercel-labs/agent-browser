import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Mock dependencies before importing the module
vi.mock("@/lib/docs-navigation", () => ({
  allDocsPages: [],
}));

vi.mock("@/lib/mdx-to-markdown", () => ({
  mdxToCleanMarkdown: (raw: string) => raw,
}));

vi.mock("@/lib/rate-limit", () => ({
  minuteRateLimit: {
    limit: vi.fn().mockResolvedValue({ success: true }),
  },
  dailyRateLimit: {
    limit: vi.fn().mockResolvedValue({ success: true }),
  },
}));

vi.mock("next/headers", () => ({
  headers: vi.fn().mockResolvedValue({
    get: vi.fn().mockReturnValue("127.0.0.1"),
  }),
}));

vi.mock("bash-tool", () => ({
  createBashTool: vi.fn().mockResolvedValue({
    tools: {
      bash: { description: "bash tool" },
      readFile: { description: "readFile tool" },
    },
  }),
}));

const mockStreamText = vi.fn().mockReturnValue({
  toUIMessageStreamResponse: vi.fn().mockReturnValue(new Response("ok")),
});

vi.mock("ai", () => ({
  streamText: mockStreamText,
  convertToModelMessages: vi.fn().mockResolvedValue([]),
  stepCountIs: vi.fn().mockReturnValue(5),
}));

vi.mock("@/lib/model", () => ({
  resolveModel: vi.fn((id: string) => {
    if (id.startsWith("minimax/"))
      return { modelId: id.slice(8), provider: "minimax" };
    return id;
  }),
  isAnthropicModel: vi.fn((id: string) => id.startsWith("anthropic/")),
}));

describe("docs-chat route", () => {
  const originalEnv = process.env;

  beforeEach(() => {
    process.env = { ...originalEnv };
    vi.clearAllMocks();
  });

  afterEach(() => {
    process.env = originalEnv;
    vi.resetModules();
  });

  it("uses anthropic model by default", async () => {
    delete process.env.DOCS_CHAT_MODEL;
    // Re-import to pick up the default
    const routeModule = await import("../../docs-chat/route");
    const req = new Request("http://localhost/api/docs-chat", {
      method: "POST",
      body: JSON.stringify({ messages: [] }),
    });

    await routeModule.POST(req);

    expect(mockStreamText).toHaveBeenCalledWith(
      expect.objectContaining({
        model: "anthropic/claude-haiku-4.5",
      }),
    );
  });

  it("applies Anthropic cache control for anthropic models", async () => {
    delete process.env.DOCS_CHAT_MODEL;
    const routeModule = await import("../../docs-chat/route");
    const req = new Request("http://localhost/api/docs-chat", {
      method: "POST",
      body: JSON.stringify({ messages: [] }),
    });

    await routeModule.POST(req);

    const call = mockStreamText.mock.calls[0][0];
    expect(call.prepareStep).toBeDefined();
    // prepareStep should add cache control for Anthropic
    const prepared = call.prepareStep({
      messages: [{ role: "user", content: "hello" }],
    });
    expect(prepared.messages).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          providerOptions: expect.objectContaining({
            anthropic: { cacheControl: { type: "ephemeral" } },
          }),
        }),
      ]),
    );
  });
});

describe("docs-chat route with MiniMax", () => {
  const originalEnv = process.env;

  beforeEach(() => {
    process.env = {
      ...originalEnv,
      DOCS_CHAT_MODEL: "minimax/MiniMax-M2.7",
      MINIMAX_API_KEY: "test-minimax-key",
    };
    vi.clearAllMocks();
  });

  afterEach(() => {
    process.env = originalEnv;
    vi.resetModules();
  });

  it("uses MiniMax model when DOCS_CHAT_MODEL is set", async () => {
    const routeModule = await import("../../docs-chat/route");
    const req = new Request("http://localhost/api/docs-chat", {
      method: "POST",
      body: JSON.stringify({ messages: [] }),
    });

    await routeModule.POST(req);

    const { resolveModel } = await import("@/lib/model");
    expect(resolveModel).toHaveBeenCalledWith("minimax/MiniMax-M2.7");
  });

  it("skips Anthropic cache control for MiniMax models", async () => {
    const routeModule = await import("../../docs-chat/route");
    const req = new Request("http://localhost/api/docs-chat", {
      method: "POST",
      body: JSON.stringify({ messages: [] }),
    });

    await routeModule.POST(req);

    const call = mockStreamText.mock.calls[0][0];
    const prepared = call.prepareStep({
      messages: [{ role: "user", content: "hello" }],
    });
    // For non-Anthropic models, messages should pass through unchanged
    expect(prepared.messages).toEqual([
      { role: "user", content: "hello" },
    ]);
  });
});
