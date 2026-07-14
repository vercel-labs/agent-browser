import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

export default defineTool({
  description:
    "List network requests tracked for the current page, or fetch the full detail of one request by id. Filter by URL substring, resource type, HTTP method, or status.",
  inputSchema: z.object({
    filter: z.string().optional().describe("Only requests whose URL contains this text."),
    method: z.string().optional().describe('HTTP method filter, e.g. "POST".'),
    requestId: z
      .string()
      .optional()
      .describe("Return full request/response detail for this request id instead of a list."),
    resourceTypes: z
      .array(z.string())
      .optional()
      .describe('Resource type filter, e.g. ["xhr", "fetch"].'),
    status: z.string().optional().describe('Status filter: "200", "2xx", or a range like "400-499".'),
  }),
  async execute({ filter, method, requestId, resourceTypes, status }, ctx) {
    if (requestId !== undefined) {
      return await runBrowser(ctx, ["network", "request", requestId]);
    }
    const args = ["network", "requests"];
    if (filter !== undefined) args.push("--filter", filter);
    if (resourceTypes !== undefined && resourceTypes.length > 0) {
      args.push("--type", resourceTypes.join(","));
    }
    if (method !== undefined) args.push("--method", method);
    if (status !== undefined) args.push("--status", status);
    return await runBrowser(ctx, args);
  },
});
