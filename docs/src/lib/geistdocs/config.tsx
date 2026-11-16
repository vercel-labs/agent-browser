import { defineConfig } from "@vercel/geistdocs/config";
import { siteUrl } from "@/lib/site";
import { ProductWordmark } from "@/components/landing/product-context";

export const config = defineConfig({
  title: "agent-browser",
  siteUrl,
  defaultLanguage: "en",
  logo: <ProductWordmark />,
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
    { label: "Docs", href: "/introduction" },
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
