import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { generateNotFoundMarkdown } from "@vercel/agent-readability";
import { parse } from "yaml";
import { allDocsPages } from "./docs-navigation";
import { mdxToCleanMarkdown } from "./mdx-to-markdown";
import { canonicalUrlFor, siteDescription, siteName, siteUrl } from "./site";

export type DocsSource = {
  title: string;
  href: string;
  markdownHref: string;
  canonicalUrl: string;
  description: string;
  markdown: string;
  legacyMarkdown: string;
};

const sourcePromises = new Map<string, Promise<DocsSource>>();
const pagesByHref = new Map(allDocsPages.map((page) => [page.href, page]));

export function isSafePathSegments(segments: readonly string[]): boolean {
  return segments.every(
    (part) =>
      part.length > 0 &&
      part !== "." &&
      part !== ".." &&
      !/[\/\\\u0000-\u001f\u007f]/.test(part),
  );
}

export function normalizeDocsHref(pathname: string): string {
  if (pathname === "/") return "/";
  return pathname.endsWith("/") ? pathname.slice(0, -1) : pathname;
}

function cleanMarkdown(body: string): string {
  let fence: string | undefined;
  return body
    .split("\n")
    .flatMap((line) => {
      const marker = line.match(/^\s*(`{3,}|~{3,})/)?.[1];
      if (fence) {
        if (
          marker?.[0] === fence[0] &&
          marker.length >= fence.length &&
          /^\s*(`+|~+)\s*$/.test(line)
        ) {
          fence = undefined;
        }
        return [line];
      }
      if (marker) {
        fence = marker;
        return [line];
      }
      if (
        /^\s*import \{ DiffDemo \} from ["']@\/components\/diff-demo["'];?\s*$/.test(
          line,
        ) ||
        /^\s*<DiffDemo\s*\/>\s*$/.test(line)
      )
        return [];
      return [
        line.replace(/(<[a-z][^>]*?)\sclassName=(?:"[^"]*"|'[^']*')/g, "$1"),
      ];
    })
    .join("\n")
    .trim();
}

export async function loadDocsSource(href: string): Promise<DocsSource | null> {
  const normalized = normalizeDocsHref(href);
  if (!pagesByHref.has(normalized)) return null;
  let pending = sourcePromises.get(normalized);
  if (!pending) {
    const slug = normalized === "/" ? "index" : normalized.slice(1);
    pending = readFile(
      join(process.cwd(), "content", "docs", `${slug}.mdx`),
      "utf8",
    ).then((raw) => {
      const frontmatter = raw.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n/);
      if (!frontmatter)
        throw new Error(`Missing frontmatter for ${normalized}`);
      const data = parse(frontmatter[1] ?? "") as {
        title?: unknown;
        description?: unknown;
      } | null;
      if (typeof data?.title !== "string")
        throw new Error(`Missing title for ${normalized}`);
      const body = raw.slice(frontmatter[0].length);
      const preamble = body.match(/^(?:import [^\n]*\r?\n\r?\n)+/)?.[0] ?? "";
      const original = `${preamble}# ${data.title}\n${body.slice(preamble.length)}`;
      return {
        title: data.title,
        href: normalized,
        markdownHref: normalized === "/" ? "/index.md" : `${normalized}.md`,
        canonicalUrl: canonicalUrlFor(normalized),
        description:
          typeof data.description === "string"
            ? data.description
            : siteDescription,
        markdown: `# ${data.title}\n\n${cleanMarkdown(body)}`,
        legacyMarkdown: mdxToCleanMarkdown(original),
      };
    });
    sourcePromises.set(normalized, pending);
    pending.catch(() => {
      if (sourcePromises.get(normalized) === pending)
        sourcePromises.delete(normalized);
    });
  }
  return pending;
}

export async function loadAllDocsSources(): Promise<DocsSource[]> {
  const sources = await Promise.all(
    allDocsPages.map((page) => loadDocsSource(page.href)),
  );
  return sources.filter((source): source is DocsSource => source !== null);
}

export async function sitemapMarkdown(): Promise<string> {
  const sources = await loadAllDocsSources();
  return [
    `# ${siteName} documentation`,
    "",
    ...sources.map(
      (source) =>
        `- [${source.title}](${source.canonicalUrl}): [Markdown](${siteUrl}${source.markdownHref})`,
    ),
    "",
  ].join("\n");
}

export async function markdownForPathname(pathname: string): Promise<{
  body: string;
  canonicalUrl: string;
  found: boolean;
}> {
  const normalized = normalizeDocsHref(pathname);
  if (normalized === "/sitemap") {
    return {
      body: await sitemapMarkdown(),
      canonicalUrl: canonicalUrlFor("/sitemap.md"),
      found: true,
    };
  }
  const source = await loadDocsSource(normalized);
  if (source) {
    return {
      body: [
        "---",
        `title: ${JSON.stringify(source.title)}`,
        `description: ${JSON.stringify(source.description)}`,
        `canonical_url: ${JSON.stringify(source.canonicalUrl)}`,
        "---",
        "",
        source.markdown,
        "",
      ].join("\n"),
      canonicalUrl: source.canonicalUrl,
      found: true,
    };
  }
  return {
    body: generateNotFoundMarkdown(normalized, {
      sitemapUrl: "/sitemap.md",
      indexUrl: "/llms.txt",
      exampleUrl: "/commands",
      baseUrl: siteUrl,
    }),
    canonicalUrl: canonicalUrlFor(normalized),
    found: false,
  };
}
