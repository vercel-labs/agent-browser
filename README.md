# agent-browser

Browser automation for AI agents: open pages, read them as an accessibility tree, and act on elements from the shell.

<p>
  <a href="https://vercel.com/labs#labs-products"><img alt="Vercel Labs Product" src="https://img.shields.io/badge/LABS-PRODUCT-0a0a0a.svg?style=for-the-badge&amp;logo=Vercel&amp;labelColor=000000" height="28"></a>
  <a href="https://www.npmjs.com/package/agent-browser"><img alt="npm version: agent-browser" src="https://img.shields.io/npm/v/agent-browser.svg?style=for-the-badge&amp;labelColor=000000" height="28"></a>
  <a href="https://github.com/vercel-labs/agent-browser/blob/main/LICENSE"><img alt="License: Apache-2.0" src="https://img.shields.io/github/license/vercel-labs/agent-browser.svg?style=for-the-badge&amp;labelColor=000000" height="28"></a>
  <a href="https://www.npmjs.com/package/agent-browser"><img alt="npm downloads per month: agent-browser" src="https://img.shields.io/npm/dm/agent-browser.svg?style=for-the-badge&amp;labelColor=000000&amp;label=npm%20downloads" height="28"></a>
  <a href="https://skills.sh/vercel-labs/agent-browser"><img alt="skills.sh" src="https://skills.sh/b/vercel-labs/agent-browser?style=for-the-badge" height="28"></a>
</p>

agent-browser is a native Rust CLI that gives an AI agent a browser it can drive one command at a time. `snapshot` returns the page as compact text with a ref such as `@e1` for each element. The agent acts on a ref, reads the page again to check the result, and repeats. The browser stays open between commands, so each step is one short shell call.

## Install

```bash
npm install -g agent-browser
agent-browser install           # download Chrome for Testing (first time only)
```

- **Homebrew:** `brew install agent-browser`, then `agent-browser install`.
- **Linux:** `agent-browser install --with-deps` also installs the browser's system libraries.
- **No install:** `npx agent-browser open example.com`.

Project installs, building from source, `upgrade`, and `doctor` are covered in [Installation](https://agent-browser.dev/installation).

## Quick start

```bash
agent-browser open example.com
agent-browser snapshot -i       # interactive elements, each with a ref
agent-browser click @e1         # act on a ref from that snapshot
agent-browser get url           # check the result
agent-browser close
```

The snapshot for `example.com`:

```
- link "Learn more" [ref=e1]
```

- Refs describe the page at the time of the snapshot. After navigation or a DOM change, take a new snapshot before using refs again.
- CSS selectors, text, XPath, and semantic locators also work, for example `agent-browser find role button click --name "Submit"`. See [Selectors](https://agent-browser.dev/selectors).
- A click fails, instead of reporting success, when the target is hidden, disabled, outside the viewport, or covered by another element such as a consent banner. Dismiss the covering element, take a new snapshot, and retry.

The full walkthrough is in the [quick start](https://agent-browser.dev/quick-start).

## How it works

The CLI sends each command to a background daemon that starts on the first command and controls the browser over the Chrome DevTools Protocol. Each `--session` gets its own daemon and browser. By default, the daemon closes a headless browser and exits after one hour without commands, saving state first when [session persistence](https://agent-browser.dev/sessions) is configured.

Chrome for Testing is the default browser. You can also attach to a running Chrome with `--cdp` or `--auto-connect` ([CDP mode](https://agent-browser.dev/cdp-mode)), select the [Lightpanda](https://agent-browser.dev/engines/lightpanda) or [Obscura](https://agent-browser.dev/engines/obscura) engine with `--engine`, drive Mobile Safari in the [iOS Simulator](https://agent-browser.dev/ios), or use a cloud browser provider such as [Browserbase](https://agent-browser.dev/providers/browserbase), [Browserless](https://agent-browser.dev/providers/browserless), [Browser Use](https://agent-browser.dev/providers/browser-use), [Kernel](https://agent-browser.dev/providers/kernel), or [AgentCore](https://agent-browser.dev/providers/agentcore).

Select Firefox with `--engine firefox`. It requires Firefox 150 or newer, uses WebDriver BiDi and Firefox's DevTools accessibility tree for snapshots, and starts with a fresh temporary profile. See the [Firefox engine guide](https://agent-browser.dev/engines/firefox).

## For AI agents

The CLI ships skills that match its version:

```bash
agent-browser skills get core                 # the core loop, refs, waiting, and common workflows
agent-browser skills get core --ref commands  # one reference, such as the full command list
agent-browser skills get core --full          # adds the full command reference and templates
agent-browser skills list                     # specialized skills: electron, slack, dogfood, and more
```

Install the discovery skill into an agent harness from [skills.sh](https://skills.sh/vercel-labs/agent-browser):

```bash
npx skills add vercel-labs/agent-browser
```

The installed skill is a thin stub that tells the agent to load `agent-browser skills get core`, so its instructions always match the installed CLI. Or add this to your `AGENTS.md`:

```markdown
Use `agent-browser` for browser automation. Before the first use, run
`agent-browser skills get core` and follow it.
```

For MCP clients, `agent-browser mcp` starts an MCP server over stdio. See [MCP server](https://agent-browser.dev/commands#mcp-server).

## Commands

Run `agent-browser --help` for the full list, or `agent-browser <command> --help` for one command.

| Group | Commands | Docs |
|---|---|---|
| Navigate and read | `open`, `back`, `forward`, `reload`, `read`, `snapshot`, `get`, `is` | [Core](https://agent-browser.dev/commands#core), [Snapshots](https://agent-browser.dev/snapshots) |
| Interact | `click`, `dblclick`, `fill`, `type`, `press`, `keyboard`, `hover`, `focus`, `select`, `check`, `uncheck`, `scroll`, `drag`, `upload`, `mouse`, `find` | [Core](https://agent-browser.dev/commands#core), [Find elements](https://agent-browser.dev/commands#find-elements) |
| Wait | `wait` | [Wait](https://agent-browser.dev/commands#wait) |
| Capture | `screenshot`, `pdf`, `record`, `download`, `diff` | [Recording](https://agent-browser.dev/recording), [Diffing](https://agent-browser.dev/diffing), [Files](https://agent-browser.dev/files) |
| Page and network state | `cookies`, `storage`, `network`, `set`, `clipboard`, `eval`, `pushstate` | [Network](https://agent-browser.dev/network), [Settings](https://agent-browser.dev/commands#settings) |
| Tabs, frames, and dialogs | `tab`, `frame`, `dialog` | [Tabs and frames](https://agent-browser.dev/commands#tabs-frames) |
| Debug and performance | `console`, `errors`, `highlight`, `inspect`, `trace`, `profiler`, `vitals`, `react`, `a11y` | [Debugging](https://agent-browser.dev/debugging), [Profiler](https://agent-browser.dev/profiler), [React](https://agent-browser.dev/react) |
| Sessions and auth | `session`, `state`, `auth`, `profiles`, `confirm`, `deny` | [Sessions](https://agent-browser.dev/sessions), [Security](https://agent-browser.dev/security) |
| Agents and integrations | `skills`, `mcp`, `webmcp`, `flow`, `chat`, `batch`, `plugin` | [Skills](https://agent-browser.dev/skills), [WebMCP](https://agent-browser.dev/webmcp), [Plugins](https://agent-browser.dev/plugins) |
| Observe | `stream`, `dashboard` | [Streaming](https://agent-browser.dev/streaming), [Dashboard](https://agent-browser.dev/dashboard) |
| Setup | `install`, `upgrade`, `doctor`, `connect`, `close` | [Installation](https://agent-browser.dev/installation) |

Flags, environment variables, and `agent-browser.json` are documented in [Configuration](https://agent-browser.dev/configuration).

## Platforms

Native binaries ship for macOS (ARM64, x64), Linux (ARM64, x64, glibc and musl), and Windows (x64). The npm package picks the right binary for the host.

## Development

Building from source requires Node.js 24 or later, pnpm 11 or later, and Rust:

```bash
pnpm install
pnpm build:source               # builds the dashboard and the native CLI
pnpm link --global
agent-browser install
```

Run the Rust checks from `cli/`:

```bash
cargo fmt -- --check
cargo clippy
cargo test
cargo test e2e -- --ignored --test-threads=1   # launches real headless Chrome
```

[AGENTS.md](AGENTS.md) lists the files to update with each change, the docs checks, and the release process.

## Documentation

- [agent-browser.dev](https://agent-browser.dev): introduction, guides, and reference
- [Commands](https://agent-browser.dev/commands) and [Configuration](https://agent-browser.dev/configuration)
- [Security](https://agent-browser.dev/security): the auth vault, domain allowlists, action policies, and content boundaries
- [Next.js and Vercel](https://agent-browser.dev/next) and [eve](https://agent-browser.dev/eve): running agent-browser in Vercel Sandbox and eve agents
- [Changelog](CHANGELOG.md)

## License

Apache-2.0
