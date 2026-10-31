import extension from "../extension";
import type { BrowserToolContext } from "./browser";

export interface ScreenshotOutput extends Record<string, unknown> {
  /** Data URL of the captured image, for channels/UIs to render inline. Hidden from the model. */
  readonly imageDataUrl?: string;
  readonly path?: string;
}

/** Structural subset of eve's sandbox handle; readBinaryFile ships in eve >= 0.22. */
interface SandboxWithFiles {
  readBinaryFile?(options: { readonly path: string }): PromiseLike<Uint8Array | null>;
}

const MAX_INLINE_SCREENSHOT_BYTES = 4 * 1024 * 1024;

/** Adds the captured image as a data URL when inline screenshots are enabled. */
export async function inlineScreenshot(
  data: unknown,
  ctx: BrowserToolContext,
): Promise<ScreenshotOutput> {
  const output = { ...(data as ScreenshotOutput | null) };
  const imageDataUrl = await inlineImage(ctx, output.path);
  return imageDataUrl === undefined ? output : { ...output, imageDataUrl };
}

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
