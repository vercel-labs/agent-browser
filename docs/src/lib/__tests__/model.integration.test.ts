import { describe, it, expect, beforeEach, afterEach } from "vitest";

/**
 * Integration tests for MiniMax model resolution.
 * These tests verify the real @ai-sdk/openai integration without mocks.
 * Skipped in CI unless MINIMAX_API_KEY is set.
 */
describe("model integration", () => {
  const originalEnv = process.env;

  beforeEach(() => {
    process.env = { ...originalEnv };
  });

  afterEach(() => {
    process.env = originalEnv;
  });

  it("resolveModel returns a LanguageModel for minimax/ prefix", async () => {
    process.env.MINIMAX_API_KEY = "test-key-for-integration";

    // Use real (unmocked) resolveModel
    const { resolveModel } = await import("../model");
    const model = resolveModel("minimax/MiniMax-M2.7");

    // Should return a LanguageModel object, not a string
    expect(typeof model).toBe("object");
    expect(model).toHaveProperty("modelId");
    expect(model).toHaveProperty("provider");
  });

  it("resolveModel returns string for built-in providers", async () => {
    const { resolveModel } = await import("../model");
    const model = resolveModel("anthropic/claude-haiku-4.5");
    expect(typeof model).toBe("string");
    expect(model).toBe("anthropic/claude-haiku-4.5");
  });

  it("isAnthropicModel correctly identifies providers", async () => {
    const { isAnthropicModel } = await import("../model");

    expect(isAnthropicModel("anthropic/claude-haiku-4.5")).toBe(true);
    expect(isAnthropicModel("minimax/MiniMax-M2.7")).toBe(false);
    expect(isAnthropicModel("openai/gpt-4o")).toBe(false);
  });
});
