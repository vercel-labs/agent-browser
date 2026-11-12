import {
  createSource,
  type GeistdocsSourceBundle,
} from "@vercel/geistdocs/source";
import { docs } from "@/.source/server";
import { docsIntroHref } from "@/lib/docs-navigation";
import { loadDocsSource } from "@/lib/docs-source";
import { config } from "./config";

const bundle = createSource({ docs, config, baseUrl: "/" });

export const geistdocsSource: GeistdocsSourceBundle = {
  ...bundle,
  source: {
    ...bundle.source,
    getPage(...args) {
      const page = bundle.source.getPage(...args);
      return page && page.slugs.length === 0 ? { ...page, url: docsIntroHref } : page;
    },
    getPages(...args) {
      return bundle.source
        .getPages(...args)
        .map((page) => (page.slugs.length === 0 ? { ...page, url: docsIntroHref } : page));
    },
  },
  async getPageMarkdown(page) {
    const source = await loadDocsSource(`/${page.slugs.join("/")}`);
    if (!source) throw new Error(`Documentation source not found: ${page.url}`);
    return source.markdown;
  },
};
