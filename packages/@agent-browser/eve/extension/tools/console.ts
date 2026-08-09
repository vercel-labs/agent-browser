import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

interface ConsoleData {
  readonly messages: unknown[];
}

interface ErrorsData {
  readonly errors: unknown[];
}

export default defineTool({
  description:
    "Read the browser console messages and uncaught page errors collected for the current session. Useful for debugging why a page misbehaves.",
  inputSchema: z.object({
    clear: z.boolean().default(false).describe("Clear collected messages and errors after reading."),
  }),
  async execute({ clear }, ctx) {
    const consoleArgs = clear ? ["console", "--clear"] : ["console"];
    const errorArgs = clear ? ["errors", "--clear"] : ["errors"];
    const messages = await runBrowser<ConsoleData>(ctx, consoleArgs);
    const errors = await runBrowser<ErrorsData>(ctx, errorArgs);
    return { errors: errors.errors ?? [], messages: messages.messages ?? [] };
  },
});
