# @agent-browser/eve

An [eve](https://eve.dev) extension that gives your agent a full browser automation tool set, backed by [agent-browser](https://agent-browser.dev) running inside the agent's sandbox.

Mount it once and the agent gets ~20 namespaced tools — navigation, accessibility snapshots with stable element refs, clicking, form filling, semantic element finding, waiting, screenshots, JavaScript evaluation, tab management, network and console inspection — without writing a single tool by hand.

## Install

```bash
npm install @agent-browser/eve
```

`eve` is a peer dependency; the extension runs on the eve installed in your app.

## Mount

Create a mount file under `agent/extensions/`. The file name becomes the tool namespace:

```ts
// agent/extensions/browser.ts
import browser from "@agent-browser/eve";

export default browser({});
```

That's it. The agent now has `browser__navigate`, `browser__snapshot`, `browser__click`, and the rest, plus an instructions fragment teaching the model the snapshot-refs workflow.

On first tool use in a fresh sandbox, the extension installs agent-browser (and Chromium plus its system libraries) automatically. For production, pre-install in your sandbox bootstrap instead so sessions start from a warm template:

```ts
// agent/sandbox.ts
import { agentBrowserRevalidationKey, installAgentBrowser } from "@agent-browser/eve/sandbox";
import { defineSandbox } from "eve/sandbox";
import { vercel } from "eve/sandbox/vercel";

export default defineSandbox({
  backend: vercel({ resources: { vcpus: 2 } }),
  revalidationKey: () => agentBrowserRevalidationKey(),
  async bootstrap({ use }) {
    const sandbox = await use();
    await installAgentBrowser(sandbox);
  },
});
```

`@agent-browser/eve/sandbox` also exports `runAgentBrowser` for calling agent-browser from a hand-written eve tool. It derives a short, stable session name from the eve sandbox id; pass `session` when multiple independent browser sessions should share one sandbox.

## Configuration

All fields are optional:

```ts
export default browser({
  // Safety
  allowedDomains: ["example.com", "*.example.com"], // restrict navigation and sub-resources
  contentBoundaries: true, // wrap page output in markers the model can recognize
  maxOutputChars: 50_000, // truncate page output

  // Output
  inlineScreenshots: true, // embed screenshots in tool output as data URLs for channels/UIs
  // (hidden from the model via toModelOutput; capped at 4 MB per image)

  // Install behavior
  autoInstall: true, // install agent-browser on first use when missing
  installSpec: "agent-browser@0.31.2", // defaults to the version matching this package
  installBrowser: true, // download Chromium during auto-install
  installSystemDependencies: true, // apt/dnf Chromium libraries during auto-install

  // Session & runtime
  session: undefined, // fixed session name; defaults to one derived from the sandbox id
  sessionPrefix: "eve", // prefix for the derived session name
  binary: "agent-browser", // binary name or path inside the sandbox
  proxy: undefined, // proxy server URL
});
```

## Tools

| Tool | Purpose |
| --- | --- |
| `navigate` | Open a URL, or go back/forward/reload |
| `snapshot` | Accessibility tree with `@eN` element refs (the primary way to see a page) |
| `read` | Page as agent-friendly text/markdown, `llms.txt` aware |
| `click` | Click or double-click an element |
| `fill` | Type into an input (clearing first by default) |
| `press_key` | Press a key or combination (`Enter`, `Control+a`) |
| `hover` | Hover an element |
| `select_option` | Pick a `<select>` option |
| `set_checked` | Check/uncheck a checkbox or radio |
| `scroll` | Scroll the page, a container, or an element into view |
| `drag` | Drag and drop |
| `upload` | Attach sandbox files to a file input |
| `wait_for` | Wait for an element, text, URL pattern, load state, JS condition, or delay |
| `get` | Read text/html/value/attribute/title/url/count/box/styles |
| `find` | Semantic locator: act on an element by role, label, text, placeholder, or test id |
| `screenshot` | Capture the page (full-page and annotated modes); output includes an inline data URL for UIs |
| `evaluate` | Run JavaScript in the page |
| `tabs` | List/open/switch/close tabs (stable ids and labels) |
| `console` | Read console messages and uncaught page errors |
| `network_requests` | List tracked requests or fetch one request's full detail |
| `close` | Close the browser session |

Cookie, storage, and saved-auth-state commands are intentionally not exposed as tools — they make credential material visible to the model. If you need them, add a scoped tool in your own agent (or an [override](#overrides)) with the guardrails your app requires.

## Overrides

Standard eve extension overrides apply. Mount as a directory to gate or replace individual tools:

```ts
// agent/extensions/browser/extension.ts
import browser from "@agent-browser/eve";

export default browser({});
```

```ts
// agent/extensions/browser/tools/evaluate.ts — require approval for raw JS
import { evaluate } from "@agent-browser/eve/tools";
import { defineTool } from "eve/tools";
import { always } from "eve/tools/approval";

export default defineTool({ ...evaluate, approval: always() });
```

```ts
// agent/extensions/browser/tools/upload.ts — or drop a tool entirely
import { disableTool } from "eve/tools";

export default disableTool();
```

## How it works

Each tool shells out to the `agent-browser` CLI inside the sandbox from `ctx.getSandbox()`, using an isolated browser session named after the sandbox id, and returns the parsed `--json` payload. The browser and its state live entirely in the sandbox; the app runtime only relays commands.

## Example

See [`examples/eve`](https://github.com/vercel-labs/agent-browser/tree/main/examples/eve) for a complete Next.js eve app with this extension mounted.

## License

Apache-2.0
