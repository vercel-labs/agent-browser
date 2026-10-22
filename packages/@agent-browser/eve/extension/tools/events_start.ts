import { defineTool } from "eve/tools";
import { z } from "zod";

import { runEvents, SELECTOR_HINT } from "../lib/browser";

const pointerMatch = z.object({
  path: z.string().describe('JSON pointer into the event payload, e.g. "/params/url".'),
  value: z.string(),
});

const pointerRegex = z.object({
  path: z.string().describe('JSON pointer into the event payload, e.g. "/params/url".'),
  regex: z.string().describe("Regular expression the pointed value must match."),
});

export default defineTool({
  description:
    "Start a subscription to something happening in the browser: an element becoming visible or hidden, or a CDP event whose payload matches JSON-pointer filters. Run it before the action that should cause the change, then use events_read with a wait instead of polling. Returns the subscription id. The owner is this eve session and is set automatically.",
  inputSchema: z.object({
    selector: z
      .string()
      .optional()
      .describe(`Element whose visibility to watch. ${SELECTOR_HINT}`),
    state: z
      .enum(["visible", "hidden"])
      .optional()
      .describe('Which transition to report. Defaults to "visible".'),
    method: z.string().optional().describe('CDP method to watch, e.g. "Page.frameNavigated".'),
    matches: z
      .array(pointerMatch)
      .optional()
      .describe("Require each JSON pointer to equal its value."),
    matchRegex: z
      .array(pointerRegex)
      .optional()
      .describe("Require each JSON pointer to match its regular expression."),
    once: z
      .boolean()
      .optional()
      .describe("End the subscription after its first stored event."),
    ttlSeconds: z
      .number()
      .int()
      .positive()
      .optional()
      .describe("End the subscription after this many seconds."),
    key: z.string().optional().describe("Caller-chosen key that names the subscription."),
  }),
  async execute({ selector, state, method, matches, matchRegex, once, ttlSeconds, key }, ctx) {
    if ((selector === undefined) === (method === undefined)) {
      throw new Error("Provide exactly one selector or method.");
    }
    if (method === undefined && ((matches?.length ?? 0) > 0 || (matchRegex?.length ?? 0) > 0)) {
      throw new Error("matches and matchRegex require a method.");
    }

    const args = ["events", "start"];
    if (selector !== undefined) {
      args.push("--selector", selector);
      if (state !== undefined) {
        args.push("--state", state);
      }
    } else if (method !== undefined) {
      args.push("--method", method);
      for (const match of matches ?? []) {
        args.push("--match", `${match.path}=${match.value}`);
      }
      for (const match of matchRegex ?? []) {
        args.push("--match-regex", `${match.path}=${match.regex}`);
      }
    }
    if (once === true) {
      args.push("--once");
    }
    if (ttlSeconds !== undefined) {
      args.push("--ttl", String(ttlSeconds));
    }
    args.push("--owner", ctx.session.id);
    if (key !== undefined) {
      args.push("--key", key);
    }
    return await runEvents(ctx, args);
  },
});
