import type { Metadata } from "next";
import { PAGE_TITLES } from "./page-titles";

import { canonicalUrlFor, isPreviewDeployment, siteDescription } from "./site";

export function pageMetadata(slug: string): Metadata {
  const title = PAGE_TITLES[slug];
  if (!title) return {};

  const displayTitle = title.replace(/\n/g, " ");
  const fullTitle = slug
    ? `${displayTitle} | agent-browser`
    : "agent-browser | Browser Automation for AI";
  const ogImageUrl = slug ? `/og/${slug}` : "/og";
  const href = slug ? `/${slug}` : "/";

  return {
    title: slug ? displayTitle : { absolute: fullTitle },
    description: siteDescription,
    alternates: {
      canonical: canonicalUrlFor(href),
      types: { "text/markdown": slug ? `${href}.md` : "/index.md" },
    },
    ...(isPreviewDeployment()
      ? { robots: { index: false, follow: false } }
      : {}),
    openGraph: {
      type: "website",
      locale: "en_US",
      siteName: "agent-browser",
      title: fullTitle,
      description: siteDescription,
      url: canonicalUrlFor(href),
      images: [
        {
          url: ogImageUrl,
          width: 1200,
          height: 630,
          alt: slug ? `${displayTitle} - agent-browser` : "agent-browser",
        },
      ],
    },
    twitter: {
      card: "summary_large_image",
      title: fullTitle,
      description: siteDescription,
      images: [ogImageUrl],
    },
  };
}
