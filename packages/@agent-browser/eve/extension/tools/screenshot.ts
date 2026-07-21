import { defineTool } from "eve/tools";
import { z } from "zod";

import extension from "../extension";
import { runBrowser, type BrowserToolContext } from "../lib/browser";

interface ScreenshotData {
  readonly path?: string;
}

interface ScreenshotOutput extends Record<string, unknown> {
  /** Data URL of the captured image, for channels/UIs to render inline. Hidden from the model. */
  readonly imageDataUrl?: string;
}

/** Structural subset of eve's sandbox handle; readBinaryFile ships in eve >= 0.22. */
interface SandboxWithFiles {
  readBinaryFile?(options: { readonly path: string }): PromiseLike<Uint8Array | null>;
}

const MAX_INLINE_SCREENSHOT_BYTES = 4 * 1024 * 1024;

export default defineTool({
  description:
    "Take a screenshot of the current page and save it inside the sandbox. Returns the sandbox file path. With annotate, interactive elements get numbered labels matching snapshot refs (@eN).",
  inputSchema: z.object({
    annotate: z
      .boolean()
      .default(false)
      .describe("Overlay numbered labels on interactive elements."),
    fullPage: z.boolean().default(false).describe("Capture the full scrollable page."),
    path: z
      .string()
      .optional()
      .describe("Sandbox path to save to. Defaults to a temporary file."),
  }),
  async execute({ annotate, fullPage, path }, ctx): Promise<ScreenshotOutput> {
    const args = ["screenshot"];
    if (path !== undefined) args.push(path);
    if (fullPage) args.push("--full");
    if (annotate) args.push("--annotate");
    const data = await runBrowser<ScreenshotData>(ctx, args);
    const imageDataUrl = await inlineImage(ctx, data?.path);
    return imageDataUrl === undefined ? { ...data } : { ...data, imageDataUrl };
  },
  // The image goes to channels/UIs via the tool output. When it is shown to
  // the user automatically, the model needs neither the base64 nor the sandbox
  // path — surfacing the path invites replies like "saved it at /workspace/…".
  toModelOutput(output) {
    const { imageDataUrl, ...rest } = output;
    if (imageDataUrl === undefined) {
      return { type: "json", value: rest };
    }
    const { path: _path, ...visible } = rest;
    return {
      type: "json",
      value: { ...visible, screenshot: "Captured and already displayed to the user." },
    };
  },
});

async function inlineImage(
  ctx: BrowserToolContext,
  path: string | undefined,
): Promise<string | undefined> {
  if (!extension.config.inlineScreenshots || path === undefined) {
    return undefined;
  }
  try {
    const sandbox = (await ctx.getSandbox()) as SandboxWithFiles | null;
    const bytes = await sandbox?.readBinaryFile?.({ path });
    if (!bytes || bytes.length === 0 || bytes.length > MAX_INLINE_SCREENSHOT_BYTES) {
      return undefined;
    }
    return `data:${mimeTypeFor(path)};base64,${Buffer.from(bytes).toString("base64")}`;
  } catch {
    // Inline rendering is best-effort; the path in the output always works.
    return undefined;
  }
}

function mimeTypeFor(path: string): string {
  const lower = path.toLowerCase();
  if (lower.endsWith(".jpg") || lower.endsWith(".jpeg")) return "image/jpeg";
  if (lower.endsWith(".webp")) return "image/webp";
  return "image/png";
}
