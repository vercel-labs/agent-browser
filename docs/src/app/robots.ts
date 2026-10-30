import type { MetadataRoute } from "next";
import { isPreviewDeployment, siteUrl } from "@/lib/site";

export default function robots(): MetadataRoute.Robots {
  return {
    rules: isPreviewDeployment()
      ? { userAgent: "*", disallow: "/" }
      : { userAgent: "*", allow: "/" },
    sitemap: [`${siteUrl}/sitemap.xml`, "https://agent-browser.dev/desktop/sitemap.xml"],
  };
}
