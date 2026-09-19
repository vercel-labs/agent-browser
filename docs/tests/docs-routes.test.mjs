import assert from "node:assert/strict";
import { test } from "node:test";
import {
  baseline,
  pages,
  origin,
  hash,
  canonical,
  markdownPath,
  apiPath,
  text,
  normalize,
  decode,
  tags,
  meta,
  renderedHeadings,
  get,
  rawGet,
  responseType,
  representationHeaders,
  indexing,
  assertModernMarkdown,
  absoluteDestination,
  imageSourceUrls,
  assertOriginalResource,
  assertUntrackedHtml,
} from "./helpers.mjs";

assert.ok(
  process.env.DOCS_TEST_URL,
  "Run node scripts/test-routes.mjs after a production build",
);
const noindex = process.env.DOCS_EXPECT_NOINDEX === "1";
const query = "?utm_source=route-test&value=a%2Fb&value=two";
const agent = { "user-agent": "ClaudeBot/1.0", accept: "text/markdown" };
const pageAt = (path) => pages.find((page) => page.path === path);
const legacyPath = (path) =>
  `/api/docs-markdown?path=${encodeURIComponent(path)}`;
const htmlCache = new Map();
async function htmlFor(page) {
  if (!htmlCache.has(page.path))
    htmlCache.set(
      page.path,
      (async () => {
        const response = await get(page.path);
        responseType(response, "text/html");
        representationHeaders(response);
        const html = await response.text();
        indexing(response, html);
        return html;
      })(),
    );
  return htmlCache.get(page.path);
}

for (const page of pages) {
  test(`${page.path}: original title, description, OG and Twitter metadata plus canonical links`, async () => {
    const html = await htmlFor(page);
    assert.deepEqual(
      [...html.matchAll(/<title\b[^>]*>([\s\S]*?)<\/title>/gi)].map((match) =>
        text(match[1]),
      ),
      [page.title],
    );
    assert.deepEqual(meta(html, "description"), [page.description]);
    assert.deepEqual(
      tags(html, "html").map((tag) => tag.lang),
      ["en"],
    );
    for (const [key, expected] of Object.entries({
      "og:title": page.ogTitle,
      "og:description": page.ogDescription,
      "og:url": canonical(page.path),
      "og:type": "website",
      "og:site_name": "agent-browser",
      "og:locale": "en_US",
      "og:image": `${origin}/og${page.path === "/" ? "" : page.path}`,
      "og:image:width": "1200",
      "og:image:height": "630",
      "og:image:alt": page.ogAlt,
      "twitter:card": "summary_large_image",
      "twitter:title": page.twitterTitle,
      "twitter:description": page.description,
      "twitter:image": `${origin}/og${page.path === "/" ? "" : page.path}`,
    }))
      assert.deepEqual(meta(html, key), [expected], `${page.path}: ${key}`);
    assert.deepEqual(
      tags(html, "link")
        .filter((tag) => tag.rel === "canonical")
        .map((tag) => tag.href),
      [canonical(page.path)],
    );
    assert.deepEqual(
      tags(html, "link")
        .filter(
          (tag) => tag.rel === "alternate" && tag.type === "text/markdown",
        )
        .map((tag) => tag.href),
      [`${origin}${markdownPath(page.path)}`],
    );
  });

  test(`${page.path}: original prose, every table cell, code example and link are server rendered`, async () => {
    const html = await htmlFor(page);
    assert.equal(tags(html, "main").length, 1);
    const main = html.match(/<main\b[^>]*>([\s\S]*?)<\/main>/i)?.[1];
    assert.ok(
      main,
      "content must be in main, not only serialized in Flight scripts",
    );
    const visible = text(main);
    for (const sample of page.content)
      assert.ok(
        visible.includes(normalize(sample)),
        `${page.path}: SSR lost ${JSON.stringify(sample)}`,
      );
    const hrefs = new Set(
      tags(main, "a")
        .filter((tag) => typeof tag.href === "string")
        .map((tag) => absoluteDestination(tag.href, page.path)),
    );
    for (const link of page.links.filter((link) => link.type === "link"))
      assert.ok(
        hrefs.has(absoluteDestination(link.url, page.path)),
        `${page.path}: lost link destination ${link.url}`,
      );
    for (const image of page.links.filter((link) => link.type === "image")) {
      const rendered = tags(main, "img").filter(
        (tag) => tag.alt === image.text,
      );
      assert.equal(
        rendered.length,
        1,
        `${page.path}: expected original image alt ${image.text}`,
      );
      const resource = baseline.resources.find(
        (entry) =>
          absoluteDestination(entry.path, page.path) ===
          absoluteDestination(image.url, page.path),
      );
      assert.ok(
        resource,
        `${page.path}: missing original resource fixture for ${image.url}`,
      );
      for (const source of imageSourceUrls(rendered[0], page.path)) {
        const url = new URL(source);
        const response = await get(`${url.pathname}${url.search}`, {
          headers: agent,
        });
        assert.equal(response.status, 200, source);
        assert.match(response.headers.get("content-type") ?? "", /^image\//);
        assertOriginalResource(
          Buffer.from(await response.arrayBuffer()),
          resource,
        );
      }
    }
  });

  test(`${page.path}: original heading text, nested levels and duplicate anchor multiplicity survive`, async () => {
    const html = await htmlFor(page);
    const headings = renderedHeadings(html);
    assert.deepEqual(
      headings
        .filter((heading) => heading.level === 1)
        .map((heading) => heading.text),
      [page.markdownTitle],
    );
    const ids = [...html.matchAll(/<[a-z][^>]*\sid="([^"]*)"[^>]*>/gi)].map(
      (match) => decode(match[1]),
    );
    for (const id of new Set(
      page.headings.map((heading) => heading.id).filter(Boolean),
    )) {
      const expected = page.headings.filter((heading) => heading.id === id);
      assert.equal(
        ids.filter((value) => value === id).length,
        expected.length,
        `${page.path}: #${id} preserves original duplicate count`,
      );
      assert.deepEqual(
        headings.filter((heading) => heading.id === id && heading.level > 1),
        expected.filter((heading) => heading.level > 1),
        `${page.path}: nested heading #${id}`,
      );
    }
    const expectedBody = page.headings.filter((heading) => heading.level > 1);
    const expectedIds = new Set(expectedBody.map((heading) => heading.id));
    assert.deepEqual(
      headings.filter(
        (heading) => heading.level > 1 && expectedIds.has(heading.id),
      ),
      expectedBody,
      `${page.path}: heading order and nesting`,
    );
  });

  test(`${page.path}: API, .md and negotiated Markdown preserve the complete original content`, async () => {
    const bodies = [];
    for (const [path, headers] of [
      [apiPath(page.path), {}],
      [markdownPath(page.path), {}],
      [page.path, { accept: "text/markdown" }],
    ]) {
      const response = await get(path, { headers });
      responseType(response, "text/markdown");
      representationHeaders(response);
      indexing(response);
      assert.equal(
        response.headers.get("link"),
        `<${canonical(page.path)}>; rel="canonical"`,
      );
      const body = await response.text();
      assertModernMarkdown(body, page);
      bodies.push(body);
    }
    assert.equal(bodies[0], bodies[1]);
    assert.equal(bodies[1], bodies[2]);
  });

  test(`${page.path}: legacy ?path= API returns exactly the old bare Markdown, not new frontmatter`, async () => {
    const response = await get(legacyPath(page.path));
    responseType(response, "text/markdown");
    representationHeaders(response);
    indexing(response);
    const body = await response.text();
    assert.equal(
      hash(body),
      page.legacyMarkdownSha256,
      `${page.path}: legacy mdxToCleanMarkdown bytes`,
    );
    assert.equal(body, page.legacyMarkdown);
  });

  test(`${page.path}: HEAD preserves all five representations without a response body`, async () => {
    for (const [path, headers, type] of [
      [page.path, {}, "text/html"],
      [apiPath(page.path), {}, "text/markdown"],
      [markdownPath(page.path), {}, "text/markdown"],
      [page.path, { accept: "text/markdown" }, "text/markdown"],
      [legacyPath(page.path), {}, "text/markdown"],
    ]) {
      const response = await get(path, { method: "HEAD", headers });
      responseType(response, type);
      representationHeaders(response);
      indexing(response);
      assert.equal(await response.text(), "");
      if (type === "text/markdown" && !path.startsWith("/api/docs-markdown?"))
        assert.equal(
          response.headers.get("link"),
          `<${canonical(page.path)}>; rel="canonical"`,
        );
    }
  });
}

for (const [name, headers, type] of [
  [
    "q=0 excludes Markdown",
    { accept: "text/markdown;q=0, text/html" },
    "text/html",
  ],
  [
    "HTML has higher quality",
    { accept: "text/markdown;q=0.5, text/html;q=1" },
    "text/html",
  ],
  [
    "Markdown has higher quality",
    { accept: "text/html;q=0.5, text/markdown;q=1" },
    "text/markdown",
  ],
  ["ordinary wildcard", { accept: "*/*" }, "text/html"],
  ...[
    "Slackbot-LinkExpanding 1.0",
    "Discordbot/2.0",
    "Googlebot/2.1",
    "Twitterbot/1.0",
  ].map((ua) => [ua, { "user-agent": ua, accept: "*/*" }, "text/html"]),
  [
    "ClaudeBot",
    { "user-agent": "ClaudeBot/1.0", accept: "*/*" },
    "text/markdown",
  ],
  [
    "browser navigation beats agent detection",
    {
      "user-agent": "ClaudeBot/1.0",
      accept: "*/*",
      "sec-fetch-mode": "navigate",
      "sec-fetch-dest": "document",
    },
    "text/html",
  ],
  [
    "explicit Markdown still works during navigation",
    { ...agent, "sec-fetch-mode": "navigate", "sec-fetch-dest": "document" },
    "text/markdown",
  ],
  ...[
    "next-router-prefetch",
    "next-router-segment-prefetch",
    "purpose",
    "sec-purpose",
  ].map((header) => [
    `${header} bypasses Markdown`,
    {
      ...agent,
      [header]:
        header === "next-router-segment-prefetch"
          ? "/_tree"
          : header.startsWith("next-")
            ? "1"
            : "prefetch",
    },
    "text/html",
  ]),
  ["agent RSC stays Flight", { ...agent, rsc: "1" }, "text/x-component"],
  [
    "ordinary RSC stays Flight",
    { accept: "text/html", rsc: "1" },
    "text/x-component",
  ],
]) {
  test(`negotiation: ${name}`, async () => {
    const path = type === "text/x-component" ? "/commands?_rsc" : "/commands";
    const response = await rawGet(path, headers);
    responseType(response, type);
    representationHeaders(response);
    indexing(response);
    const body = await response.text();
    if (type === "text/html") {
      assert.equal(tags(body, "html").length, 1);
      assert.deepEqual(meta(body, "og:title"), [pageAt("/commands").ogTitle]);
    }
    if (type === "text/markdown")
      assertModernMarkdown(body, pageAt("/commands"));
    if (type === "text/x-component") {
      assert.ok(body.length > 0);
      assert.ok(!body.startsWith("---\n") && !body.includes("<!DOCTYPE html>"));
      assert.match(body, /(?:^|\n)[0-9a-f]+:/i);
    }
  });
}

test("invalid RSC cachebusters redirect to the valid Flight URL without changing representation", async () => {
  for (const headers of [
    { ...agent, rsc: "1" },
    { accept: "text/html", rsc: "1" },
  ]) {
    const response = await rawGet("/commands?_rsc=regression", headers);
    assert.equal(response.status, 307);
    representationHeaders(response);
    indexing(response);
    assert.equal(response.headers.get("location"), "/commands?_rsc");
    assert.equal(await response.text(), "");
    const destination = await rawGet(response.headers.get("location"), headers);
    responseType(destination, "text/x-component");
    representationHeaders(destination);
    indexing(destination);
    const body = await destination.text();
    assert.match(body, /(?:^|\n)[0-9a-f]+:/i);
    assert.ok(!body.startsWith("---\n") && !body.includes("<!DOCTYPE html>"));
  }
});

test("alternating representations and queries never contaminate one another", async () => {
  for (const path of ["/", "/commands", "/providers/browserbase"]) {
    const page = pageAt(path);
    const untracked = await htmlFor(page);
    for (const accept of [
      "text/markdown",
      "text/html",
      "text/markdown",
      "text/html",
    ]) {
      const response = await get(`${path}${query}`, { headers: { accept } });
      responseType(response, accept);
      representationHeaders(response);
      const body = await response.text();
      if (accept === "text/markdown") {
        assertModernMarkdown(body, page);
        assert.ok(
          !body.includes("utm_source") && !body.includes("route-test"),
          "tracking must not leak into Markdown",
        );
      } else {
        assertUntrackedHtml(body, page);
        assert.equal(
          text(body),
          text(untracked),
          `${path}: queries must not change visible page content`,
        );
        assert.deepEqual(
          tags(body, "meta"),
          tags(untracked, "meta"),
          `${path}: queries must not change metadata`,
        );
        assert.deepEqual(
          tags(body, "link"),
          tags(untracked, "link"),
          `${path}: queries must not change document links`,
        );
      }
    }
  }
});

for (const page of pages) {
  test(`${page.path}: locale alias is a permanent query-preserving, cookie-free redirect`, async () => {
    for (const suffix of ["", ".md"]) {
      const target = suffix ? markdownPath(page.path) : page.path;
      const path = suffix
        ? `/en${target}`
        : `/en${page.path === "/" ? "" : page.path}`;
      const response = await get(`${path}${query}`);
      assert.equal(response.status, 308, path);
      representationHeaders(response);
      const location = new URL(
        response.headers.get("location"),
        process.env.DOCS_TEST_URL,
      );
      assert.equal(location.origin, new URL(process.env.DOCS_TEST_URL).origin);
      assert.equal(location.pathname, target);
      assert.equal(location.search, query);
      assert.equal(location.hash, "");
      await response.body?.cancel();
    }
  });
}

test("trailing slashes keep permanent public paths and query strings", async () => {
  for (const page of pages.filter((page) => page.path !== "/")) {
    const response = await get(`${page.path}/${query}`);
    assert.equal(response.status, 308, page.path);
    assert.equal(response.headers.get("set-cookie"), null);
    const location = new URL(
      response.headers.get("location"),
      process.env.DOCS_TEST_URL,
    );
    assert.equal(location.pathname, page.path);
    assert.equal(location.search, query);
    await response.body?.cancel();
  }
});

for (const path of [
  "/missing-page",
  "/docs",
  "/docs/commands",
  "/fr/commands",
  "/providers/missing",
  "/engines/missing",
]) {
  for (const [name, url, headers, type] of [
    ["HTML", path, {}, "text/html"],
    ["explicit Markdown", `${path}.md`, {}, "text/markdown"],
    ["negotiated Markdown", path, { accept: "text/markdown" }, "text/markdown"],
    ["API Markdown", `/api/docs-md${path}`, {}, "text/markdown"],
  ]) {
    test(`404 ${name}: ${url}`, async () => {
      const response = await get(url, { headers });
      responseType(response, type, 404);
      representationHeaders(response);
      const body = await response.text();
      indexing(response, type === "text/html" ? body : undefined, true);
      if (type === "text/markdown") {
        assert.match(body, /not found/i);
        assert.ok(body.includes("/sitemap.md") && body.includes("/llms.txt"));
      } else assert.match(text(body), /not found/i);
      const head = await get(url, { method: "HEAD", headers });
      responseType(head, type, 404);
      representationHeaders(head);
      indexing(head, undefined, true);
      assert.equal(await head.text(), "");
    });
  }
}

for (const path of [
  "/%",
  "/%25",
  "/%2F",
  "/%5C",
  "/commands%2Fmissing",
  "/%252F",
  "/%E0%A4%A",
  "/%C0%AF",
  "/%00",
  "/%7F",
]) {
  for (const [name, url, headers] of [
    ["HTML", path, {}],
    ["explicit Markdown", `${path}.md`, {}],
    ["negotiated Markdown", path, { accept: "text/markdown" }],
    ["API Markdown", `/api/docs-md${path}`, {}],
  ]) {
    test(`malformed path is a real 404, never a decoding 500: ${name} ${url}`, async () => {
      const response = await rawGet(url, headers);
      assert.equal(response.status, 404, url);
      representationHeaders(response);
      indexing(response, undefined, true);
      assert.ok(!response.headers.has("location"));
      const body = await response.text();
      assert.ok(!/URIError|Internal Server Error/.test(body));
    });
  }
}

test("legacy Markdown handles required, missing, unknown and normalized path parameters", async () => {
  for (const path of ["/api/docs-markdown", "/api/docs-markdown?path="]) {
    const response = await get(path);
    responseType(response, "application/json", 400);
    assert.deepEqual(await response.json(), {
      error: "Missing ?path= parameter",
    });
  }
  for (const path of ["/not-a-page", "/commands.md", "/fr/commands"]) {
    const response = await get(legacyPath(path));
    responseType(response, "application/json", 404);
    assert.deepEqual(await response.json(), { error: "Page not found" });
  }
  for (const path of [
    "commands",
    "/commands/",
    "providers/browserbase",
    "/providers/browserbase/",
  ]) {
    const response = await get(legacyPath(path));
    responseType(response, "text/markdown");
    const canonicalPath = `/${path.replace(/^\//, "").replace(/\/$/, "")}`;
    assert.equal(
      await response.text(),
      pageAt(canonicalPath).legacyMarkdown,
      path,
    );
  }
  const duplicate = await get(
    "/api/docs-markdown?path=%2Fcommands&path=%2Fmissing",
  );
  responseType(duplicate, "text/markdown");
  assert.equal(
    await duplicate.text(),
    pageAt("/commands").legacyMarkdown,
    "first path parameter wins, matching original URLSearchParams.get",
  );
});

test("legacy Markdown never resolves traversal or malformed path values to unrelated pages", async () => {
  for (const path of [
    "/../commands",
    "/commands/../../package",
    "/commands\\missing",
    "/commands\u0000",
    "/%2f",
    "/%",
    "/%E0%A4%A",
  ]) {
    const response = await get(legacyPath(path));
    assert.ok(
      [400, 404].includes(response.status),
      `${path}: must reject unsafe input, got ${response.status}`,
    );
    assert.equal(
      response.headers.get("content-type")?.split(";")[0],
      "application/json",
    );
    const body = await response.json();
    assert.equal(typeof body.error, "string");
    assert.ok(body.error.length > 0);
  }
});

for (const fixture of baseline.legacySearch) {
  test(`legacy search ?q=${JSON.stringify(fixture.query)} retains original object, ranking and snippet shape`, async () => {
    const suffix =
      fixture.query === null ? "" : `?q=${encodeURIComponent(fixture.query)}`;
    const response = await get(`/api/search${suffix}`);
    responseType(response, "application/json");
    const body = await response.json();
    assert.deepEqual(body, fixture.response);
    assert.deepEqual(Object.keys(body), ["results"]);
    for (const result of body.results)
      assert.deepEqual(Object.keys(result).sort(), [
        "href",
        "section",
        "snippet",
        "title",
      ]);
  });
}

test("WebMCP search results use Features without changing the public URL", async () => {
  const legacy = await get("/api/search?q=webmcp");
  responseType(legacy, "application/json");
  const { results } = await legacy.json();
  const matches = results.filter((result) => result.href === "/webmcp");
  assert.equal(matches.length, 1);
  assert.equal(matches[0].section, "Features");
  const native = await get("/api/search?query=webmcp&locale=en");
  responseType(native, "application/json");
  const nativeMatches = (await native.json()).filter(
    (result) => result.type === "page" && result.url === "/webmcp",
  );
  assert.equal(nativeMatches.length, 1);
  assert.ok(nativeMatches[0].breadcrumbs.includes("Features"));
  assert.ok(!nativeMatches[0].breadcrumbs.includes("Reference"));
});

for (const probe of [
  {
    query: "restoreCheckFn",
    path: "/configuration",
    fragment: "all-options",
    text: "restoreCheckFn",
    kind: "HTML table code cell",
  },
  {
    query: "libx264",
    path: "/recording",
    fragment: "requirements",
    text: "libx264",
    kind: "nested inline code in prose",
  },
  {
    query: "unintended visual changes",
    path: "/diffing",
    fragment: "visual-regression-testing",
    text: "unintended visual changes",
    kind: "nested H3 section",
  },
]) {
  test(`native search indexes ${probe.kind} with addressable content fragments`, async () => {
    const page = pageAt(probe.path);
    assert.ok(
      page.modernMarkdown.includes(probe.text),
      "query evidence must exist in original source",
    );
    assert.ok(
      page.headings.some((heading) => heading.id === probe.fragment),
      "fragment must come from the original slugger",
    );
    const response = await get(
      `/api/search?query=${encodeURIComponent(probe.query)}&locale=en`,
    );
    responseType(response, "application/json");
    const results = await response.json();
    assert.ok(
      Array.isArray(results),
      "native query= contract is a Fumadocs result array",
    );
    assert.ok(
      results.some(
        (result) =>
          result.type === "text" &&
          result.url === `${probe.path}#${probe.fragment}` &&
          result.content.includes(probe.text),
      ),
      JSON.stringify(results),
    );
    for (const result of results) {
      assert.equal(typeof result.id, "string");
      assert.equal(typeof result.content, "string");
      assert.ok(["page", "heading", "text"].includes(result.type));
      const target = new URL(result.url, origin);
      assert.equal(target.origin, origin);
      const destination = pageAt(target.pathname);
      assert.ok(destination, `search leaked non-public route ${result.url}`);
      if (target.hash) {
        const html = await htmlFor(destination);
        assert.ok(
          [...html.matchAll(/\sid="([^"]*)"/g)].some(
            (match) =>
              decode(match[1]) === decodeURIComponent(target.hash.slice(1)),
          ),
          `search returned missing fragment ${result.url}`,
        );
      }
    }
  });
}

test("native search empty and unmatched queries return arrays, not legacy objects", async () => {
  for (const query of ["", "zzzyyy-nonexistent-123456"]) {
    const response = await get(
      `/api/search?query=${encodeURIComponent(query)}&locale=en`,
    );
    responseType(response, "application/json");
    assert.deepEqual(await response.json(), []);
  }
});

test("sitemap contains exactly the 38 canonical production URLs", async () => {
  const response = await get("/sitemap.xml", { headers: agent });
  responseType(response, "application/xml");
  indexing(response);
  const xml = await response.text();
  assert.match(xml, /<urlset\b/);
  const locations = [...xml.matchAll(/<loc>([^<]+)<\/loc>/g)].map((match) =>
    decode(match[1]),
  );
  assert.deepEqual(
    locations.sort(),
    pages.map((page) => canonical(page.path)).sort(),
  );
  assert.ok(
    !xml.includes("/en/") &&
      !xml.includes("localhost") &&
      !xml.includes("127.0.0.1"),
  );
});

for (const path of ["/llms.txt", "/sitemap.md"]) {
  test(`${path}: agent index exposes all 38 public pages and Markdown alternatives exactly once`, async () => {
    const response = await get(path, { headers: agent });
    responseType(
      response,
      path.endsWith(".md") ? "text/markdown" : "text/plain",
    );
    indexing(response);
    const body = await response.text();
    const links = [...body.matchAll(/^- \[([^\]]+)\]\(([^)]+)\)/gm)].map(
      (match) => [match[1], match[2]],
    );
    assert.deepEqual(
      links.sort((a, b) => a[1].localeCompare(b[1])),
      pages
        .map((page) => [page.markdownTitle, canonical(page.path)])
        .sort((a, b) => a[1].localeCompare(b[1])),
    );
    const markdownLinks = [...body.matchAll(/\[Markdown\]\(([^)]+)\)/g)].map(
      (match) => match[1],
    );
    assert.deepEqual(
      markdownLinks.sort(),
      pages.map((page) => `${origin}${markdownPath(page.path)}`).sort(),
    );
    assert.ok(
      !body.includes("/en/") &&
        !body.includes("localhost") &&
        !body.includes("127.0.0.1"),
    );
    const head = await get(path, { method: "HEAD", headers: agent });
    responseType(head, path.endsWith(".md") ? "text/markdown" : "text/plain");
    indexing(head);
    assert.equal(await head.text(), "");
  });
}

test("robots is environment-aware without losing the production sitemap", async () => {
  const response = await get("/robots.txt", { headers: agent });
  responseType(response, "text/plain");
  indexing(response);
  const directives = (await response.text())
    .split(/\r?\n/)
    .map((line) => line.trim().toLowerCase())
    .filter(Boolean);
  assert.ok(directives.includes("user-agent: *"));
  assert.ok(directives.includes(`sitemap: ${origin}/sitemap.xml`));
  assert.ok(directives.includes(noindex ? "disallow: /" : "allow: /"));
  if (!noindex)
    assert.ok(!directives.some((line) => /^disallow:\s*\S/.test(line)));
});

for (const resource of baseline.resources) {
  test(`${resource.path}: original static resource bytes bypass locale and Markdown routing`, async () => {
    const response = await get(resource.path, { headers: agent });
    assert.equal(response.status, 200);
    indexing(response);
    const bytes = Buffer.from(await response.arrayBuffer());
    assert.equal(bytes.length, resource.size);
    assert.equal(hash(bytes), resource.sha256, resource.source);
    assert.doesNotMatch(
      response.headers.get("content-type") ?? "",
      /text\/(html|markdown)/,
    );
    if (resource.path.endsWith(".png"))
      assert.equal(
        response.headers.get("content-type")?.split(";")[0],
        "image/png",
      );
    if (resource.path.endsWith(".json"))
      assert.equal(
        response.headers.get("content-type")?.split(";")[0],
        "application/json",
      );
    const head = await get(resource.path, { method: "HEAD", headers: agent });
    assert.equal(head.status, 200);
    assert.equal(await head.text(), "");
  });
}

for (const page of pages) {
  test(`${page.path}: original OG endpoint returns an actual 1200x630 PNG for agents`, async () => {
    const response = await get(`/og${page.path === "/" ? "" : page.path}`, {
      headers: agent,
    });
    responseType(response, "image/png");
    indexing(response);
    const png = Buffer.from(await response.arrayBuffer());
    assert.deepEqual(
      [...png.subarray(0, 8)],
      [137, 80, 78, 71, 13, 10, 26, 10],
    );
    assert.equal(png.toString("ascii", 12, 16), "IHDR");
    assert.equal(png.readUInt32BE(16), 1200);
    assert.equal(png.readUInt32BE(20), 630);
  });
}

test("unknown OG routes retain the original JSON 404", async () => {
  const response = await get("/og/missing-page", { headers: agent });
  responseType(response, "application/json", 404);
  assert.deepEqual(await response.json(), { error: "Not found" });
});

test("Next static CSS, JavaScript and fonts remain binary/static resources for agents", async () => {
  const response = await get("/");
  responseType(response, "text/html");
  const html = await response.text();
  const css = tags(html, "link").find(
    (tag) => tag.rel === "stylesheet" && tag.href.startsWith("/_next/"),
  )?.href;
  const js = tags(html, "script").find((tag) =>
    tag.src?.startsWith("/_next/"),
  )?.src;
  const font =
    tags(html, "link").find(
      (tag) => tag.as === "font" && tag.href.startsWith("/_next/"),
    )?.href ?? /<([^>]+\.woff2)>/.exec(response.headers.get("link") ?? "")?.[1];
  for (const [url, type] of [
    [css, /^text\/css/],
    [js, /^(application|text)\/javascript/],
    [font, /^(font\/|application\/(font|octet-stream))/],
  ]) {
    assert.ok(
      url?.startsWith("/_next/"),
      "SSR must advertise a bundled resource",
    );
    const resource = await get(url, { headers: agent });
    assert.equal(resource.status, 200);
    assert.match(resource.headers.get("content-type") ?? "", type);
    assert.ok((await resource.arrayBuffer()).byteLength > 0);
    assert.equal(resource.headers.get("set-cookie"), null);
  }
});
