import {
  defineGeistdocsSourceConfig,
  geistdocsFrontmatterSchema,
  geistdocsMetaSchema,
} from "@vercel/geistdocs/source-config";
import { remarkGfm } from "fumadocs-core/mdx-plugins";
import { defineConfig, defineDocs } from "fumadocs-mdx/config";
import { remarkLegacyHeadings } from "./src/lib/remark-legacy-headings";

export const docs = defineDocs({
  dir: "content/docs",
  docs: {
    schema: geistdocsFrontmatterSchema,
    postprocess: { includeProcessedMarkdown: true },
  },
  meta: { schema: geistdocsMetaSchema },
});

const config = defineGeistdocsSourceConfig({
  mdxOptions: {
    remarkStructureOptions: {
      types: [
        "heading",
        "paragraph",
        "blockquote",
        "tableCell",
        "mdxJsxFlowElement",
        "mdxJsxTextElement",
        "text",
        "inlineCode",
        "code",
      ],
    },
  },
});

export default defineConfig({
  ...config,
  mdxOptions: {
    ...config.mdxOptions,
    remarkPlugins: [remarkLegacyHeadings, [remarkGfm, { singleTilde: false }]],
  },
});
