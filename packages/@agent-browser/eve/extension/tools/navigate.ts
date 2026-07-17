import { defineTool } from "eve/tools";
import { z } from "zod";

import extension from "../extension";
import { runBrowser, type BrowserToolContext } from "../lib/browser";

interface SessionInfo {
  readonly provider?: string;
  readonly providerMetadata?: unknown;
}

interface NavigateOutput extends Record<string, unknown> {
  readonly provider?: string;
  readonly providerMetadata?: unknown;
}

async function withProviderMetadata(
  ctx: BrowserToolContext,
  output: Record<string, unknown>,
): Promise<NavigateOutput> {
  if (!extension.config.includeProviderMetadata) {
    return output;
  }
  try {
    const info = await runBrowser<SessionInfo>(ctx, ["session", "info"]);
    return {
      ...output,
      ...(info.provider === undefined ? {} : { provider: info.provider }),
      ...(info.providerMetadata === undefined
        ? {}
        : { providerMetadata: info.providerMetadata }),
    };
  } catch {
    // Provider metadata is for channel observability only. Navigation remains
    // usable with older CLIs or providers that do not expose session details.
    return output;
  }
}

export default defineTool({
  description:
    "Navigate the sandboxed browser: open a URL, or go back/forward/reload. Launches the browser on first use. After navigating, call the snapshot tool to see the page.",
  inputSchema: z.object({
    action: z
      .enum(["goto", "back", "forward", "reload"])
      .default("goto")
      .describe('"goto" opens the given URL; the others act on history.'),
    url: z.string().optional().describe('Required when action is "goto".'),
  }),
  async execute({ action, url }, ctx) {
    if (action === "goto") {
      if (url === undefined) {
        throw new Error('The "goto" action requires a url.');
      }
      const output = await runBrowser<Record<string, unknown>>(ctx, ["open", url]);
      return await withProviderMetadata(ctx, output);
    }
    // Skip the extra session-info round-trip for history actions; live-view
    // URLs are most useful when a session starts or changes page.
    return await runBrowser<Record<string, unknown>>(ctx, [action]);
  },
  // Provider live-view URLs are capability-bearing UI data. Preserve them in
  // the channel result while keeping the model-facing result unchanged. Any
  // other tool that attaches provider/providerMetadata must strip them here too.
  toModelOutput(output) {
    const { provider: _provider, providerMetadata: _providerMetadata, ...visible } = output;
    return { type: "json", value: visible };
  },
});
