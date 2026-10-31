import { defineConfig } from "@vercel/geistdocs/config";
import { siteUrl } from "@/lib/site";

export const config = defineConfig({
  title: "agent-browser",
  siteUrl,
  defaultLanguage: "en",
  logo: (
    <span
      className="font-medium tracking-tight"
      style={{ fontFamily: "var(--font-geist-pixel-square)" }}
    >
      agent-browser
    </span>
  ),
  navbarActiveProduct: "agent-browser",
  navbarBrand: "labs",
  github: {
    owner: "vercel-labs",
    repo: "agent-browser",
    branch: "main",
    editPath: "docs/content/docs",
  },
  content: [
    { id: "docs", label: "Documentation", dir: "content/docs", route: "/" },
  ],
  nav: [
    { label: "Docs", href: "/" },
    {
      label: "npm",
      href: "https://www.npmjs.com/package/agent-browser",
      external: true,
    },
  ],
  ai: { enabled: false },
  feedback: { enabled: false },
  language: { enabled: false },
  pageActions: { askAI: false, openInChat: false, copyPage: true },
  webmcp: { enabled: true },
});
