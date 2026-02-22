import type { Metadata } from "next";
import { PAGE_TITLES } from "./page-titles";

const DESCRIPTION =
  "Headless browser automation CLI for AI agents";

export function pageMetadata(slug: string): Metadata {
  const title = PAGE_TITLES[slug];
  if (!title) return {};

  const displayTitle = title.replace(/\n/g, " ");
  const fullTitle = `${displayTitle} | agent-browser`;
  const ogImageUrl = slug ? `/og/${slug}` : "/og";

  return {
    title: displayTitle,
    openGraph: {
      type: "website",
      locale: "en_US",
      siteName: "agent-browser",
      title: fullTitle,
      description: DESCRIPTION,
      images: [
        {
          url: ogImageUrl,
          width: 1200,
          height: 630,
          alt: `${displayTitle} - agent-browser`,
        },
      ],
    },
    twitter: {
      card: "summary_large_image",
      title: fullTitle,
      description: DESCRIPTION,
      images: [ogImageUrl],
    },
  };
}
