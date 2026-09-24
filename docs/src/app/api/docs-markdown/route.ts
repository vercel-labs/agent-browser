import { applyMarkdownHeaders } from "@vercel/agent-readability";
import { NextRequest, NextResponse } from "next/server";
import {
  isSafePathSegments,
  loadDocsSource,
  normalizeDocsHref,
} from "@/lib/docs-source";
import { applyDocsResponseHeaders } from "@/lib/docs-response-headers";

export async function GET(req: NextRequest) {
  const docPath = req.nextUrl.searchParams.get("path");
  const headers = new Headers();
  applyDocsResponseHeaders(headers);
  if (!docPath) {
    return NextResponse.json(
      { error: "Missing ?path= parameter" },
      { status: 400, headers },
    );
  }
  const href = normalizeDocsHref(
    docPath.startsWith("/") ? docPath : `/${docPath}`,
  );
  const segments = href === "/" ? [] : href.slice(1).split("/");
  const page = isSafePathSegments(segments) ? await loadDocsSource(href) : null;
  if (!page) {
    headers.set("X-Robots-Tag", "noindex, nofollow");
    return NextResponse.json(
      { error: "Page not found" },
      { status: 404, headers },
    );
  }
  headers.set("Content-Type", "text/markdown; charset=utf-8");
  applyMarkdownHeaders(headers, { canonicalUrl: page.canonicalUrl });
  applyDocsResponseHeaders(headers);
  return new NextResponse(page.legacyMarkdown, { headers });
}
