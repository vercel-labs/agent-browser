import { defineTool } from "eve/tools";
import { z } from "zod";

import { runEvents } from "../lib/browser";

export default defineTool({
  description:
    "Mark events up to a cursor as consumed so an unread check no longer reports them. Pass the cursor from an events_read result. The owner is this eve session and is set automatically.",
  inputSchema: z.object({
    id: z.string().describe("Subscription id returned by events_start."),
    cursor: z.string().describe("Cursor from an events_read result."),
  }),
  async execute({ id, cursor }, ctx) {
    return await runEvents(ctx, ["events", "ack", id, cursor, "--owner", ctx.session.id]);
  },
});
