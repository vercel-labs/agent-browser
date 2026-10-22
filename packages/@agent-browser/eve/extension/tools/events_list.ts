import { defineTool } from "eve/tools";
import { z } from "zod";

import { runEvents } from "../lib/browser";

export default defineTool({
  description:
    "List this eve session's event subscriptions with their status, rule, head, and consumed cursor. Set unread to only see subscriptions that have events past their consumed cursor.",
  inputSchema: z.object({
    unread: z
      .boolean()
      .optional()
      .describe("Only subscriptions with events past their consumed cursor."),
  }),
  async execute({ unread }, ctx) {
    const args = ["events", "list"];
    if (unread === true) {
      args.push("--unread");
    }
    args.push("--owner", ctx.session.id);
    return await runEvents(ctx, args);
  },
});
