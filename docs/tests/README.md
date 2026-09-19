# Agent-browser Geistdocs regression suite

## Fixture provenance

`fixtures/docs-baseline.json` is pinned to `aff6125c023b810ea3f2e5deec5379e9a4270bdc`. The capture script reads git objects at that commit, not migrated application code or content. It preserves all 38 original `docs/src/app/**/page.mdx` files, their layouts, root metadata, `mdx-components.tsx`, `page-metadata.ts`, `page-titles.ts`, navigation, search implementation, and `mdx-to-markdown.ts`: 84 complete source files with SHA-256 hashes. Public resources and favicon are pinned by byte length and SHA-256.

Each page records its complete source hash, original H1 insertion position, metadata, heading levels/text/IDs, prose paragraphs, table cells, code examples, links, legacy Markdown and new Markdown derived from the original source. The H1 insertion position preserves imports before the title, as on `/diffing`.

The generator executes the original slugger and recursive React-child text extractor without substituting GitHub slugging or deduplicating IDs. The independent oracle compiles and renders the captured original MDX with the original heading components, checking every captured heading and content sample. Synthetic cases exercise nested inline children, numeric text, punctuation and duplicate IDs. Fenced code is not parsed as headings.

Never regenerate fixtures from migrated output to accept a failing assertion. To verify reproducibility from the pinned commit:

```sh
node scripts/capture-docs-baseline.mjs --check
```

To reproduce the same frozen baseline explicitly:

```sh
node scripts/capture-docs-baseline.mjs
node scripts/capture-docs-baseline.mjs --check
```

## Running tests

Run from `docs/`:

```sh
node --test tests/docs-baseline.test.mjs tests/baseline-oracle.test.mjs
node scripts/test-routes.mjs
```

The route runner requires a completed production build. It starts an isolated `next start` process on an ephemeral loopback port, waits for readiness, runs the Node tests, propagates failures, and cleans up its child processes. It does not build or start a development server. Its lifecycle follows the portless and json-render reference suites.

Production and preview checks need matching build environments, including statically generated metadata and robots:

```sh
VERCEL_ENV=production pnpm run build
node scripts/test-routes.mjs
VERCEL_ENV=preview pnpm run build
node scripts/test-routes.mjs --preview
```

The runner explicitly sets `VERCEL_ENV` and `DOCS_EXPECT_NOINDEX`. Preview requires `noindex` in HTML metadata and response headers and `Disallow: /` in robots. Reusing a production build can expose metadata baked into the wrong environment.

To test an already running production server without starting or stopping any server:

```sh
DOCS_TEST_URL=http://127.0.0.1:3000 DOCS_EXPECT_NOINDEX=0 node --test tests/docs-routes.test.mjs
```

Set `DOCS_EXPECT_NOINDEX=1` when testing an existing preview server. The capture script and original-render oracle use `typescript`, `@mdx-js/mdx`, `react`, and `react-dom`; the HTTP suite uses Node built-ins.

## Regression invariants

- Exactly 38 public pages, byte-for-byte reconstruction of original MDX, SSR prose/table/code content, original heading levels/order/anchor multiplicity, metadata, canonical URLs and Markdown alternates.
- Every page's new Markdown API, `.md`, negotiated Markdown, legacy query API and HEAD responses. Markdown content is checked in full, not only by headings or representative substrings.
- Link destinations are resolved against the canonical page URL before comparison. Absolute and relative references to the same resource are equivalent; changes to origin, path, query or fragment are not.
- Images retain original alt text. Direct sources and every responsive `srcset` candidate are resolved through Next's image optimizer to their underlying same-origin resources. Those bytes must match the original resource fixture's length and SHA-256, including when static imports generate hashed filenames. Alt text or filenames alone cannot establish image parity.
- Accept quality/exclusion, browsers, AI bots, preview/search bots, unmodified Sec-Fetch headers, RSC, both Next prefetch headers, Purpose and Sec-Purpose.
- RSC requests use the valid empty cachebuster `/commands?_rsc`. A separate test checks Next's 307 normalization of an invalid cachebuster, then verifies Flight at the destination. Redirects are not accepted as successful Flight responses.
- Explicit Vary tokens for Accept, User-Agent, Signature-Agent, Sec-Fetch-Mode, Sec-Fetch-Dest, RSC, Next-Router-State-Tree, Next-Router-Prefetch, Next-Router-Segment-Prefetch, Next-Url, Purpose and Sec-Purpose; private/no-store and both CDN no-store headers; no locale cookies.
- Alternating HTML/Markdown requests cannot contaminate one another. Tracking queries must not change visible text, metadata, document links or Markdown. Next's Flight hydration scripts may serialize the request URL; their presence is not content or canonical leakage. Mutation tests demonstrate that visible, canonical and OG leakage still fails.
- Permanent locale and trailing-slash redirects preserve request origin and queries. HTML/Markdown/API misses return real 404s, including HEAD. Raw malformed encoding, encoded separators/control characters, double encoding and unsafe legacy path values are rejected.
- Legacy search shape, ranking, snippets and empty-query behavior; native search arrays containing table text and nested-section fragments that resolve to SSR anchors.
- Exact canonical sitemap/index inventories, environment-aware robots, original static resource hashes, all 38 OG PNG endpoints with signature and 1200x630 dimensions, and Next CSS/JS/font routing.

## Compatibility contracts

1. Legacy `/api/search?q=...` returns `{ "results": [...] }`. Entries have `title`, `href`, `section` and `snippet`, with at most 20 ranked results. Absent, empty and whitespace-only legacy queries return `{ "results": [] }`. Native `?query=...&locale=en` returns Fumadocs result arrays.
2. Legacy `/api/docs-markdown?path=...` returns bare, trimmed output from the original converter, without YAML frontmatter or a trailing newline. It accepts `commands`, `/commands` and trailing slashes. The first repeated `path` parameter wins. Missing/empty parameters return JSON 400 with `Missing ?path= parameter`; unknown paths return JSON 404 with `Page not found`.
3. The original converter removes lines beginning with `export ` or `import ` even inside code fences and retains `<DiffDemo />`. Legacy fixtures preserve those historical bytes. New Markdown is a separate, lossless contract: original MDX with only AST-level imports, the non-textual DiffDemo component and presentation `className` attributes removed. Fenced imports/exports remain intact. These endpoints must not be made artificially equal by changing fixtures.
4. The original slugger is stateless. `/diffing` has three `#options` and two `#output` headings; `/changelog` has 53 `#bug-fixes` headings; `/streaming` has two `#streaming` targets across its H1 and H2. Nested headings must not silently acquire numeric suffixes. An H1 may use a separate addressable alias when Geistdocs renders the title, but original ID multiplicity remains required.
5. The homepage H1 is `agent-browser`; its document/OG/Twitter title is `agent-browser | Browser Automation for AI`. The root `PAGE_TITLES` entry is for OG rendering, not the document title.
6. Unsafe legacy traversal must return JSON 400 or 404 rather than reproducing the old sanitizer's ability to turn traversal into unrelated valid pages. Successful content is never accepted for these inputs.
