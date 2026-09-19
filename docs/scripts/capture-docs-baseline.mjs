import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, posix } from "node:path";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";
import { createProcessor } from "@mdx-js/mdx";
import ts from "typescript";

const commit = "aff6125c023b810ea3f2e5deec5379e9a4270bdc";
const root = fileURLToPath(new URL("../../", import.meta.url));
const output = new URL("../tests/fixtures/docs-baseline.json", import.meta.url);
const git = (...args) =>
  execFileSync("git", args, { cwd: root, maxBuffer: 16 * 1024 * 1024 });
const hash = (value) => createHash("sha256").update(value).digest("hex");
const sources = {};
function original(path) {
  if (!sources[path]) {
    const raw = git("show", `${commit}:${path}`).toString("utf8");
    sources[path] = { sha256: hash(raw), raw };
  }
  return sources[path].raw;
}
const modules = new Map();
function evaluate(raw, filename, imports = {}) {
  const evaluated = { exports: {} };
  const js = ts.transpileModule(raw, {
    fileName: filename,
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  runInNewContext(
    js,
    {
      module: evaluated,
      exports: evaluated.exports,
      URL,
      require(specifier) {
        if (Object.hasOwn(imports, specifier)) return imports[specifier];
        if (specifier.startsWith(".")) {
          return load(
            `${posix.normalize(posix.join(posix.dirname(filename), specifier))}.ts`,
          );
        }
        throw new Error(
          `Unapproved baseline import ${specifier} in ${filename}`,
        );
      },
    },
    { filename },
  );
  return evaluated.exports;
}
function load(path) {
  if (!modules.has(path)) modules.set(path, evaluate(original(path), path));
  return modules.get(path);
}
function initializer(raw, name, filename) {
  const file = ts.createSourceFile(
    filename,
    raw,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  let result;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(file) === name) {
      result = node.initializer.getText(file);
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
  assert.ok(result, `${filename}: ${name}`);
  return result;
}
const componentSource = original("docs/mdx-components.tsx");
const headingFunctions = componentSource.slice(
  componentSource.indexOf("function slugify"),
  componentSource.indexOf("export function useMDXComponents"),
);
const { slugify, extractText } = evaluate(
  `${headingFunctions}\nexport { slugify, extractText };`,
  "original-heading-functions.ts",
);
const { mdxToCleanMarkdown } = load("docs/src/lib/mdx-to-markdown.ts");
const { PAGE_TITLES } = load("docs/src/lib/page-titles.ts");
const { pageMetadata } = load("docs/src/lib/page-metadata.ts");
const { navigation } = load("docs/src/lib/docs-navigation.ts");
const rootPath = "docs/src/app/layout.tsx";
const rootMetadata = evaluate(
  `export const value = ${initializer(original(rootPath), "metadata", rootPath)};`,
  rootPath,
).value;
const parser = createProcessor();
const normalize = (value) => value.replace(/\s+/g, " ").trim();
function reactChildren(node) {
  if (["text", "inlineCode"].includes(node.type)) return node.value;
  if (node.type === "break") return "\n";
  if (node.type === "mdxTextExpression") {
    if (node.value.trim() === "") return "";
    const expression = node.data?.estree?.body?.[0]?.expression;
    assert.equal(
      expression?.type,
      "Literal",
      "Only literal MDX expressions can be captured",
    );
    return expression.value;
  }
  return { props: { children: (node.children ?? []).map(reactChildren) } };
}
const nodeText = (node) => extractText(reactChildren(node));
function walk(node, callback) {
  callback(node);
  for (const child of node.children ?? []) walk(child, callback);
}
function containsTable(node) {
  return (
    ["table", "thead", "tbody", "tr", "td", "th"].includes(node.name) ||
    (node.children ?? []).some(containsTable)
  );
}
const files = git("ls-tree", "-r", "--name-only", commit, "docs/src/app")
  .toString()
  .trim()
  .split("\n")
  .filter((path) => path.endsWith("/page.mdx"))
  .sort();
assert.equal(
  files.length,
  38,
  "Pinned commit must contain exactly 38 public pages",
);
const pages = files.map((source) => {
  const raw = original(source);
  const path =
    source.slice("docs/src/app".length).replace(/\/page\.mdx$/, "") || "/";
  const slug = path === "/" ? "" : path.slice(1);
  assert.ok(Object.hasOwn(PAGE_TITLES, slug), source);
  const layout =
    path === "/" ? rootPath : source.replace(/page\.mdx$/, "layout.tsx");
  if (path !== "/") {
    const call = initializer(original(layout), "metadata", layout);
    assert.equal(call, `pageMetadata(${JSON.stringify(slug)})`, layout);
  }
  const metadata = path === "/" ? rootMetadata : pageMetadata(slug);
  const tree = parser.parse(raw);
  const headings = [];
  const content = [];
  const links = [];
  const edits = [];
  let h1;
  walk(tree, (node) => {
    if (node.type === "heading") {
      const text = nodeText(node);
      headings.push({
        level: node.depth,
        text,
        id: node.depth <= 3 ? slugify(text) : null,
      });
      if (node.depth === 1) {
        assert.equal(h1, undefined, `${source}: multiple H1s`);
        h1 = node;
      }
    }
    if (
      (node.type === "paragraph" && !containsTable(node)) ||
      (node.type.startsWith("mdxJsx") && ["td", "th", "p"].includes(node.name))
    ) {
      const text = normalize(nodeText(node));
      if (text) content.push(text);
    }
    if (node.type === "code") content.push(normalize(node.value));
    if (["link", "image"].includes(node.type))
      links.push({
        type: node.type,
        url: node.url,
        text: node.alt ?? nodeText(node),
      });
    if (
      node.type === "mdxjsEsm" ||
      (node.type.startsWith("mdxJsx") && node.name === "DiffDemo")
    ) {
      const end = node.position.end.offset;
      edits.push([
        node.position.start.offset,
        end + (raw[end] === "\n" ? 1 : 0),
        "",
      ]);
    }
    for (const attr of node.attributes ?? []) {
      if (attr.name === "className")
        edits.push([
          attr.position.start.offset - 1,
          attr.position.end.offset,
          "",
        ]);
    }
  });
  assert.ok(h1, `${source}: missing H1`);
  let modernMarkdown = raw;
  for (const [start, end, replacement] of edits.sort((a, b) => b[0] - a[0])) {
    modernMarkdown =
      modernMarkdown.slice(0, start) + replacement + modernMarkdown.slice(end);
  }
  const title = nodeText(h1);
  const og = metadata.openGraph;
  const nav = navigation
    .flatMap((section) =>
      section.items.map((item) => ({ ...item, section: section.title ?? "" })),
    )
    .find((item) => item.href === path);
  assert.ok(nav, `${source}: navigation entry`);
  return {
    path,
    source,
    layout,
    sourceSha256: hash(raw),
    h1InsertionOffset: h1.position.start.offset,
    h1Source:
      raw.slice(h1.position.start.offset, h1.position.end.offset) + "\n",
    markdownTitle: title,
    title:
      path === "/"
        ? rootMetadata.title.default
        : rootMetadata.title.template.replace("%s", metadata.title),
    description: rootMetadata.description,
    ogTitle: og.title,
    ogDescription: og.description,
    ogAlt: og.images[0].alt,
    twitterTitle: metadata.twitter.title,
    headings,
    content: [...new Set(content)],
    links,
    legacyMarkdown: mdxToCleanMarkdown(raw),
    legacyMarkdownSha256: hash(mdxToCleanMarkdown(raw)),
    modernMarkdown: modernMarkdown.trim(),
    modernMarkdownSha256: hash(modernMarkdown.trim()),
    navigation: { title: nav.name, section: nav.section },
  };
});
const searchIndexPath = "docs/src/lib/search-index.ts";
const searchIndexSource = original(searchIndexPath);
const stripStart = searchIndexSource.indexOf("function stripMarkdown");
const stripEnd = searchIndexSource.indexOf("function mdxFileForSlug");
const { stripMarkdown } = evaluate(
  `${searchIndexSource.slice(stripStart, stripEnd)}\nexport { stripMarkdown };`,
  searchIndexPath,
);
const searchIndex = navigation.flatMap((section) =>
  section.items.map((item) => ({
    title: item.name,
    href: item.href,
    section: section.title ?? "",
    content: stripMarkdown(
      pages.find((page) => page.path === item.href).legacyMarkdown,
    ),
  })),
);
const searchRoutePath = "docs/src/app/api/search/route.ts";
const { GET } = evaluate(original(searchRoutePath), searchRoutePath, {
  "next/server": {
    NextResponse: { json: (body) => JSON.parse(JSON.stringify(body)) },
  },
  "@/lib/search-index": { getSearchIndex: async () => searchIndex },
});
original("docs/src/app/api/docs-markdown/route.ts");
const legacySearch = [];
for (const query of [
  null,
  "",
  "   ",
  "zzzyyy-nonexistent-123456",
  "installation",
  "snapshot",
  "  SNAPSHOT  ",
  "browser automation",
  "restoreCheckFn",
  "WebRTC",
  "ffmpeg",
]) {
  const nextUrl = new URL("https://agent-browser.dev/api/search");
  if (query !== null) nextUrl.searchParams.set("q", query);
  legacySearch.push({ query, response: await GET({ nextUrl }) });
}
const resources = git(
  "ls-tree",
  "-r",
  "--name-only",
  commit,
  "docs/public",
  "docs/src/app/favicon.ico",
)
  .toString()
  .trim()
  .split("\n")
  .map((source) => {
    const bytes = git("show", `${commit}:${source}`);
    return {
      source,
      path:
        source === "docs/src/app/favicon.ico"
          ? "/favicon.ico"
          : source.replace("docs/public", ""),
      sha256: hash(bytes),
      size: bytes.length,
    };
  });
const serialized = `${JSON.stringify(
  {
    commit,
    policy:
      "Only git objects at the pinned commit are inputs. Modern Markdown removes AST-level MDX imports, DiffDemo and presentation className attributes; fenced code is preserved. Legacy Markdown executes the original converter verbatim, including its removal of export/import lines inside fences. Heading IDs execute the original slugify/extractText without deduplication. Root metadata comes from the root layout; child metadata comes from each original layout and pageMetadata/PAGE_TITLES.",
    sources,
    pages,
    legacySearch,
    resources,
  },
  null,
  2,
)}\n`;
if (process.argv.includes("--check")) {
  assert.equal(
    await readFile(output, "utf8"),
    serialized,
    "Baseline is not reproducible from aff6125",
  );
  console.log(
    `PASS: ${pages.length} fixtures reproduce exactly from ${commit}`,
  );
} else {
  assert.deepEqual(
    process.argv.slice(2),
    [],
    "Only --check is supported; the baseline commit cannot be overridden",
  );
  await mkdir(dirname(fileURLToPath(output)), { recursive: true });
  await writeFile(output, serialized);
  console.log(
    `Captured ${pages.length} original pages and ${Object.keys(sources).length} original source files at ${commit}`,
  );
}
