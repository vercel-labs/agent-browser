import { applyMarkdownHeaders } from "@vercel/agent-readability";
import { isSafePathSegments, markdownForPathname } from "@/lib/docs-source";
import { applyDocsResponseHeaders } from "@/lib/docs-response-headers";

type RouteContext = { params: Promise<{ slug?: string[] }> };

export async function GET(_request: Request, { params }: RouteContext) {
  const { slug = [] } = await params;
  const pathname = slug.length ? `/${slug.join("/")}` : "/";
  const page = await markdownForPathname(
    isSafePathSegments(slug) ? pathname : "/invalid-path",
  );
  const headers = new Headers({
    "Content-Type": "text/markdown; charset=utf-8",
  });
  applyMarkdownHeaders(headers, { canonicalUrl: page.canonicalUrl });
  applyDocsResponseHeaders(headers);
  if (!page.found) headers.set("X-Robots-Tag", "noindex, nofollow");
  return new Response(page.body, { status: page.found ? 200 : 404, headers });
}
