import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser } from "../lib/browser";

export default defineTool({
  description:
    'Find an element semantically (by ARIA role, text, label, placeholder, alt text, title, or test id) and act on it in one step. Useful when you know what the element is without taking a snapshot, e.g. find the "Email" label and fill it.',
  inputSchema: z.object({
    action: z.enum(["click", "fill", "check", "hover", "text"]),
    by: z.enum(["role", "text", "label", "placeholder", "alt", "title", "testid", "first", "last"]),
    exact: z
      .boolean()
      .default(false)
      .describe(
        'Exact, case-sensitive match. For "role" it applies to the accessible name, whose default is a case-insensitive substring.',
      ),
    name: z
      .string()
      .optional()
      .describe('Accessible name filter when by is "role", e.g. role "button" named "Submit".'),
    query: z
      .string()
      .describe('What to match: the role name, text, label, etc. For "first"/"last", a CSS selector.'),
    value: z.string().optional().describe('Text to enter for the "fill" action.'),
  }),
  async execute({ action, by, exact, name, query, value }, ctx) {
    if (action === "fill" && value === undefined) {
      throw new Error(`The "${action}" action requires a value.`);
    }
    const args = ["find", by, query, action];
    if (value !== undefined) args.push(value);
    if (name !== undefined) args.push("--name", name);
    if (exact) args.push("--exact");
    return await runBrowser(ctx, args);
  },
});
