import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const mockProviderFn = vi.fn(() => ({
  modelId: "MiniMax-M2.7",
  provider: "minimax",
}));
const mockCreateOpenAI = vi.fn(() => mockProviderFn);

vi.mock("@ai-sdk/openai", () => ({
  createOpenAI: (...args: unknown[]) => mockCreateOpenAI(...args),
}));

import { resolveModel, isAnthropicModel } from "../model";

describe("resolveModel", () => {
  const originalEnv = process.env;

  beforeEach(() => {
    process.env = { ...originalEnv };
    vi.clearAllMocks();
  });

  afterEach(() => {
    process.env = originalEnv;
  });

  it("returns a string for anthropic/ models (AI SDK registry)", () => {
    const result = resolveModel("anthropic/claude-haiku-4.5");
    expect(result).toBe("anthropic/claude-haiku-4.5");
  });

  it("returns a string for openai/ models (AI SDK registry)", () => {
    const result = resolveModel("openai/gpt-4o");
    expect(result).toBe("openai/gpt-4o");
  });

  it("creates a MiniMax provider for minimax/ prefix", () => {
    process.env.MINIMAX_API_KEY = "test-key";

    const result = resolveModel("minimax/MiniMax-M2.7");

    expect(mockCreateOpenAI).toHaveBeenCalledWith({
      name: "minimax",
      baseURL: "https://api.minimax.io/v1",
      apiKey: "test-key",
    });
    expect(result).toEqual({ modelId: "MiniMax-M2.7", provider: "minimax" });
  });

  it("strips the minimax/ prefix before passing to provider", () => {
    process.env.MINIMAX_API_KEY = "test-key";

    resolveModel("minimax/MiniMax-M2.5-highspeed");

    expect(mockProviderFn).toHaveBeenCalledWith("MiniMax-M2.5-highspeed");
  });

  it("passes undefined API key when env var is not set", () => {
    delete process.env.MINIMAX_API_KEY;

    resolveModel("minimax/MiniMax-M2.7");

    expect(mockCreateOpenAI).toHaveBeenCalledWith(
      expect.objectContaining({ apiKey: undefined }),
    );
  });

  it("returns string for unknown providers (passed to AI SDK registry)", () => {
    const result = resolveModel("google/gemini-2.0-flash");
    expect(result).toBe("google/gemini-2.0-flash");
  });
});

describe("isAnthropicModel", () => {
  it("returns true for anthropic/ prefix", () => {
    expect(isAnthropicModel("anthropic/claude-haiku-4.5")).toBe(true);
    expect(isAnthropicModel("anthropic/claude-sonnet-4")).toBe(true);
  });

  it("returns false for non-anthropic prefixes", () => {
    expect(isAnthropicModel("minimax/MiniMax-M2.7")).toBe(false);
    expect(isAnthropicModel("openai/gpt-4o")).toBe(false);
  });

  it("returns false for strings without a prefix", () => {
    expect(isAnthropicModel("claude-haiku-4.5")).toBe(false);
  });
});
