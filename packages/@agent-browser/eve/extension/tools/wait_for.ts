import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

export default defineTool({
  description:
    "Wait for conditions on the page: an element, text, a URL pattern, a load state, a JavaScript expression, and/or a fixed delay. Provide at least one; multiple conditions are awaited in sequence (load state first, delay last).",
  inputSchema: z.object({
    jsCondition: z
      .string()
      .optional()
      .describe(
        'JavaScript expression to wait for, e.g. "window.ready === true". Also the way to wait for something to disappear: "!document.querySelector(\'#spinner\')".',
      ),
    loadState: z.enum(["load", "domcontentloaded", "networkidle"]).optional(),
    selector: z
      .string()
      .optional()
      .describe(`Wait for this element to be visible. ${SELECTOR_HINT}`),
    text: z.string().optional().describe("Wait for this text to appear (substring match)."),
    timeMs: z.number().int().positive().optional().describe("Wait a fixed number of milliseconds."),
    urlPattern: z.string().optional().describe('Wait for a URL glob pattern, e.g. "**/dashboard".'),
  }),
  async execute({ jsCondition, loadState, selector, text, timeMs, urlPattern }, ctx) {
    const waits: string[][] = [];
    if (loadState !== undefined) waits.push(["wait", "--load", loadState]);
    if (selector !== undefined) waits.push(["wait", selector]);
    if (text !== undefined) waits.push(["wait", "--text", text]);
    if (urlPattern !== undefined) waits.push(["wait", "--url", urlPattern]);
    if (jsCondition !== undefined) waits.push(["wait", "--fn", jsCondition]);
    if (timeMs !== undefined) waits.push(["wait", String(timeMs)]);
    if (waits.length === 0) {
      throw new Error(
        "Provide at least one of: selector, text, urlPattern, loadState, jsCondition, timeMs.",
      );
    }
    const results = [];
    for (const args of waits) {
      results.push(await runBrowser(ctx, args));
    }
    return results.length === 1 ? results[0] : results;
  },
});
