import { defineTool } from "eve/tools";
import { z } from "zod";

import { runBrowser, SELECTOR_HINT } from "../lib/browser";

const PAGE_PROPERTIES = new Set(["title", "url"]);

export default defineTool({
  description:
    "Read a property of the page or an element: text content, innerHTML, input value, an attribute, the page title or URL, a match count, bounding box, or computed styles.",
  inputSchema: z.object({
    attribute: z.string().optional().describe('Attribute name, required when property is "attr".'),
    property: z.enum(["text", "html", "value", "attr", "title", "url", "count", "box", "styles"]),
    selector: z
      .string()
      .optional()
      .describe(`Required for element properties. ${SELECTOR_HINT}`),
  }),
  async execute({ attribute, property, selector }, ctx) {
    const args = ["get", property];
    if (!PAGE_PROPERTIES.has(property)) {
      if (selector === undefined) {
        throw new Error(`The "${property}" property requires a selector.`);
      }
      args.push(selector);
      if (property === "attr") {
        if (attribute === undefined) {
          throw new Error('The "attr" property requires an attribute name.');
        }
        args.push(attribute);
      }
    }
    return await runBrowser(ctx, args);
  },
});
