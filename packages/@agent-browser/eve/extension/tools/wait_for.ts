import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT, type BrowserToolContext } from "../lib/browser";

interface WaitOutcome {
  readonly condition: string;
  readonly satisfied: boolean;
  readonly timedOut?: boolean;
  readonly result?: unknown;
}

export default defineTool({
  description:
    "Wait for conditions on the page: an element, text, a URL pattern, a load state, a JavaScript expression, and/or a fixed delay. Provide at least one; multiple conditions are awaited in sequence (load state first, delay last). Navigation already waits for the page to load, so this is only needed for content that appears later. A condition that does not come true within the timeout returns satisfied: false rather than failing — note that busy pages may never reach networkidle.",
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
    timeMs: z
      .number()
      .int()
      .positive()
      .max(30_000)
      .optional()
      .describe("Wait a fixed number of milliseconds."),
    timeoutMs: z
      .number()
      .int()
      .positive()
      .max(30_000)
      .default(10_000)
      .describe("Max time to wait per condition before giving up."),
    urlPattern: z.string().optional().describe('Wait for a URL glob pattern, e.g. "**/dashboard".'),
  }),
  async execute({ jsCondition, loadState, selector, text, timeMs, timeoutMs, urlPattern }, ctx) {
    const waits: { args: string[]; condition: string; isDelay?: boolean }[] = [];
    if (loadState !== undefined) {
      waits.push({ args: ["wait", "--load", loadState], condition: `load state ${loadState}` });
    }
    if (selector !== undefined) {
      waits.push({ args: ["wait", selector], condition: `element ${selector}` });
    }
    if (text !== undefined) {
      waits.push({ args: ["wait", "--text", text], condition: `text "${text}"` });
    }
    if (urlPattern !== undefined) {
      waits.push({ args: ["wait", "--url", urlPattern], condition: `url ${urlPattern}` });
    }
    if (jsCondition !== undefined) {
      waits.push({ args: ["wait", "--fn", jsCondition], condition: `js ${jsCondition}` });
    }
    if (timeMs !== undefined) {
      // A fixed delay is its own duration; --timeout would override it.
      waits.push({ args: ["wait", String(timeMs)], condition: `${timeMs}ms delay`, isDelay: true });
    }
    if (waits.length === 0) {
      throw new Error(
        "Provide at least one of: selector, text, urlPattern, loadState, jsCondition, timeMs.",
      );
    }
    const cap = timeoutMs ?? 10_000;
    const outcomes: WaitOutcome[] = [];
    for (const wait of waits) {
      const args = wait.isDelay ? wait.args : [...wait.args, "--timeout", String(cap)];
      outcomes.push(await runWait(ctx, args, wait.condition));
    }
    return outcomes.length === 1 ? outcomes[0] : outcomes;
  },
});

async function runWait(
  ctx: BrowserToolContext,
  args: readonly string[],
  condition: string,
): Promise<WaitOutcome> {
  try {
    const result = await runBrowser(ctx, args);
    return { condition, satisfied: true, result };
  } catch (error) {
    // A wait that runs out of time is an answer ("it didn't happen"), not a
    // failure — surfacing it as an error sends models into retry loops.
    if (error instanceof Error && /timed out/i.test(error.message)) {
      return { condition, satisfied: false, timedOut: true };
    }
    throw error;
  }
}
