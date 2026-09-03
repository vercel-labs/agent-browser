import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

export default defineTool({
  description:
    "Read a page as agent-friendly text (markdown when available). With a URL it fetches without the browser; without one it reads the rendered active tab, including logged-in state. Prefer this over snapshot for reading articles or docs.",
  inputSchema: z.object({
    filter: z.string().optional().describe("Narrow the output to matching sections or headings."),
    llms: z
      .enum(["index", "full"])
      .optional()
      .describe("Read the site's llms.txt link index or llms-full.txt instead of the page."),
    outline: z.boolean().default(false).describe("Return only a compact heading outline."),
    url: z.string().optional().describe("URL to fetch. Omit to read the active tab."),
  }),
  async execute({ filter, llms, outline, url }, ctx) {
    const args = ["read"];
    if (url !== undefined) args.push(url);
    if (outline) args.push("--outline");
    if (llms !== undefined) args.push("--llms", llms);
    if (filter !== undefined) args.push("--filter", filter);
    return await runBrowser(ctx, args);
  },
});
