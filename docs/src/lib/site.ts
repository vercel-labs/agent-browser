export const siteUrl = "https://agent-browser.dev";
export const siteName = "agent-browser";
export const siteDescription = "Browser automation CLI for AI agents";

export function canonicalUrlFor(href: string): string {
  return `${siteUrl}${href === "/" ? "" : href}`;
}

export function isPreviewDeployment(): boolean {
  return Boolean(
    process.env.VERCEL_ENV && process.env.VERCEL_ENV !== "production",
  );
}
