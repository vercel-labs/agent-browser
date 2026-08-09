import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

interface SnapshotData {
  readonly origin: string;
  readonly snapshot: string;
}

async function snapshotWithRetry(
  ctx: Parameters<typeof runBrowser>[0],
  args: readonly string[],
): Promise<SnapshotData> {
  try {
    return await runBrowser<SnapshotData>(ctx, args);
  } catch (error) {
    // A snapshot taken while the page is mid-navigation can hit transient
    // CDP races (stale node ids). One delayed retry rides them out.
    if (!(error instanceof Error) || !error.message.includes("CDP error")) {
      throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
    return await runBrowser<SnapshotData>(ctx, args);
  }
}

export default defineTool({
  description:
    'Take an accessibility snapshot of the current page. Elements are annotated with refs like [ref=e12] that other tools accept as "@e12" selectors. Use this before interacting with a page.',
  inputSchema: z.object({
    compact: z.boolean().default(true).describe("Remove empty structural elements."),
    depth: z.number().int().positive().optional().describe("Limit tree depth."),
    includeUrls: z.boolean().default(false).describe("Include href URLs for links."),
    interactiveOnly: z
      .boolean()
      .default(false)
      .describe("Only include interactive elements (buttons, links, inputs)."),
    selector: z
      .string()
      .optional()
      .describe('Scope the snapshot to a CSS selector, e.g. "#main". Not a ref like "@e1".'),
  }),
  async execute({ compact, depth, includeUrls, interactiveOnly, selector }, ctx) {
    const argsFor = (cssSelector: string | undefined) => {
      const args = ["snapshot"];
      if (interactiveOnly) args.push("--interactive");
      if (compact) args.push("--compact");
      if (includeUrls) args.push("--urls");
      if (depth !== undefined) args.push("--depth", String(depth));
      if (cssSelector !== undefined) args.push("--selector", cssSelector);
      return args;
    };
    // Refs (@eN) come from snapshots and are not valid CSS scopes; a whole-page
    // snapshot covers the ref'd element anyway, so drop them rather than fail.
    const cssSelector = selector !== undefined && !selector.startsWith("@") ? selector : undefined;
    let data: SnapshotData;
    try {
      data = await snapshotWithRetry(ctx, argsFor(cssSelector));
    } catch (error) {
      // A scoped snapshot can fail on an invalid or non-matching selector;
      // the whole page always includes whatever the scope would have shown.
      if (cssSelector === undefined) {
        throw error;
      }
      data = await snapshotWithRetry(ctx, argsFor(undefined));
    }
    return { origin: data.origin, snapshot: data.snapshot };
  },
});
