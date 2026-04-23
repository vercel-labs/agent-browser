import type {
  EvalCase,
  PatternResult,
  JudgeResult,
  EvalResult,
  Provider,
  ProviderOptions,
} from "./types.ts";
import { claudeProvider } from "./claude.ts";

function testPatterns(
  response: string,
  evalCase: EvalCase,
): { pass: boolean; results: PatternResult[] } {
  const results: PatternResult[] = [];
  let pass = true;

  for (const pattern of evalCase.expectedPatterns) {
    const regex = new RegExp(pattern, "is");
    const matched = regex.test(response);
    results.push({ pattern, matched, type: "expected" });
    if (!matched) pass = false;
  }

  if (evalCase.forbiddenPatterns) {
    for (const pattern of evalCase.forbiddenPatterns) {
      const regex = new RegExp(pattern, "is");
      const matched = regex.test(response);
      results.push({ pattern, matched, type: "forbidden" });
      if (matched) pass = false;
    }
  }

  return { pass, results };
}

const JUDGE_PROMPT_TEMPLATE = `You are an eval judge scoring an AI agent's response to a browser automation task.

The agent was given a task and a skill file that instructs it to use agent-browser CLI commands.
Score the response on a scale of 1-5 based on the rubric below.

Rubric:
{rubric}

Response to judge:
<response>
{response}
</response>

Reply with ONLY a JSON object (no markdown fences, no other text):
{{"score": <1-5>, "reasoning": "<one sentence>"}}`;

const JUDGE_MODEL = "anthropic/claude-opus-4.6";

async function runLLMJudge(
  response: string,
  rubric: string,
  options: ProviderOptions,
): Promise<JudgeResult> {
  const prompt = JUDGE_PROMPT_TEMPLATE.replace("{rubric}", rubric).replace(
    "{response}",
    response,
  );

  // Judge always uses Claude regardless of eval provider
  const result = await claudeProvider.callRaw(prompt, {
    model: JUDGE_MODEL,
    timeout: options.timeout ?? 30_000,
  });

  if (result.error) {
    return { score: 0, reasoning: `Judge error: ${result.error}` };
  }

  try {
    const cleaned = result.output.replace(/```json\n?|```\n?/g, "").trim();
    const parsed = JSON.parse(cleaned);
    return {
      score: Math.max(0, Math.min(5, Number(parsed.score) || 0)),
      reasoning: String(parsed.reasoning || ""),
    };
  } catch {
    return {
      score: 0,
      reasoning: `Failed to parse judge response: ${result.output.slice(0, 200)}`,
    };
  }
}

export async function evaluate(
  evalCase: EvalCase,
  provider: Provider,
  options: { model?: string; judge?: boolean; timeout?: number } = {},
): Promise<EvalResult> {
  const providerOptions: ProviderOptions = {
    model: options.model,
    timeout: options.timeout,
  };

  const response = await provider.call(
    evalCase.prompt,
    providerOptions,
    evalCase.context,
  );

  if (response.error) {
    return {
      caseId: evalCase.id,
      caseName: evalCase.name,
      category: evalCase.category,
      pass: false,
      patternResults: [],
      response: "",
      durationMs: response.durationMs,
      error: response.error,
    };
  }

  const { pass, results } = testPatterns(response.output, evalCase);

  let judge: JudgeResult | undefined;
  if (options.judge && evalCase.rubric) {
    judge = await runLLMJudge(
      response.output,
      evalCase.rubric,
      providerOptions,
    );
  }

  return {
    caseId: evalCase.id,
    caseName: evalCase.name,
    category: evalCase.category,
    pass,
    patternResults: results,
    judge,
    response: response.output,
    durationMs: response.durationMs,
  };
}
