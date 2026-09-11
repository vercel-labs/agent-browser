# Setup Guide

Complete installation guide for agent-browser MCP Server.

## Step 1: Install Prerequisites

### Install Node.js
- Download from [nodejs.org](https://nodejs.org/) (v20 or later recommended)
- Verify: `node --version`

### Install agent-browser
```bash
npm install -g agent-browser
agent-browser install
```

Verify installation:
```bash
agent-browser --version
```

## Step 2: Install MCP Server

### Option A: NPX (Recommended - No Installation)

No installation needed! NPX will download and run automatically.

Skip to Step 3 for configuration.

### Option B: Global Install

```bash
npm install -g @agent-browser/mcp-server
```

Verify:
```bash
agent-browser-mcp --help
```

### Option C: From Source (Development)

1. Clone the agent-browser repository:
```bash
git clone https://github.com/vercel-labs/agent-browser.git
cd agent-browser/mcp-server
```

2. Install dependencies:
```bash
npm install
```

3. Build:
```bash
npm install -g agent-browser-mcp-server
```

Then you can use the `agent-browser-mcp` command directly.

## Step 3: Configure Your MCP Client

### For Cursor

1. Open Cursor settings directory:
   - **macOS/Linux**: `~/.cursor/`
   - **Windows**: `%USERPROFILE%\.cursor\`

2. Create or edit `mcp.json`:

**If installed from source:**
```json
{
  "mcpServers": {
    "agent-browser": {
      "command": "node",
      "args": ["/absolute/path/to/agent-browser-mcp-server/dist/index.js"]
    }
  }
}
```

**If installed globally:**
```json
{
  "mcpServers": {
    "agent-browser": {
      "command": "agent-browser-mcp"
    }
  }
}
```

3. Restart Cursor

### For Claude Desktop

1. Locate config file:
   - **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
   - **Windows**: `%APPDATA%\Claude\claude_desktop_config.json`
   - **Linux**: `~/.config/Claude/claude_desktop_config.json`

2. Edit config:

**If installed from source:**
```json
{
  "mcpServers": {
    "agent-browser": {
      "command": "node",
      "args": ["/absolute/path/to/agent-browser-mcp-server/dist/index.js"]
    }
  }
}
```

**Example paths:**
- macOS: `"/Users/username/projects/agent-browser-mcp-server/dist/index.js"`
- Windows: `"C:\\Users\\username\\projects\\agent-browser-mcp-server\\dist\\index.js"`
- Linux: `"/home/username/projects/agent-browser-mcp-server/dist/index.js"`

3. Restart Claude Desktop

## Step 4: Test It!

After restarting your MCP client, try asking:

```
"Open https://example.com and take a screenshot"
```

The AI should:
1. Launch a browser
2. Navigate to the site
3. Capture and show you a screenshot

## Troubleshooting

### "agent-browser command not found"

**Solution:**
```bash
npm install -g agent-browser
agent-browser install
```

Verify it's in PATH:
```bash
which agent-browser  # Unix/Mac
where agent-browser  # Windows
```

### "Cannot find module '../../dist/browser.js'"

**Cause:** MCP server can't find agent-browser installation.

**Solution:**
1. Make sure agent-browser is installed globally: `npm list -g agent-browser`
2. Or install it in the same directory: `npm install agent-browser`

### "MCP server not starting"

**Check:**
1. Is Node.js installed? `node --version`
2. Is the path in config correct? (Use absolute paths!)
3. Was the server built? Check if `dist/index.js` exists
4. Check client logs:
   - Cursor: Open Developer Tools (Help → Toggle Developer Tools)
   - Claude Desktop: Check console logs

### Browser doesn't appear (headless mode)

By default, browser runs in **headed mode** (visible window).

To change to headless:
1. Edit `src/index.ts` line 232
2. Change `headless: false` to `headless: true`
3. Rebuild: `npm run build`
4. Restart MCP client

### Windows-specific issues

**Node.js not recognized:**
- Make sure Node.js is in PATH
- Restart terminal/PowerShell after installing Node.js

**Path escaping:**
- Use double backslashes in JSON: `"C:\\path\\to\\file"`
- Or use forward slashes: `"C:/path/to/file"`

**Permission errors:**
- Run terminal as Administrator if needed
- Check antivirus isn't blocking browser spawn

## Platform-Specific Notes

### macOS
- First time running may trigger security prompt
- Go to System Preferences → Security & Privacy → Allow

### Linux
- Install browser dependencies:
  ```bash
  agent-browser install --with-deps
  ```
  Or manually:
  ```bash
  npx playwright install-deps chromium
  ```

### Windows
- Use PowerShell or Command Prompt (not Git Bash for config)
- Check Windows Defender isn't blocking

## Advanced Configuration

### Change Browser Type

Edit `src/index.ts`:
```typescript
await browser.launch({
  browser: 'chromium',  // or 'firefox', 'webkit'
  headless: false,
});
```

### Custom Viewport Size

In `src/index.ts`:
```typescript
await browser.launch({
  viewport: { width: 1920, height: 1080 },
});
```

### Multiple Sessions

The AI can use different sessions automatically:
```
"Open Google in session 'task1' and GitHub in session 'task2'"
```

Each session is an isolated browser instance.

## Getting Help

- Check [GitHub Issues](https://github.com/YOUR_USERNAME/agent-browser-mcp-server/issues)
- agent-browser docs: [github.com/vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser)
- MCP Protocol docs: [modelcontextprotocol.io](https://modelcontextprotocol.io/)

## Next Steps

Once working:
1. Try the [Usage Examples](README.md#usage-examples)
2. Explore all [50+ tools](README.md#available-tools-50)
3. Build your own automation workflows!
