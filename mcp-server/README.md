# agent-browser MCP Server

[Model Context Protocol](https://modelcontextprotocol.io) server for [agent-browser](https://github.com/vercel-labs/agent-browser). Enables LLMs to control browsers through 50+ automation tools.

## Quick Start

### Prerequisites

```bash
npm install -g agent-browser
agent-browser install
```

### Option 1: NPX (Recommended)

No installation needed! Just configure and use:

**Cursor** (`~/.cursor/mcp.json`):
```json
{
  "mcpServers": {
    "agent-browser": {
      "command": "npx",
      "args": ["-y", "@agent-browser/mcp-server"]
    }
  }
}
```

**Claude Desktop**:
- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "agent-browser": {
      "command": "npx",
      "args": ["-y", "@agent-browser/mcp-server"]
    }
  }
}
```

### Option 2: Global Install

```bash
npm install -g @agent-browser/mcp-server
```

**Configuration**:
```json
{
  "mcpServers": {
    "agent-browser": {
      "command": "agent-browser-mcp"
    }
  }
}
```

### Option 3: Local Development

```bash
cd mcp-server
npm install
npm run build
```

**Configuration**:
```json
{
  "mcpServers": {
    "agent-browser": {
      "command": "node",
      "args": ["/absolute/path/to/mcp-server/dist/index.js"]
    }
  }
}
```

## Usage

Once configured, LLMs can use browser automation:

```
"Open https://example.com and take a screenshot"
"Navigate to GitHub and click the login button"
"Fill the email field with test@example.com"
```

## Tools

The server provides 50+ browser automation tools:

### Navigation
- `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload`

### Interactions
- `browser_click`, `browser_fill`, `browser_type`, `browser_press`
- `browser_hover`, `browser_select`, `browser_check`, `browser_scroll`
- `browser_drag`, `browser_upload`

### Information
- `browser_snapshot` - Get accessibility tree with refs (AI-optimized)
- `browser_get_text`, `browser_get_html`, `browser_get_value`
- `browser_get_title`, `browser_get_url`, `browser_screenshot`

### Tabs
- `browser_tab_new`, `browser_tab_switch`, `browser_tab_close`, `browser_tab_list`

### Storage
- `browser_cookies_get`, `browser_cookies_set`, `browser_cookies_clear`
- `browser_storage_get`, `browser_storage_set`, `browser_storage_clear`

### Advanced
- `browser_evaluate` - Execute JavaScript
- `browser_frame_switch` - Work with iframes
- `browser_dialog_accept` - Handle alerts/confirms
- `browser_network_requests` - Track network activity

[View complete tool list](SETUP.md#available-tools-50)

## Workflow Pattern

```javascript
// 1. Navigate
browser_navigate({ url: "https://example.com" })

// 2. Get page structure
browser_snapshot({ interactive: true, compact: true })

// 3. Interact using refs
browser_click({ selector: "@e5" })  // Use ref from snapshot
browser_fill({ selector: "@e3", value: "text" })

// 4. Extract data
browser_get_text({ selector: "@e1" })
browser_screenshot({ fullPage: false })
```

## Architecture

```
LLM (Claude/Cursor) 
    ↓ MCP Protocol
MCP Server (this package)
    ↓ Direct import
BrowserManager (agent-browser)
    ↓
Playwright
```

This server directly imports `BrowserManager` from agent-browser for optimal performance.

## Development

```bash
npm run dev      # Watch mode
npm run build    # Production build
npm run format   # Format code with Prettier
npm test         # Run tests
```

## Configuration Options

### Headless Mode

Edit `src/index.ts` line ~680:

```typescript
headless: false,  // Change to true for headless mode
```

### Session Management

Multiple browser instances:

```javascript
browser_navigate({ url: "site1.com", session: "task1" })
browser_navigate({ url: "site2.com", session: "task2" })
```

## Troubleshooting

**agent-browser not found**
```bash
npm install -g agent-browser
agent-browser install
```

**Module not found**
- Ensure agent-browser is installed globally
- Check the path in MCP config is correct

**Linux dependencies**
```bash
agent-browser install --with-deps
```

See [SETUP.md](SETUP.md) for detailed troubleshooting.

## License

Apache-2.0

## Credits

Built on [agent-browser](https://github.com/vercel-labs/agent-browser) by Vercel Labs.
