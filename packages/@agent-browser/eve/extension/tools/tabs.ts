import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

export default defineTool({
  description:
    'Manage browser tabs: list them, open a new one, switch, or close. Tabs have stable ids like "t1" and optional user-assigned labels; both work as targets.',
  inputSchema: z.object({
    action: z.enum(["list", "new", "switch", "close"]).default("list"),
    label: z
      .string()
      .optional()
      .describe('Label to assign when opening a new tab, e.g. "docs".'),
    target: z
      .string()
      .optional()
      .describe('Tab id ("t2") or label. Required for "switch"; "close" defaults to the active tab.'),
    url: z.string().optional().describe('URL to open when action is "new".'),
  }),
  async execute({ action, label, target, url }, ctx) {
    switch (action) {
      case "list":
        return await runBrowser(ctx, ["tab"]);
      case "new": {
        const args = ["tab", "new"];
        if (label !== undefined) args.push("--label", label);
        if (url !== undefined) args.push(url);
        return await runBrowser(ctx, args);
      }
      case "switch":
        if (target === undefined) {
          throw new Error('The "switch" action requires a target tab id or label.');
        }
        return await runBrowser(ctx, ["tab", target]);
      case "close":
        return await runBrowser(ctx, target === undefined ? ["tab", "close"] : ["tab", "close", target]);
    }
  },
});
