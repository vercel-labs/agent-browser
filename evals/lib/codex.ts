import { readFileSync, writeFileSync, mkdirSync, existsSync } from "fs";
import { resolve, dirname, join } from "path";
import { fileURLToPath } from "url";
import { tmpdir } from "os";
import type { Provider, ProviderOptions, ProviderResponse } from "./types.ts";

const __dirname = dirname(fileURLToPath(import.meta.url));
const SKILL_PATH = resolve(__dirname, "../../skills/agent-browser/SKILL.md");

const AI_GATEWAY_URL = "https://ai-gateway.vercel.sh/v1";
const DEFAULT_MODEL = "openai/o3";

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

let evalHome: string | null = null;

function getEvalHome(model: string): string {
  if (!evalHome) {
    evalHome = join(tmpdir(), `agent-browser-evals-${process.pid}`);
  }
  const configDir = join(evalHome, ".codex");
  const configPath = join(configDir, "config.toml");

  const config = `model = "${model}"
model_provider = "vercel-ai-gateway"

[model_providers.vercel-ai-gateway]
name = "Vercel AI Gateway"
base_url = "${AI_GATEWAY_URL}"
env_key = "AI_GATEWAY_API_KEY"
wire_api = "responses"
`;

  if (!existsSync(configDir)) {
    mkdirSync(configDir, { recursive: true });
  }
  writeFileSync(configPath, config, "utf-8");
  return evalHome;
}

function getCodexEnv(model: string): Record<string, string> {
  const apiKey = process.env.AI_GATEWAY_API_KEY;
  if (!apiKey) {
    throw new Error(
      "AI_GATEWAY_API_KEY is not set. Export it before running evals.",
    );
  }
  return {
    ...(process.env as Record<string, string>),
    HOME: getEvalHome(model),
    AI_GATEWAY_API_KEY: apiKey,
  };
}

function parseJsonlOutput(raw: string): string {
  const lines = raw.split("\n");
  const textParts: string[] = [];

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    try {
      const parsed = JSON.parse(trimmed);
      const eventType = parsed.type as string;

      if (eventType === "item.completed") {
        const item = parsed.item as Record<string, unknown> | undefined;
        if (item?.type === "agent_message") {
          const text = item.text as string | undefined;
          if (text) textParts.push(text);
        }
      }
    } catch {
      // Non-JSON line (e.g. stderr leak), skip
    }
  }

  return textParts.join("\n\n").trim();
}

function spawnCodex(
  prompt: string,
  model: string,
  timeout: number,
): Promise<{ output: string; stderr: string; exitCode: number }> {
  const escaped = prompt.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  const proc = Bun.spawn(
    [
      "codex",
      "exec",
      "--dangerously-bypass-approvals-and-sandbox",
      "--json",
      escaped,
    ],
    {
      stdout: "pipe",
      stderr: "pipe",
      env: getCodexEnv(model),
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

export const codexProvider: Provider = {
  name: "codex",
  defaultModel: DEFAULT_MODEL,

  async call(
    userPrompt: string,
    options: ProviderOptions = {},
    context?: string,
  ): Promise<ProviderResponse> {
    const { model = DEFAULT_MODEL, timeout = 120_000 } = options;
    const prompt = buildPrompt(userPrompt, context);
    const start = performance.now();

    try {
      const result = await spawnCodex(prompt, model, timeout);
      const durationMs = Math.round(performance.now() - start);

      if (result.exitCode !== 0) {
        return {
          output: "",
          durationMs,
          error: `codex exited with code ${result.exitCode}: ${result.stderr}`,
        };
      }

      const output = parseJsonlOutput(result.output);
      return { output, durationMs };
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
    const { model = DEFAULT_MODEL, timeout = 120_000 } = options;
    const start = performance.now();

    try {
      const result = await spawnCodex(prompt, model, timeout);
      const durationMs = Math.round(performance.now() - start);

      if (result.exitCode !== 0) {
        return {
          output: "",
          durationMs,
          error: `codex exited with code ${result.exitCode}: ${result.stderr}`,
        };
      }

      const output = parseJsonlOutput(result.output);
      return { output, durationMs };
    } catch (err) {
      const durationMs = Math.round(performance.now() - start);
      const message = err instanceof Error ? err.message : String(err);
      return { output: "", durationMs, error: message };
    }
  },
};
