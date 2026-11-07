import { loadAllDocsSources } from "@/lib/docs-source";
import { applyDocsResponseHeaders } from "@/lib/docs-response-headers";
import { siteDescription, siteName, siteUrl } from "@/lib/site";

export async function GET() {
  const sources = await loadAllDocsSources();
  const body = [
    `# ${siteName}`,
    "",
    `> ${siteDescription}`,
    "",
    `Documentation: [${siteUrl}](${siteUrl})`,
    "",
    `Full page Markdown is available at each .md URL or with Accept: text/markdown.`,
    "",
    "## Pages",
    "",
    ...sources.map(
      (source) =>
        `- [${source.title}](${source.canonicalUrl}): [Markdown](${siteUrl}${source.markdownHref})`,
    ),
    "",
  ].join("\n");
  const headers = new Headers({ "Content-Type": "text/plain; charset=utf-8" });
  applyDocsResponseHeaders(headers);
  return new Response(body, { headers });
}
