import { readFile } from "fs/promises";
import { join } from "path";
import { convertToModelMessages, stepCountIs, streamText } from "ai";
import type { ModelMessage, UIMessage } from "ai";
import { createBashTool } from "bash-tool";
import { headers } from "next/headers";
import { allDocsPages } from "@/lib/docs-navigation";
import { mdxToCleanMarkdown } from "@/lib/mdx-to-markdown";
import { minuteRateLimit, dailyRateLimit } from "@/lib/rate-limit";

export const maxDuration = 60;

const DEFAULT_MODEL = "anthropic/claude-haiku-4.5";

const SYSTEM_PROMPT = `You are a helpful documentation assistant for agent-browser, a headless browser automation CLI designed for AI agents.

GitHub repository: https://github.com/vercel-labs/agent-browser
Documentation: https://agent-browser.dev
npm package: agent-browser

You have access to the full agent-browser documentation via the bash and readFile tools. The docs are available as markdown files in the /workspace/ directory.

When answering questions:
- Use the bash tool to list files (ls /workspace/) or search for content (grep -r "keyword" /workspace/)
- Use the readFile tool to read specific documentation pages (e.g. readFile with path "/workspace/index.md")
- Do NOT use bash to write, create, modify, or delete files (no tee, cat >, sed -i, echo >, cp, mv, rm, mkdir, touch, etc.) — you are read-only
- Always base your answers on the actual documentation content
- Be concise and accurate
- If the docs don't cover a topic, say so honestly
- Do NOT include source references or file paths in your response
- Do NOT use emojis in your responses`;

async function loadDocsFiles(): Promise<Record<string, string>> {
  const files: Record<string, string> = {};

  const results = await Promise.allSettled(
    allDocsPages.map(async (page) => {
      const slug = page.href === "/" ? "" : page.href.replace(/^\//, "");
      const filePath = slug
        ? join(process.cwd(), "src", "app", slug, "page.mdx")
        : join(process.cwd(), "src", "app", "page.mdx");

      const raw = await readFile(filePath, "utf-8");
      const md = mdxToCleanMarkdown(raw);
      const fileName = slug ? `/${slug}.md` : "/index.md";
      return { fileName, md };
    }),
  );

  for (const result of results) {
    if (result.status === "fulfilled") {
      files[result.value.fileName] = result.value.md;
    }
  }

  return files;
}

function addCacheControl(messages: ModelMessage[]): ModelMessage[] {
  if (messages.length === 0) return messages;
  return messages.map((message, index) => {
    if (index === messages.length - 1) {
      return {
        ...message,
        providerOptions: {
          ...message.providerOptions,
          anthropic: { cacheControl: { type: "ephemeral" } },
        },
      };
    }
    return message;
  });
}

export async function POST(req: Request) {
  const headersList = await headers();
  const ip = headersList.get("x-forwarded-for")?.split(",")[0] ?? "anonymous";

  const [minuteResult, dailyResult] = await Promise.all([
    minuteRateLimit.limit(ip),
    dailyRateLimit.limit(ip),
  ]);

  if (!minuteResult.success || !dailyResult.success) {
    const isMinuteLimit = !minuteResult.success;
    return new Response(
      JSON.stringify({
        error: "Rate limit exceeded",
        message: isMinuteLimit
          ? "Too many requests. Please wait a moment before trying again."
          : "Daily limit reached. Please try again tomorrow.",
      }),
      {
        status: 429,
        headers: { "Content-Type": "application/json" },
      },
    );
  }

  const { messages }: { messages: UIMessage[] } = await req.json();

  const docsFiles = await loadDocsFiles();
  const {
    tools: { bash, readFile },
  } = await createBashTool({ files: docsFiles });

  const result = streamText({
    model: DEFAULT_MODEL,
    system: SYSTEM_PROMPT,
    messages: await convertToModelMessages(messages),
    stopWhen: stepCountIs(5),
    tools: { bash, readFile },
    prepareStep: ({ messages: stepMessages }) => ({
      messages: addCacheControl(stepMessages),
    }),
  });

  return result.toUIMessageStreamResponse();
}
