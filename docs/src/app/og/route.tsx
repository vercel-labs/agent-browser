import { renderLabsOg } from "./labs-og";

// The default card's title is sentence case; the site's page and metadata
// titles keep their own wording in PAGE_TITLES.
const title = "Browser automation for AI";

export async function GET() {
  return renderLabsOg({ product: "agent-browser", title });
}
