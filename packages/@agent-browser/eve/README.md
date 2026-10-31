# @agent-browser/eve

An [eve](https://eve.dev) extension that gives your agent a full browser automation tool set, backed by [agent-browser](https://agent-browser.dev) running inside the agent's sandbox.

Mount it once and the agent gets 42 namespaced tools for navigation, accessibility snapshots with stable element refs, clicking, form filling, semantic element finding, waiting, screenshots, JavaScript evaluation, tab management, and network and console inspection. Their names, descriptions, and input schemas come from the `agent-browser mcp` tool catalog of the matching CLI version, so they stay in sync with the CLI.

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

That's it. The agent now has `browser__open`, `browser__snapshot`, `browser__click`, and the rest, plus an instructions fragment teaching the model the snapshot-refs workflow.

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

`@agent-browser/eve/sandbox` also exports `runAgentBrowser` for calling agent-browser from a hand-written eve tool. It uses one default browser session per sandbox; pass `session` when multiple independent browser sessions should share one sandbox.

## Configuration

All fields are optional:

```ts
export default browser({
  // Safety
  allowedDomains: ["example.com", "*.example.com"], // restrict navigation and sub-resources
  caCert: "/etc/ssl/certs/proxy-ca.pem",
  clearCaCert: false,
  useSystemCa: true, // OS trust store for the CLI's own HTTPS requests, such as read
  contentBoundaries: true, // wrap page output in markers the model can recognize
  maxOutputChars: 50_000, // truncate page output

  // Output
  inlineScreenshots: true, // embed screenshots in tool output as data URLs for channels/UIs
  // (hidden from the model via toModelOutput; capped at 4 MB per image)

  // Install behavior
  autoInstall: true, // install agent-browser on first use, or replace an older default install
  installSpec: undefined, // npm spec; must install this package's agent-browser minor (default: the matching version)
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

The 28 tools in the CLI's `core` MCP profile are listed to the model every turn. The other 14, marked *deferred*, stay out of the list and the model finds them with `eve__search`, so they add no context until used.

| Tool | Purpose |
| --- | --- |
| `back` | Back |
| `check` | Check element |
| `click` | Click element |
| `close` | Close browser |
| `console` | Console logs (deferred) |
| `dblclick` | Double-click element (deferred) |
| `drag` | Drag and drop (deferred) |
| `errors` | Page errors (deferred) |
| `eval` | Evaluate JavaScript |
| `fill` | Fill input |
| `find` | Find element (deferred) |
| `forward` | Forward |
| `get_attr` | Get attribute (deferred) |
| `get_box` | Get box (deferred) |
| `get_count` | Get count (deferred) |
| `get_html` | Get HTML (deferred) |
| `get_styles` | Get styles (deferred) |
| `get_text` | Get text |
| `get_title` | Get title |
| `get_url` | Get URL |
| `get_value` | Get value (deferred) |
| `hover` | Hover element (deferred) |
| `network_requests` | Network requests (deferred) |
| `open` | Open page |
| `press` | Press key |
| `read` | Read URL |
| `reload` | Reload |
| `screenshot` | Take screenshot |
| `scroll` | Scroll page |
| `select` | Select options |
| `snapshot` | Snapshot page |
| `tab_close` | Tab close |
| `tab_list` | Tab list |
| `tab_new` | Tab new |
| `tab_switch` | Tab switch |
| `type` | Type text |
| `uncheck` | Uncheck element |
| `upload` | Upload files (deferred) |
| `wait_for_load` | Wait for load state |
| `wait_for_selector` | Wait for selector |
| `wait_for_text` | Wait for text |
| `wait_ms` | Wait milliseconds |

Cookie, storage, and saved-auth-state commands are intentionally not exposed as tools — they make credential material visible to the model. If you need them, add a scoped tool in your own agent (or an [override](#overrides)) with the guardrails your app requires.

## Overrides

Standard eve extension overrides apply. Mount as a directory to gate or replace individual tools:

```ts
// agent/extensions/browser/extension.ts
import browser from "@agent-browser/eve";

export default browser({});
```

```ts
// agent/extensions/browser/tools/eval.ts — require approval for raw JS
import { eval as evaluate } from "@agent-browser/eve/tools";
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

Each tool runs `agent-browser mcp call <tool> '<arguments>'` inside the sandbox from `ctx.getSandbox()`, using an isolated browser session named after the sandbox id, and returns the `data` payload of the CLI response. Every call is its own short-lived process, so nothing outlives the tool call; the browser and its state live in the session daemon inside the sandbox. The extension sets `session`, `allowedDomains`, and the CA options from its config and hides them from the model. If the sandbox runs a different agent-browser minor version than this package, tools fail with an install hint instead of sending arguments the CLI may not accept.

## Example

See [`examples/eve`](https://github.com/vercel-labs/agent-browser/tree/main/examples/eve) for a complete Next.js eve app with this extension mounted.

## License

Apache-2.0
