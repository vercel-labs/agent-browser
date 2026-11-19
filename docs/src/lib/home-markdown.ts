import home from "../../content/landing/home";
import { canonicalUrlFor } from "./site";

/** Primary documentation page for each homepage feature tab. */
const FEATURE_DOCS: Record<string, string> = {
  drive: "/commands",
  snapshots: "/snapshots",
  verify: "/recording",
  performance: "/profiler",
  sessions: "/sessions",
  network: "/network",
};

const urlFor = (href: string) =>
  href.startsWith("http") ? href : canonicalUrlFor(href);

/** Homepage Markdown shares the copy rendered by the landing page. */
export function homeMarkdown(): string {
  return [
    "# agent-browser",
    "",
    home.hero.description,
    "",
    `## ${home.useCases.heading}`,
    "",
    ...home.useCases.items.map(
      ({ title, visual, description }) =>
        `- **[${title}](${urlFor(FEATURE_DOCS[visual] ?? "/introduction")})**: ${description}`,
    ),
    "",
    "## Browser and desktop",
    "",
    `- [${home.hero.browser.headline}](${urlFor(home.hero.browser.docsHref)})`,
    `- [${home.hero.desktop.headline}](${urlFor(home.hero.desktop.docsHref)})`,
    "",
    "## Docs",
    "",
    `Read the [agent-browser documentation](${urlFor("/introduction")}) for installation, the command reference, configuration, and troubleshooting. The [llms.txt](${urlFor("/llms.txt")}) index lists every page, and each page is available as Markdown at its \`.md\` URL or with an \`Accept: text/markdown\` header.`,
    "",
  ].join("\n");
}
