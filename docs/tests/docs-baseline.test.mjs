import assert from "node:assert/strict";
import { access, readFile, readdir } from "node:fs/promises";
import { test } from "node:test";
import { navigation } from "../src/lib/docs-navigation.ts";
import {
  baseline,
  pages,
  hash,
  canonical,
  assertModernMarkdown,
  absoluteDestination,
  imageSourceUrls,
  assertOriginalResource,
  assertUntrackedHtml,
} from "./helpers.mjs";

test("deployment pins the same pnpm version as the workspace", async () => {
  const [workspace, docs] = await Promise.all([
    readFile(new URL("../../package.json", import.meta.url), "utf8").then(
      JSON.parse,
    ),
    readFile(new URL("../package.json", import.meta.url), "utf8").then(
      JSON.parse,
    ),
  ]);
  assert.match(workspace.packageManager, /^pnpm@\d+\.\d+\.\d+$/);
  assert.equal(docs.packageManager, workspace.packageManager);
});

test("deployment uses the workspace lockfile without a stale docs override", async () => {
  await access(new URL("../../pnpm-lock.yaml", import.meta.url));
  await assert.rejects(
    access(new URL("../pnpm-lock.yaml", import.meta.url)),
    { code: "ENOENT" },
    "A docs-local lockfile shadows the workspace lockfile during deployment",
  );
});

test("deployment explicitly uses Corepack for frozen installs and builds", async () => {
  const config = JSON.parse(
    await readFile(new URL("../vercel.json", import.meta.url), "utf8"),
  );
  assert.equal(
    config.installCommand,
    "corepack pnpm --filter docs install --frozen-lockfile",
  );
  assert.equal(config.buildCommand, "corepack pnpm run build");
});

test("baseline pins all 38 routes and the original metadata, slugger and converter sources", () => {
  assert.equal(baseline.commit, "aff6125c023b810ea3f2e5deec5379e9a4270bdc");
  assert.equal(pages.length, 38);
  assert.equal(new Set(pages.map((page) => page.path)).size, 38);
  for (const path of [
    "docs/mdx-components.tsx",
    "docs/src/lib/mdx-to-markdown.ts",
    "docs/src/lib/page-metadata.ts",
    "docs/src/lib/page-titles.ts",
    "docs/src/app/layout.tsx",
    "docs/src/app/api/docs-markdown/route.ts",
    "docs/src/app/api/search/route.ts",
  ])
    assert.ok(baseline.sources[path], path);
  for (const [path, source] of Object.entries(baseline.sources))
    assert.equal(hash(source.raw), source.sha256, path);
  for (const page of pages) {
    assert.equal(
      hash(baseline.sources[page.source].raw),
      page.sourceSha256,
      page.source,
    );
    assert.ok(baseline.sources[page.layout], page.layout);
    assert.equal(
      hash(page.legacyMarkdown),
      page.legacyMarkdownSha256,
      page.path,
    );
    assert.equal(
      hash(page.modernMarkdown),
      page.modernMarkdownSha256,
      page.path,
    );
    assert.equal(
      page.headings.filter((heading) => heading.level === 1).length,
      1,
      page.path,
    );
    assert.ok(page.content.length > 0, page.path);
  }
});

test("WebMCP appears once in Features after CDP Mode in both navigation sources", async () => {
  const meta = JSON.parse(
    await readFile(
      new URL("../content/docs/meta.json", import.meta.url),
      "utf8",
    ),
  );
  const reference = navigation.find((section) => section.title === "Reference");
  const features = navigation.find((section) => section.title === "Features");
  const featurePaths = features.items.map((item) => item.href);
  assert.equal(
    navigation
      .flatMap((section) => section.items)
      .filter((item) => item.href === "/webmcp").length,
    1,
  );
  assert.ok(!reference.items.some((item) => item.href === "/webmcp"));
  assert.ok(featurePaths.includes("/webmcp"));
  assert.equal(
    featurePaths.indexOf("/webmcp"),
    featurePaths.indexOf("/cdp-mode") + 1,
  );
  assert.equal(meta.pages.filter((page) => page === "webmcp").length, 1);
  assert.ok(
    meta.pages.indexOf("webmcp") > meta.pages.indexOf("---Features---"),
  );
  assert.equal(
    meta.pages.indexOf("webmcp"),
    meta.pages.indexOf("cdp-mode") + 1,
  );
  const referenceSlugs = meta.pages.slice(
    meta.pages.indexOf("---Reference---") + 1,
    meta.pages.indexOf("---Features---"),
  );
  assert.deepEqual(
    referenceSlugs,
    reference.items.map((item) => item.href.slice(1)),
  );
  assert.deepEqual(
    meta.pages.slice(
      meta.pages.indexOf("---Features---") + 1,
      meta.pages.indexOf("providers"),
    ),
    featurePaths.map((href) => href.slice(1)),
  );
});

test("heading oracle excludes fenced shell comments and preserves original duplicate IDs", () => {
  const quickStart = pages.find((page) => page.path === "/quick-start");
  assert.equal(quickStart.headings.length, 8);
  assert.ok(
    !quickStart.headings.some((heading) => heading.text.includes("@e1")),
  );
  const diffing = pages.find((page) => page.path === "/diffing");
  assert.equal(
    diffing.headings.filter((heading) => heading.id === "options").length,
    3,
  );
  assert.equal(
    diffing.headings.filter((heading) => heading.id === "output").length,
    2,
  );
  const changelog = pages.find((page) => page.path === "/changelog");
  assert.equal(
    changelog.headings.filter((heading) => heading.id === "bug-fixes").length,
    53,
  );
  assert.ok(changelog.headings.some((heading) => heading.level === 3));
  assert.equal(
    pages
      .find((page) => page.path === "/streaming")
      .headings.filter((heading) => heading.id === "streaming").length,
    2,
  );
});

test("legacy Markdown quirks are distinguished from the new lossless Markdown contract", () => {
  const browserbase = pages.find(
    (page) => page.path === "/providers/browserbase",
  );
  assert.ok(
    browserbase.modernMarkdown.includes(
      'export BROWSERBASE_API_KEY="your-api-key"',
    ),
  );
  assert.ok(
    !browserbase.legacyMarkdown.includes(
      'export BROWSERBASE_API_KEY="your-api-key"',
    ),
  );
  const diffing = pages.find((page) => page.path === "/diffing");
  assert.ok(diffing.legacyMarkdown.includes("<DiffDemo />"));
  assert.ok(!diffing.modernMarkdown.includes("<DiffDemo />"));
  const root = pages.find((page) => page.path === "/");
  assert.equal(root.markdownTitle, "agent-browser");
  assert.equal(root.title, "agent-browser | Browser Automation for AI");
});

test("Markdown oracle rejects missing tables, fenced export examples and nested sections", () => {
  for (const path of ["/configuration", "/providers/browserbase", "/diffing"]) {
    const page = pages.find((candidate) => candidate.path === path);
    const prefix = `---\ntitle: ${JSON.stringify(page.markdownTitle)}\ndescription: ${JSON.stringify(page.description)}\ncanonical_url: ${JSON.stringify(canonical(path))}\n---\n\n`;
    assertModernMarkdown(`${prefix}${page.modernMarkdown}\n`, page);
    const mutated = page.modernMarkdown.replace(
      /<table>[\s\S]*?<\/table>|^export .+$|^### .+$/m,
      "",
    );
    assert.notEqual(mutated, page.modernMarkdown);
    assert.throws(
      () => assertModernMarkdown(`${prefix}${mutated}\n`, page),
      assert.AssertionError,
    );
  }
});

test("link normalization preserves origin, path, query and fragment destinations", () => {
  const expected = absoluteDestination(
    "https://agent-browser.dev/schema.json",
    "/configuration",
  );
  assert.equal(absoluteDestination("/schema.json", "/configuration"), expected);
  assert.equal(
    absoluteDestination("../schema.json", "/providers/browserbase"),
    expected,
  );
  for (const wrong of [
    "https://example.com/schema.json",
    "/other.json",
    "/schema.json?changed=1",
    "/schema.json#changed",
  ]) {
    assert.notEqual(absoluteDestination(wrong, "/configuration"), expected);
  }
});

test("image assertions unwrap optimizer sources and inspect every responsive candidate", () => {
  const source = "/_next/static/media/contact-sheet-example.hash.png";
  const optimized = `/_next/image?url=${encodeURIComponent(source)}&w=640&q=75`;
  assert.deepEqual(
    imageSourceUrls(
      {
        src: optimized,
        srcset: `${optimized} 640w, ${optimized.replace("w=640", "w=1200")} 1200w`,
      },
      "/recording",
    ),
    [absoluteDestination(source, "/recording")],
  );
  assert.deepEqual(imageSourceUrls({ src: source }, "/recording"), [
    absoluteDestination(source, "/recording"),
  ]);
  assert.deepEqual(
    imageSourceUrls(
      { src: optimized, srcset: "/unrelated.png 1200w" },
      "/recording",
    ),
    [
      absoluteDestination(source, "/recording"),
      absoluteDestination("/unrelated.png", "/recording"),
    ],
  );
  for (const src of [
    "https://example.com/image.png",
    "/_next/image?w=640",
    "/_next/image?url=https%3A%2F%2Fexample.com%2Fimage.png",
    "/_next/image?url=%2F_next%2Fimage",
  ]) {
    assert.throws(
      () => imageSourceUrls({ src }, "/recording"),
      assert.AssertionError,
    );
  }
});

test("image byte oracle rejects altered and truncated originals even when the alt text is unchanged", async () => {
  const resource = baseline.resources.find(
    (entry) => entry.path === "/recording/contact-sheet-example.png",
  );
  assert.ok(resource);
  const bytes = await readFile(
    new URL(`../public${resource.path}`, import.meta.url),
  );
  assertOriginalResource(bytes, resource);
  const mutated = Buffer.from(bytes);
  mutated[mutated.length - 1] ^= 1;
  assert.throws(
    () => assertOriginalResource(mutated, resource),
    assert.AssertionError,
  );
  assert.throws(
    () => assertOriginalResource(bytes.subarray(1), resource),
    assert.AssertionError,
  );
});

test("tracking oracle permits Flight serialization but rejects visible, canonical and metadata leakage", () => {
  const page = pages.find((entry) => entry.path === "/commands");
  const html = `<html><head><link rel="canonical" href="${canonical(page.path)}"><meta property="og:url" content="${canonical(page.path)}"><meta property="og:title" content="Commands"></head><body><main>Original content</main><script>self.__next_f.push([1, {c: ["commands?utm_source=route-test"], q: "?utm_source=route-test"}])</script></body></html>`;
  assertUntrackedHtml(html, page);
  for (const mutated of [
    html.replace("Original content", "Original content utm_source=route-test"),
    html.replace('content="Commands"', 'content="Commands route-test"'),
    html.replace(
      `href="${canonical(page.path)}"`,
      `href="${canonical(page.path)}?utm_source=route-test"`,
    ),
    html.replace(
      `content="${canonical(page.path)}"`,
      `content="${canonical(page.path)}?utm_source=route-test"`,
    ),
  ])
    assert.throws(
      () => assertUntrackedHtml(mutated, page),
      assert.AssertionError,
    );
});

test("migrated content has exactly the 38 original public pages", async () => {
  const entries = await readdir(new URL("../content/docs/", import.meta.url), {
    recursive: true,
  });
  const mdx = entries.filter((path) => path.endsWith(".mdx")).sort();
  assert.deepEqual(
    mdx,
    pages
      .map((page) => `${page.path === "/" ? "index" : page.path.slice(1)}.mdx`)
      .sort(),
  );
});

for (const page of pages) {
  test(`${page.path}: full migrated MDX reconstructs the original git blob byte for byte`, async () => {
    const slug = page.path === "/" ? "index" : page.path.slice(1);
    const raw = await readFile(
      new URL(`../content/docs/${slug}.mdx`, import.meta.url),
      "utf8",
    );
    const frontmatter = raw.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n/);
    assert.ok(frontmatter, page.path);
    assert.ok(
      frontmatter[1]
        .split("\n")
        .includes(`title: ${JSON.stringify(page.markdownTitle)}`),
      `${page.path}: title derived from original H1`,
    );
    const body = raw.slice(frontmatter[0].length);
    const restored =
      body.slice(0, page.h1InsertionOffset) +
      page.h1Source +
      body.slice(page.h1InsertionOffset);
    assert.equal(hash(restored), page.sourceSha256, page.source);
  });
}
