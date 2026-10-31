import {
  createSource,
  type GeistdocsSourceBundle,
} from "@vercel/geistdocs/source";
import { docs } from "@/.source/server";
import { loadDocsSource } from "@/lib/docs-source";
import { config } from "./config";

const bundle = createSource({ docs, config, baseUrl: "/" });

export const geistdocsSource: GeistdocsSourceBundle = {
  ...bundle,
  async getPageMarkdown(page) {
    const source = await loadDocsSource(`/${page.slugs.join("/")}`);
    if (!source) throw new Error(`Documentation source not found: ${page.url}`);
    return source.markdown;
  },
};
