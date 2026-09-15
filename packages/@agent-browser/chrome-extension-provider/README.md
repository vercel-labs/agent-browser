# Chrome extension provider

The user authorizes one existing Chrome tab in the extension, and agent-browser runs its normal commands on that signed-in page; the user chooses the tab and can stop control at any time. The provider connects through a local relay and Chrome's debugger extension API, so it needs no debugging port or profile copy.

This is an optional package. Its runtime requires Node.js 24+; the default agent-browser CLI and daemon remain native Rust. The unpacked extension declares Chrome 125 as its minimum because it uses the child-session debugger API. This is an API requirement, not a claim that every Chrome version or platform has been verified.

## Build and register

Until the package is published to npm, build it locally from the agent-browser repository root:

```bash
pnpm install
pnpm --filter @agent-browser/chrome-extension-provider build
```

Use the agent-browser CLI built from the same checkout; see [From Source](../../../README.md#from-source). Add this entry to the `plugins` array in your `agent-browser.json`, replacing the path with the absolute path to the built plugin. Preserve any other configured plugins:

```json
{
  "plugins": [
    {
      "name": "chrome-extension",
      "command": "node",
      "args": ["/absolute/path/to/agent-browser/packages/@agent-browser/chrome-extension-provider/dist/plugin.js"],
      "capabilities": ["browser.provider", "command.run"]
    }
  ]
}
```

## Authorize a tab

```bash
agent-browser plugin run chrome-extension chrome-extension.setup --payload '{"session":"work"}'
```

Setup returns immediately with `extensionPath`, a one-time `pairingCode`, and its expiration time. Complete these steps in the Chrome profile containing the tab you want to use:

1. Open `chrome://extensions`, enable **Developer mode**, choose **Load unpacked**, and select `extensionPath`.
2. Open the **Agent Browser** extension and paste the complete `pairingCode` into its setup page.
3. Check the displayed session and choose the exact tab to authorize.

The pairing code is private. If it expires, run setup again for a new code. Setup does not authorize a tab by itself.

```bash
agent-browser --provider chrome-extension --session work snapshot -i
agent-browser --provider chrome-extension --session work fill @e1 "Draft text"
agent-browser --provider chrome-extension --session work screenshot page.png
agent-browser --provider chrome-extension --session work close
```

Use refs from the latest snapshot. `open <url>` navigates the authorized tab. `close` releases control and leaves Chrome, the tab, and its unsaved page state open. The extension also provides **Stop**.

Always put the session explicitly in setup/status payloads: `plugin run` does not inject the global `--session`. If you use a namespace, pass the same `--namespace` or set the same `AGENT_BROWSER_NAMESPACE` for setup, status, and every browser command:

```bash
export AGENT_BROWSER_NAMESPACE=my-project
agent-browser plugin run chrome-extension chrome-extension.setup --payload '{"session":"work"}'
# Authorize the tab in the extension, then:
agent-browser --provider chrome-extension --session work snapshot -i
```

## Scope and release

Each session controls one explicitly authorized tab and its frames. Other tabs, popups, and windows are not added to that authorization. A tab can be controlled by only one session at a time. New tabs, switching to other tabs, and creating browser contexts are unsupported. `tab close` follows the core rule that rejects closing the last controlled tab; close the tab in Chrome when that is your intent.

Profile/state/restore options, `--pin-tab`, `--allowed-domains`, and launch-only settings such as executable paths, Chrome arguments, extension loading, launch proxies, headless mode, and non-Chrome engines are unsupported. Remove conflicting settings from flags, environment, or config before connecting. The default idle timeout exempts this user-managed browser; an explicit timeout releases control.

Authorization limits debugger targets and cookie requests to the authorized page's scope. It does not create a separate cookie jar or origin sandbox. The page still navigates and runs scripts in the user's Chrome profile, and same-origin pages can observe shared storage changes.

Stop, debugger detachment, a closed tab, or a lost connection ends authorization. Control is never silently restored or moved to another tab. Run setup and let the user authorize again, then take a new snapshot before using refs.

## Status and troubleshooting

```bash
agent-browser plugin run chrome-extension chrome-extension.status --payload '{"session":"work"}'
```

Status returns the session scope, connection state, authorized tab when available, and `nextStep`. `extensionInstalled` reports whether the package contains built extension assets; it does not prove the extension is loaded in Chrome. If a browser command cannot connect, check status and complete the indicated setup or authorization step. If the tab is busy, release its current controlling session before authorizing it elsewhere.

Set `AGENT_BROWSER_CHROME_EXTENSION_DIR` to an absolute path when you need a separate provider state directory. It defaults to `~/.agent-browser/chrome-extension`, must be private to the current OS user, and must be consistent across setup, status, and browser commands.

If a relay fails its health check while its process is still alive, setup reports `unresponsive` instead of starting a second relay. Stop that relay process before retrying. If an interrupted setup leaves `startup.lock`, the error gives its path. Confirm that no setup process is running, remove only that lock file, and retry; setup does not automatically take over a stale lock.

## MCP

MCP uses the existing tools and the canonical CLI parser. Enable the `debug` profile for `agent_browser_plugin_run`, pass `name: "chrome-extension"`, `requestType: "chrome-extension.setup"` or `"chrome-extension.status"`, and `payload: { "session": "work" }`. Browser tools use `session: "work"` and `extraArgs: ["--provider", "chrome-extension"]`. Keep their `namespace` consistent with setup/status. No extension-specific MCP server or tools are needed.

## Developer verification

From this package directory, run:

```bash
pnpm test
pnpm test:pack
node test/fixtures/site.mjs
```

`test` builds the package and checks the plugin, relay, and mocked extension API contracts. `test:pack` checks installation and setup from a packed artifact outside the repository. These checks do not establish real desktop Chrome compatibility. The fixture command stays running and prints its main `url` and cross-site `frameUrl`.

For a desktop check, start Chrome normally with a clean test profile and no remote-debugging port or pipe flags. Open the fixture's main URL, click **Sign in for test**, and type a unique value into **Unsaved draft** (`#draft`). This creates a synthetic HttpOnly login through the page's normal form. Load the extension through `chrome://extensions`, run setup as above, and pair and authorize that exact `/workspace` tab through the extension UI.

Use the same session and namespace as setup. For example, with the `work` session:

```bash
export AGENT_BROWSER_PROVIDER=chrome-extension
export AGENT_BROWSER_SESSION=work
agent-browser snapshot -i
agent-browser eval "document.querySelector('#draft').value"
agent-browser fill '#task-input' 'Fixture task'
agent-browser find role button click --name 'Run action'
agent-browser frame iframe
agent-browser snapshot -i
agent-browser find role button click --name 'Frame action'
agent-browser eval "document.querySelector('output').textContent"
agent-browser frame main
agent-browser screenshot fixture.png
agent-browser find role link click --name 'Navigate in the same tab'
agent-browser snapshot -i
```

Check that the initial draft is unchanged, **Run action** produces **Action completed**, the frame evaluation returns **Frame clicked**, and navigation retains the signed-in session. Navigation creates a new document, so enter a new unsaved draft afterward. Repeat a snapshot and `#task-input` fill through MCP using the same provider, session, and namespace, then run `agent-browser close` and confirm the tab and new draft remain open. Reauthorize, connect again, and use the extension's **Stop** to check the same preservation and that subsequent commands require fresh authorization.

Record the exact Chrome version, operating system, and observed results for each desktop check. Record Chrome for Testing separately from regular Chrome, and leave untested browsers and platforms unverified.
