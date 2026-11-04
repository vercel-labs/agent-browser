# Codex browser connections for macOS

Connect Codex to agent-browser 0.41.2 through two Model Context Protocol (MCP) servers. `browser-isolated` launches a separate Chrome with a temporary profile. `browser-chrome` connects to an already running Chrome and pins a dedicated tab.

This integration belongs to this fork. It does not change the upstream CLI or publish a new agent-browser release.

## Requirements

- macOS on Apple Silicon or Intel, Python 3.11 or later, and Google Chrome in `/Applications`.
- Codex CLI on your `PATH` to register the servers.
- Network access to download the pinned native binary and its usage guides.

The installer verifies the native binary against the SHA-256 digest for the [0.41.2 release](https://github.com/vercel-labs/agent-browser/releases/tag/v0.41.2). It does not install Node dependencies or download another Chrome. The binary is approximately 16 MB on Apple Silicon or 18 MB on Intel.

## Install

Run these commands from the repository root after reviewing `integrations/codex-macos/setup.py`:

```sh
# Prepare local runtime files. This does not register MCP servers.
python3 integrations/codex-macos/setup.py

# Back up Codex configuration, then register both servers.
python3 integrations/codex-macos/setup.py --apply
```

The default destination is `~/Downloads/codex-browser-automation`. Use `--root /absolute/path/to/runtime` to choose another directory. Preparation writes the generated connection files in that directory; review and copy any customized connection files before rerunning it.

With `--apply`, the installer backs up `config.toml` and `AGENTS.md` from the configured Codex home, adds the two MCP entries, and appends browser guidance if its heading is absent. It checks that unrelated existing Codex settings are preserved. Backups stay in the local runtime's `backups/` directory and may contain private configuration. Keep them out of Git.

Restart your Codex client to load the entries, then check its MCP settings. Registration alone does not verify browser operation. See the [official Codex MCP guide](https://developers.openai.com/codex/mcp) for client configuration.

## Choose the connection

| Server | Use | Connection |
| --- | --- | --- |
| `browser-isolated` | Public pages and testing without the daily browser's profile | Installed Chrome, temporary profile |
| `browser-chrome` | Tasks explicitly authorized in the daily browser | `ws://127.0.0.1:9222/devtools/browser`, dedicated pinned tab |

For daily Chrome, open `chrome://inspect/#remote-debugging`, enable **Allow remote debugging for this browser instance**, and confirm that the displayed server uses port 9222. Approve Chrome's connection prompt when you expect the connection. The installer does not change that browser setting or approve prompts.

The direct WebSocket URL avoids the temporary discovery connection used by a numeric `9222` configuration. That discovery path opens a probe socket, closes it, and then opens the actual socket. Chrome may prompt again when a new connection is created. This configuration does not bypass Chrome's consent checks or guarantee that a future reconnection will not prompt.

Every MCP child command loads the same selected configuration. If using the CLI yourself, pass the same config and session on every command; omitting the connection settings can launch an isolated browser instead.

```sh
BROWSER_RUNTIME="$HOME/Downloads/codex-browser-automation"
"$BROWSER_RUNTIME/bin/agent-browser" --config "$BROWSER_RUNTIME/configs/chrome.json" --session codex-example open about:blank
"$BROWSER_RUNTIME/bin/agent-browser" --config "$BROWSER_RUNTIME/configs/chrome.json" --session codex-example snapshot -i
```

## Data boundaries

The launcher exposes the `core,tabs` tool profiles, without the dedicated cookie, storage, or authentication tool profile. This is not a security sandbox: JavaScript evaluation and a connection to the daily browser still have substantial access. Tab pinning controls tab selection; it does not isolate accounts or cookies.

For a check that does not access stored browser data, use a dedicated `about:blank` tab with invented test content. Do not navigate to account pages, call cookie or storage APIs, read saved passwords, or export browser state. Avoid inherited `AGENT_BROWSER_RESTORE` settings and `--restore` options; explicit environment variables can override generated configuration. The verification below cleared inherited agent-browser environment settings and supplied `restore: false` and `restoreSave: "never"` on every call.

Publish only source files and instructions from this integration directory. Keep runtime binaries, browser profiles, cookies, credentials, configuration backups, screenshots of private pages, and local verification output on the local machine. The installer adds ignore rules to its runtime directory, but review the staged file list before any push.

## Verification

On macOS Apple Silicon, the installed 0.41.2 runtime passed a test through one MCP process using the direct WebSocket configuration: open a dedicated `about:blank` tab, add invented text and a button in memory, snapshot it, click the button, confirm the result, and check the URL. All six calls reported connection reuse without restarting the daemon; browser state restoration was not configured and saving was not attempted.

This verifies that local Chrome connection and interaction sequence. It does not establish that this Codex client has reloaded the servers, that every future connection avoids prompts, or that Intel and isolated-browser workflows have passed the same test. The public installer has not been rerun against the live Codex configuration for this update.

## Roll back

For the connection-address change, copy the reviewed local `chrome.json` backup back to `configs/chrome.json`, then restart the affected MCP client. This changes the connection configuration only.

To unregister this integration, remove only `browser-isolated` and `browser-chrome` from Codex's MCP settings and remove the `Codex browser connections` guidance section if you no longer want it. Compare against your backups; do not replace the entire Codex configuration if other settings have changed since the backup. Keep runtime files and backups until you decide to remove them. This guide does not require deleting browser data.

## 中文说明

这套配置提供两个入口：`browser-isolated` 使用独立的临时 Chrome 配置，`browser-chrome` 连接日常 Chrome 并绑定专用标签页。安装命令和路径见上文。

日常 Chrome 的地址使用 `ws://127.0.0.1:9222/devtools/browser`，减少端口自动探测带来的额外连接。新连接仍可能出现 Chrome 授权提示。每次操作使用相同的配置与会话，避免意外切换到独立浏览器。

已验证空白页上的显示、读取、按钮点击和结果检查。验证没有调用 Cookie、密码、网站存储或状态导出接口。专用标签页不是账户隔离机制；日常 Chrome 仍具有登录数据，必须按任务范围限制操作。

GitHub 保存安装源代码与说明；运行程序和私有备份留在本机。上传代码不会自动释放本机空间。本次更新不删除现有文件，也不要求清理浏览器数据。
