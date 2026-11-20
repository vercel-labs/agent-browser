import type { GeistdocsNavItem } from "@vercel/geistdocs/config";

/** Browser docs live at the introduction. */
const browserDocsHref = "/introduction";

/** Navbar links shared by the docs and marketing layouts. */
export function docsNav(docsHref: string = browserDocsHref): GeistdocsNavItem[] {
  return [
    { label: "Docs", href: docsHref },
    {
      label: "npm",
      href: "https://www.npmjs.com/package/agent-browser",
      external: true,
    },
  ];
}
