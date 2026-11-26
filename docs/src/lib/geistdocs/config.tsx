import { defineConfig } from "@vercel/geistdocs/config";
import { siteUrl } from "@/lib/site";
import { docsNav } from "@/lib/geistdocs/nav";
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
  nav: docsNav(),
  ai: { enabled: false },
  feedback: { enabled: false },
  language: { enabled: false },
  pageActions: { askAI: false, openInChat: false, copyPage: true },
  webmcp: { enabled: true },
});
