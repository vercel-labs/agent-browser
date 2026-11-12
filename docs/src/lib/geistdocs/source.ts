import {
  createSource,
  type GeistdocsSourceBundle,
} from "@vercel/geistdocs/source";
import type { Node, Root } from "fumadocs-core/page-tree";
import { docs } from "@/.source/server";
import { docsIntroHref } from "@/lib/docs-navigation";
import { loadDocsSource } from "@/lib/docs-source";
import { config } from "./config";

const bundle = createSource({ docs, config, baseUrl: "/" });

/** Points the introduction's tree node at its public URL so the sidebar, breadcrumb, and Previous/Next links match `getPage`. */
function remapIntro(node: Node): Node {
  if (node.type === "page") return node.url === "/" ? { ...node, url: docsIntroHref } : node;
  if (node.type === "folder")
    return {
      ...node,
      index: node.index ? (remapIntro(node.index) as typeof node.index) : undefined,
      children: node.children.map(remapIntro),
    };
  return node;
}

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
    getPageTree(...args) {
      const tree: Root = bundle.source.getPageTree(...args);
      return { ...tree, children: tree.children.map(remapIntro) };
    },
  },
  async getPageMarkdown(page) {
    const source = await loadDocsSource(`/${page.slugs.join("/")}`);
    if (!source) throw new Error(`Documentation source not found: ${page.url}`);
    return source.markdown;
  },
};
