import type { LanguageModel } from "ai";
import { createOpenAI } from "@ai-sdk/openai";

/**
 * Resolves a model identifier string to a LanguageModel instance.
 *
 * Supported prefixes:
 * - "minimax/" — MiniMax via OpenAI-compatible API (requires MINIMAX_API_KEY)
 * - Other prefixes (e.g. "anthropic/") — passed through for AI SDK provider registry
 */
export function resolveModel(modelId: string): LanguageModel | string {
  if (modelId.startsWith("minimax/")) {
    const minimax = createOpenAI({
      name: "minimax",
      baseURL: "https://api.minimax.io/v1",
      apiKey: process.env.MINIMAX_API_KEY,
    });
    return minimax(modelId.slice("minimax/".length));
  }

  // Built-in providers (anthropic/, openai/, etc.) resolved by AI SDK registry
  return modelId;
}

/**
 * Returns true if the model uses Anthropic's provider (supports cache control).
 */
export function isAnthropicModel(modelId: string): boolean {
  return modelId.startsWith("anthropic/");
}
