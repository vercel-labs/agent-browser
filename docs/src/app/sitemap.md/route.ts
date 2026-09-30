import { applyMarkdownHeaders } from "@vercel/agent-readability";
import { sitemapMarkdown } from "@/lib/docs-source";
import { applyDocsResponseHeaders } from "@/lib/docs-response-headers";
import { siteUrl } from "@/lib/site";

export async function GET() {
  const headers = new Headers({
    "Content-Type": "text/markdown; charset=utf-8",
  });
  applyMarkdownHeaders(headers, { canonicalUrl: `${siteUrl}/sitemap.md` });
  applyDocsResponseHeaders(headers);
  return new Response(await sitemapMarkdown(), { headers });
}
