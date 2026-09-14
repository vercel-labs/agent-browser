# Existing Chrome Tab Authorization

The optional `@agent-browser/chrome-extension-provider` lets the user select one existing signed-in Chrome tab for normal agent-browser commands. The user grants authorization in the extension and can end it with **Stop**. No debugging port or profile copy is needed.

## Setup

The package currently uses a local build and an unpacked extension. It requires Node.js 24+; the native CLI and daemon do not. The extension's Chrome 125 minimum is a requirement of its child-session debugger API, not a claim that every version or platform has been verified. Follow the [provider installation guide](https://agent-browser.dev/providers/chrome-extension) to build and register the plugin as `chrome-extension`.

```bash
agent-browser plugin run chrome-extension chrome-extension.setup --payload '{"session":"work"}'
```

Setup returns `extensionPath`, a one-time `pairingCode`, and its expiration time without waiting for authorization. The user opens `chrome://extensions`, enables Developer mode, loads `extensionPath` with Load unpacked, opens the Agent Browser extension, pastes the pairing code, and selects the exact tab. Keep the code private; run setup again if it expires.

Management payloads must contain `session` explicitly. `plugin run` does not inject the global `--session` or `AGENT_BROWSER_SESSION` into the payload. If using a namespace, pass the same `--namespace` or set the same `AGENT_BROWSER_NAMESPACE` for setup, status, and browser commands.

## Normal commands

```bash
agent-browser --provider chrome-extension --session work snapshot -i
agent-browser --provider chrome-extension --session work click @e1
agent-browser --provider chrome-extension --session work snapshot -i
agent-browser --provider chrome-extension --session work screenshot page.png
agent-browser --provider chrome-extension --session work close
```

Use refs from the latest snapshot. `open <url>` navigates the authorized tab. `close` releases control while preserving Chrome, the tab, and unsaved input. The extension's Stop action also ends control. After disconnection, debugger detachment, or tab closure, run setup and have the user authorize again; take a fresh snapshot before using refs. Never choose another tab automatically.

One session controls one tab and its frames; only one session can control a given tab. Other tabs and popups are outside the authorization. Creating tabs or browser contexts and switching to other tabs are unsupported. The core `tab close` rule rejects closing the last controlled tab; the user can close it in Chrome.

Do not use profile/state/restore options, `--pin-tab`, `--allowed-domains`, or browser launch settings with this provider. These include executable paths, Chrome arguments, extension loading, launch proxies, headless mode, and non-Chrome engines. Remove conflicting environment and config values too. The default idle timeout exempts the user-managed browser; an explicit timeout releases control.

The authorization limits debugger targets and cookie requests, not the browser's cookie jar or origin storage. The authorized page and its scripts still use the user's Chrome profile; same-origin pages can observe shared storage changes.

## Readiness and troubleshooting

```bash
agent-browser plugin run chrome-extension chrome-extension.status --payload '{"session":"work"}'
```

Status returns the scope, connection state, authorized tab when available, and `nextStep`. Use it after connection errors. If the tab is busy, release its current session before trying a new authorization. `extensionInstalled` reports that built extension assets exist in the package, not that they are loaded in Chrome.

`AGENT_BROWSER_CHROME_EXTENSION_DIR` optionally selects a separate provider state directory. It must be an absolute path, defaults to `~/.agent-browser/chrome-extension`, and must be private to the current OS user. Use the same value for setup, status, and browser commands.

If the relay fails its health check but its process is still alive, setup reports `unresponsive` and will not start a second relay. Stop that relay process before retrying. If setup reports a leftover `startup.lock` path, first confirm that no setup process is running, then remove only that lock file and retry. Setup does not automatically take over stale locks.

## MCP

MCP uses existing tools through the canonical CLI parser. Enable the `debug` profile for `agent_browser_plugin_run`, with `name: "chrome-extension"`, `requestType: "chrome-extension.setup"` or `"chrome-extension.status"`, and `payload: { "session": "work" }`. Browser tools use `session: "work"` and `extraArgs: ["--provider", "chrome-extension"]`; use the same `namespace` as setup/status. There is no separate extension-specific MCP tool or server.
