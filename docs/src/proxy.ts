import { createProxy } from "@vercel/geistdocs/proxy";
import { createI18nMiddleware } from "fumadocs-core/i18n/middleware";
import {
  NextResponse,
  type NextFetchEvent,
  type NextRequest,
} from "next/server";
import { allDocsPages } from "@/lib/docs-navigation";
import { applyDocsResponseHeaders } from "@/lib/docs-response-headers";
import { config as geistdocsConfig } from "@/lib/geistdocs/config";
import { isPreviewDeployment } from "@/lib/site";

const pages = new Set(allDocsPages.map((page) => page.href));
const localeProxy = createI18nMiddleware({
  defaultLanguage: "en",
  languages: ["en"],
  hideLocale: "default-locale",
});

function isFlightRequest(request: NextRequest): boolean {
  return (
    request.headers.get("rsc") === "1" ||
    request.headers.has("next-router-prefetch") ||
    request.headers.has("next-router-segment-prefetch") ||
    /\bprefetch\b/i.test(request.headers.get("purpose") ?? "") ||
    /\bprefetch\b/i.test(request.headers.get("sec-purpose") ?? "")
  );
}

function notFoundResponse() {
  const response = new NextResponse(
    '<!doctype html><html lang="en"><head><title>Page Not Found | agent-browser</title><meta name="robots" content="noindex, nofollow"></head><body><main><h1>Page Not Found</h1><p>The requested documentation page does not exist.</p><a href="/">Documentation</a></main></body></html>',
    {
      status: 404,
      headers: {
        "Content-Type": "text/html; charset=utf-8",
        "X-Robots-Tag": "noindex, nofollow",
      },
    },
  );
  applyDocsResponseHeaders(response.headers);
  return response;
}

const geistdocsProxy = createProxy({
  config: geistdocsConfig,
  markdownRoutes: [{ from: "/*path", to: "/api/docs-md/*path" }],
  before: async ({ request, context }) => {
    if (isFlightRequest(request)) {
      return (await localeProxy(request, context)) ?? NextResponse.next();
    }
  },
  after: ({ request }) => {
    const pathname =
      decodeURIComponent(request.nextUrl.pathname).replace(/\/$/, "") || "/";
    if (!pages.has(pathname)) return notFoundResponse();
  },
});

export default async function proxy(
  request: NextRequest,
  event: NextFetchEvent,
) {
  const { pathname } = request.nextUrl;
  let decoded: string;
  try {
    decoded = decodeURIComponent(pathname);
  } catch (error) {
    if (!(error instanceof URIError)) throw error;
    return notFoundResponse();
  }
  if (/%2f|%5c/i.test(pathname) || /[\\\u0000-\u001f\u007f]/.test(decoded)) {
    return notFoundResponse();
  }
  if (decoded === "/en" || decoded.startsWith("/en/")) {
    const destination = new URL(request.url);
    destination.pathname = `/${pathname.split("/").slice(2).join("/")}`;
    const response = NextResponse.redirect(destination, 308);
    applyDocsResponseHeaders(response.headers);
    return response;
  }
  if (
    /^\/(?:api|og|_next)(?:\/|$)/.test(decoded) ||
    decoded === "/llms.txt" ||
    decoded === "/sitemap.md" ||
    (decoded.includes(".") && !/\.mdx?$/.test(decoded))
  ) {
    const response = NextResponse.next();
    if (isPreviewDeployment())
      response.headers.set("X-Robots-Tag", "noindex, nofollow");
    return response;
  }
  const response = await geistdocsProxy(request, event);
  applyDocsResponseHeaders(response.headers);
  return response;
}

export const config = {
  matcher: ["/((?!_next/static|_next/image).*)"],
};
