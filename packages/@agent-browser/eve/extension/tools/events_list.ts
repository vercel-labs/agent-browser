import { defineTool } from "eve/tools";
import { z } from "zod";

import { runEvents } from "../lib/browser";

export default defineTool({
  description:
    "List this eve session's event subscriptions with their status, rule, head, and consumed cursor. Set unread to only see subscriptions that have events past their consumed cursor.",
  inputSchema: z.object({
    unread: z.boolean().optional().describe("Only subscriptions with events past their consumed cursor."),
  }),
  async execute({ unread }, ctx) {
    const args = unread === true ? ["events", "list", "--unread"] : ["events", "list"];
    return await runEvents(ctx, [...args, "--owner", ctx.session.id]);
  },
});
