import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";

export const baseline = JSON.parse(
  await readFile(
    new URL("fixtures/docs-baseline.json", import.meta.url),
    "utf8",
  ),
);
export const { pages } = baseline;
export const origin = "https://agent-browser.dev";
export const hash = (body) => createHash("sha256").update(body).digest("hex");
export const canonical = (path) => `${origin}${path === "/" ? "" : path}`;
export const markdownPath = (path) =>
  path === "/" ? "/index.md" : `${path}.md`;
export const apiPath = (path) => `/api/docs-md${path === "/" ? "" : path}`;
export const tokens = (value) =>
  (value ?? "")
    .toLowerCase()
    .split(/\s*,\s*/)
    .filter(Boolean);
export const normalize = (value) => value.replace(/\s+/g, " ").trim();
export const decode = (value) =>
  value.replace(
    /&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos|nbsp);/gi,
    (_, entity) => {
      if (entity.startsWith("#"))
        return String.fromCodePoint(
          entity[1].toLowerCase() === "x"
            ? Number.parseInt(entity.slice(2), 16)
            : Number(entity.slice(1)),
        );
      return { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " }[
        entity.toLowerCase()
      ];
    },
  );
export const text = (html) =>
  normalize(
    decode(
      html
        .replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1>/gi, "")
        .replace(/<[^>]*>/g, ""),
    ),
  );
export const attributes = (tag) =>
  Object.fromEntries(
    [...tag.matchAll(/([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g)].map(
      (match) => [match[1].toLowerCase(), decode(match[2] ?? match[3])],
    ),
  );
export const tags = (html, name) =>
  [...html.matchAll(new RegExp(`<${name}\\b[^>]*>`, "gi"))].map(([tag]) =>
    attributes(tag),
  );
export const meta = (html, key) =>
  tags(html, "meta")
    .filter((tag) => tag.name === key || tag.property === key)
    .map((tag) => tag.content);
export const renderedHeadings = (html) =>
  [...html.matchAll(/<h([1-6])\b([^>]*)>([\s\S]*?)<\/h\1>/gi)].map((match) => ({
    level: Number(match[1]),
    text: text(match[3]).replace(/\s*#$/, ""),
    id: attributes(match[2]).id ?? null,
  }));

export const absoluteDestination = (href, pagePath) =>
  new URL(href, canonical(pagePath)).href;
export function imageSourceUrls(image, pagePath) {
  assert.equal(typeof image.src, "string", "image must expose a source URL");
  const candidates = [
    image.src,
    ...(image.srcset
      ? image.srcset.split(",").map((entry) => entry.trim().split(/\s+/)[0])
      : []),
  ];
  return [
    ...new Set(
      candidates.map((candidate) => {
        const url = new URL(absoluteDestination(candidate, pagePath));
        assert.equal(
          url.origin,
          origin,
          "original local images must not move to an unrelated origin",
        );
        if (url.pathname !== "/_next/image") return url.href;
        const source = url.searchParams.get("url");
        assert.ok(
          source,
          "Next image optimizer must specify its underlying resource",
        );
        const underlying = new URL(source, origin);
        assert.equal(
          underlying.origin,
          origin,
          "optimized images must resolve to a local original resource",
        );
        assert.notEqual(
          underlying.pathname,
          "/_next/image",
          "do not recursively unwrap optimizer URLs",
        );
        return underlying.href;
      }),
    ),
  ];
}
export function assertOriginalResource(bytes, resource) {
  assert.equal(
    bytes.length,
    resource.size,
    `${resource.source}: original byte length`,
  );
  assert.equal(
    hash(bytes),
    resource.sha256,
    `${resource.source}: original SHA-256`,
  );
}
export function assertUntrackedHtml(html, page) {
  assert.deepEqual(
    tags(html, "link")
      .filter((tag) => tag.rel === "canonical")
      .map((tag) => tag.href),
    [canonical(page.path)],
  );
  assert.deepEqual(meta(html, "og:url"), [canonical(page.path)]);
  const surfaces = [
    text(html),
    JSON.stringify(tags(html, "meta")),
    JSON.stringify(tags(html, "link")),
  ];
  for (const marker of ["utm_source", "route-test"]) {
    for (const surface of surfaces)
      assert.ok(
        !surface.includes(marker),
        `${page.path}: tracking leaked into visible text, metadata or document links`,
      );
  }
}

export function get(path, options = {}) {
  assert.ok(
    process.env.DOCS_TEST_URL,
    "Run node scripts/test-routes.mjs after building docs",
  );
  return fetch(new URL(path, process.env.DOCS_TEST_URL), {
    redirect: "manual",
    signal: AbortSignal.timeout(30000),
    ...options,
    headers: {
      "user-agent": "Mozilla/5.0",
      accept: "text/html",
      ...options.headers,
    },
  });
}
export function rawGet(path, headers = {}, method = "GET") {
  const url = new URL(process.env.DOCS_TEST_URL);
  return new Promise((resolve, reject) => {
    const req = (url.protocol === "https:" ? httpsRequest : httpRequest)(
      {
        protocol: url.protocol,
        hostname: url.hostname,
        port: url.port,
        path,
        method,
        headers: {
          "user-agent": "Mozilla/5.0",
          accept: "text/html",
          ...headers,
        },
        signal: AbortSignal.timeout(30000),
      },
      (incoming) => {
        const chunks = [];
        incoming.on("data", (chunk) => chunks.push(chunk));
        incoming.once("error", reject);
        incoming.once("end", () => {
          const responseHeaders = new Headers();
          for (let i = 0; i < incoming.rawHeaders.length; i += 2)
            responseHeaders.append(
              incoming.rawHeaders[i],
              incoming.rawHeaders[i + 1],
            );
          resolve(
            new Response(method === "HEAD" ? null : Buffer.concat(chunks), {
              status: incoming.statusCode,
              headers: responseHeaders,
            }),
          );
        });
      },
    );
    req.once("error", reject);
    req.end();
  });
}
export function responseType(response, expected, status = 200) {
  assert.equal(response.status, status, response.url);
  assert.equal(
    response.headers.get("content-type")?.split(";")[0],
    expected,
    response.url,
  );
}
export function representationHeaders(response) {
  const vary = tokens(response.headers.get("vary"));
  for (const input of [
    "accept",
    "user-agent",
    "signature-agent",
    "sec-fetch-mode",
    "sec-fetch-dest",
    "rsc",
    "next-router-state-tree",
    "next-router-prefetch",
    "next-router-segment-prefetch",
    "next-url",
    "purpose",
    "sec-purpose",
  ]) {
    assert.ok(
      vary.includes(input),
      `${response.url}: Vary missing ${input}: ${vary}`,
    );
  }
  assert.ok(
    !vary.includes("*"),
    "Vary must explicitly retain every negotiation and Next input",
  );
  const cache = tokens(response.headers.get("cache-control"));
  assert.ok(
    cache.includes("private") && cache.includes("no-store"),
    `${response.url}: ${cache}`,
  );
  assert.ok(
    !cache.includes("public") &&
      !cache.some((token) => /^(s-maxage|max-age)=[1-9]/.test(token)),
  );
  for (const name of ["cdn-cache-control", "vercel-cdn-cache-control"])
    assert.equal(
      response.headers.get(name),
      "no-store",
      `${response.url}: ${name}`,
    );
  assert.equal(
    response.headers.get("set-cookie"),
    null,
    `${response.url}: no locale cookies`,
  );
}
export function indexing(response, html, missing = false) {
  const noindex = process.env.DOCS_EXPECT_NOINDEX === "1" || missing;
  const header = tokens(response.headers.get("x-robots-tag"));
  const robots = html === undefined ? [] : meta(html, "robots").flatMap(tokens);
  if (noindex) {
    assert.ok(
      header.includes("noindex"),
      `${response.url}: missing X-Robots-Tag noindex`,
    );
    if (html !== undefined)
      assert.ok(
        robots.includes("noindex"),
        `${response.url}: missing robots meta noindex`,
      );
  } else {
    assert.ok(
      ![...header, ...robots].some((value) =>
        ["noindex", "nofollow", "none"].includes(value),
      ),
      `${response.url}: unexpectedly unindexable`,
    );
  }
}
export function assertModernMarkdown(body, page) {
  const frontmatter = `---\ntitle: ${JSON.stringify(page.markdownTitle)}\ndescription: ${JSON.stringify(page.description)}\ncanonical_url: ${JSON.stringify(canonical(page.path))}\n---\n\n`;
  assert.ok(
    body.startsWith(frontmatter),
    `${page.path}: exact canonical frontmatter`,
  );
  assert.equal(
    body.slice(frontmatter.length),
    `${page.modernMarkdown}\n`,
    `${page.path}: original body, preserving fenced export/import examples`,
  );
}
