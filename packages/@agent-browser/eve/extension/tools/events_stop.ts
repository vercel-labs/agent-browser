import { defineTool } from "eve/tools";
import { z } from "zod";

import { runEvents } from "../lib/browser";

export default defineTool({
  description:
    "Stop an event subscription and release its watch. The owner is this eve session and is set automatically.",
  inputSchema: z.object({
    id: z.string().describe("Subscription id returned by events_start."),
  }),
  async execute({ id }, ctx) {
    return await runEvents(ctx, ["events", "stop", id, "--owner", ctx.session.id]);
  },
});
