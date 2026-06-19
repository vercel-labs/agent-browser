import { readFileSync } from "fs";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";
import type { Provider, ProviderOptions, ProviderResponse } from "./types.ts";

const __dirname = dirname(fileURLToPath(import.meta.url));
const SKILL_PATH = resolve(__dirname, "../../skills/agent-browser/SKILL.md");

const AI_GATEWAY_URL = "https://ai-gateway.vercel.sh";
const DEFAULT_MODEL = "anthropic/claude-sonnet-4.6";

let cachedSkillContent: string | null = null;

function getSkillContent(): string {
  if (!cachedSkillContent) {
    cachedSkillContent = readFileSync(SKILL_PATH, "utf-8");
  }
  return cachedSkillContent;
}

function buildPrompt(userTask: string, context?: string): string {
  const skill = getSkillContent();
  const parts = [
    "You have the following skill installed:\n",
    "<skill>",
    skill,
    "</skill>\n",
  ];
  if (context) {
    parts.push(context + "\n");
  }
  parts.push(
    `Complete this task: ${userTask}\n`,
    "Show the exact shell commands you would run. Do not explain, just show the commands.",
  );
  return parts.join("\n");
}

function getGatewayEnv(): Record<string, string> {
  const apiKey = process.env.AI_GATEWAY_API_KEY;
  if (!apiKey) {
    throw new Error(
      "AI_GATEWAY_API_KEY is not set. Export it before running evals.",
    );
  }
  return {
    ...(process.env as Record<string, string>),
    ANTHROPIC_API_KEY: apiKey,
    ANTHROPIC_BASE_URL: AI_GATEWAY_URL,
  };
}

function spawnClaude(
  prompt: string,
  model: string,
  timeout: number,
): Promise<{ output: string; stderr: string; exitCode: number }> {
  const proc = Bun.spawn(
    ["claude", "-p", "--output-format", "text", "--model", model, prompt],
    {
      stdout: "pipe",
      stderr: "pipe",
      env: getGatewayEnv(),
    },
  );

  return Promise.race([
    (async () => {
      const output = await new Response(proc.stdout).text();
      const stderr = await new Response(proc.stderr).text();
      const exitCode = await proc.exited;
      return { output, stderr, exitCode };
    })(),
    new Promise<never>((_, reject) =>
      setTimeout(() => {
        proc.kill();
        reject(new Error(`Timed out after ${timeout}ms`));
      }, timeout),
    ),
  ]);
}

export const claudeProvider: Provider = {
  name: "claude",
  defaultModel: DEFAULT_MODEL,

  async call(
    userPrompt: string,
    options: ProviderOptions = {},
    context?: string,
  ): Promise<ProviderResponse> {
    const { model = DEFAULT_MODEL, timeout = 60_000 } = options;
    const prompt = buildPrompt(userPrompt, context);
    const start = performance.now();

    try {
      const result = await spawnClaude(prompt, model, timeout);
      const durationMs = Math.round(performance.now() - start);

      if (result.exitCode !== 0) {
        return {
          output: "",
          durationMs,
          error: `claude exited with code ${result.exitCode}: ${result.stderr}`,
        };
      }

      return { output: result.output.trim(), durationMs };
    } catch (err) {
      const durationMs = Math.round(performance.now() - start);
      const message = err instanceof Error ? err.message : String(err);
      return { output: "", durationMs, error: message };
    }
  },

  async callRaw(
    prompt: string,
    options: ProviderOptions = {},
  ): Promise<ProviderResponse> {
    const { model = DEFAULT_MODEL, timeout = 60_000 } = options;
    const start = performance.now();

    try {
      const result = await spawnClaude(prompt, model, timeout);
      const durationMs = Math.round(performance.now() - start);

      if (result.exitCode !== 0) {
        return {
          output: "",
          durationMs,
          error: `claude exited with code ${result.exitCode}: ${result.stderr}`,
        };
      }

      return { output: result.output.trim(), durationMs };
    } catch (err) {
      const durationMs = Math.round(performance.now() - start);
      const message = err instanceof Error ? err.message : String(err);
      return { output: "", durationMs, error: message };
    }
  },
};
