export type Category = "skill-loading" | "skill-selection" | "command-usage";
export type ProviderName = "claude" | "codex";

export interface ProviderOptions {
  model?: string;
  timeout?: number;
}

export interface ProviderResponse {
  output: string;
  durationMs: number;
  error?: string;
}

export interface Provider {
  name: ProviderName;
  defaultModel: string;
  call(prompt: string, options?: ProviderOptions, context?: string): Promise<ProviderResponse>;
  callRaw(prompt: string, options?: ProviderOptions): Promise<ProviderResponse>;
}

export interface EvalCase {
  id: string;
  name: string;
  category: Category;
  /** The user task prompt sent to the model */
  prompt: string;
  /** Additional context injected after the skill content (e.g., simulated skill output) */
  context?: string;
  /** Regex patterns that must all match in the response */
  expectedPatterns: string[];
  /** Regex patterns that must NOT match in the response */
  forbiddenPatterns?: string[];
  /** Rubric for LLM judge quality scoring (1-5) */
  rubric?: string;
}

export interface PatternResult {
  pattern: string;
  matched: boolean;
  type: "expected" | "forbidden";
}

export interface JudgeResult {
  score: number;
  reasoning: string;
}

export interface EvalResult {
  caseId: string;
  caseName: string;
  category: Category;
  pass: boolean;
  patternResults: PatternResult[];
  judge?: JudgeResult;
  response: string;
  durationMs: number;
  error?: string;
}

export interface EvalSummary {
  total: number;
  passed: number;
  failed: number;
  errors: number;
  byCategory: Record<Category, { total: number; passed: number }>;
  durationMs: number;
}

export interface RunOptions {
  provider: ProviderName;
  model: string;
  category?: Category;
  judge: boolean;
  json: boolean;
  concurrency: number;
  timeout: number;
}
