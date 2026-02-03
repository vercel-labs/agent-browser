#!/usr/bin/env node
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import { exec } from "child_process";
import { promisify } from "util";
import { existsSync, unlinkSync, writeFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";

const execAsync = promisify(exec);

// Signal file for pause/continue mechanism
const CONTINUE_SIGNAL = join(homedir(), ".agent-browser-continue");
const WAITING_SIGNAL = join(homedir(), ".agent-browser-waiting");

// Helper to wait for user signal (no timeout - waits forever)
async function waitForUserSignal(message) {
  // Clean up any existing signals
  if (existsSync(CONTINUE_SIGNAL)) unlinkSync(CONTINUE_SIGNAL);
  
  // Create waiting signal so user knows we're paused
  writeFileSync(WAITING_SIGNAL, message || "Waiting for user...");
  
  const startTime = Date.now();
  const pollInterval = 500; // Check every 500ms
  
  return new Promise((resolve) => {
    const checkSignal = () => {
      // Check if continue signal exists
      if (existsSync(CONTINUE_SIGNAL)) {
        // Clean up signals
        try { unlinkSync(CONTINUE_SIGNAL); } catch {}
        try { unlinkSync(WAITING_SIGNAL); } catch {}
        resolve({ continued: true, elapsed: Date.now() - startTime });
        return;
      }
      
      // Keep polling forever
      setTimeout(checkSignal, pollInterval);
    };
    
    checkSignal();
  });
}

// Helper to run agent-browser commands
async function runAgentBrowser(args) {
  try {
    // Only use headless shell when explicitly in headless mode (server environments without display)
    // When AGENT_BROWSER_HEADED=1, use normal Chromium for visible browser window
    const headed = process.env.AGENT_BROWSER_HEADED === '1';
    const headlessShell = headed ? null : join(homedir(), '.cache/ms-playwright/chromium_headless_shell-1208/chrome-linux/headless_shell');
    const execPath = !headed && headlessShell && existsSync(headlessShell) ? `AGENT_BROWSER_EXECUTABLE_PATH="${headlessShell}" ` : '';
    const cmd = `export NVM_DIR="$HOME/.nvm" && [ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh" && ${execPath}agent-browser ${args}`;
    const execEnv = { ...process.env };
    if (headed) execEnv.AGENT_BROWSER_HEADED = "1";
    const { stdout, stderr } = await execAsync(cmd, {
      shell: "/bin/bash",
      timeout: 60000,
      env: execEnv,
    });
    return { success: true, output: stdout || stderr };
  } catch (error) {
    return { success: false, error: error.message, output: error.stderr || error.stdout };
  }
}

// Define available tools
const tools = [
  {
    name: "browser_open",
    description: "Navigate to a URL in the browser",
    inputSchema: {
      type: "object",
      properties: {
        url: { type: "string", description: "The URL to navigate to" },
        headed: { type: "boolean", description: "Show browser window (not headless)", default: true },
      },
      required: ["url"],
    },
  },
  {
    name: "browser_snapshot",
    description: "Get accessibility tree with interactive element refs (@e1, @e2, etc.). Use this to understand page structure and get refs for interactions. Set includeHtml=true to also get full page HTML for more accurate analysis.",
    inputSchema: {
      type: "object",
      properties: {
        interactive: { type: "boolean", description: "Only show interactive elements", default: true },
        compact: { type: "boolean", description: "Remove empty structural elements", default: false },
        selector: { type: "string", description: "CSS selector to scope snapshot" },
        depth: { type: "number", description: "Limit tree depth (e.g., 3 for shallow, omit for full depth)" },
        includeHtml: { type: "boolean", description: "Also return full page HTML for detailed analysis", default: false },
      },
    },
  },
  {
    name: "browser_click",
    description: "Click an element by ref (e.g., @e1) or CSS selector",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector" },
      },
      required: ["target"],
    },
  },
  {
    name: "browser_fill",
    description: "Clear and fill an input field",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector" },
        text: { type: "string", description: "Text to fill" },
      },
      required: ["target", "text"],
    },
  },
  {
    name: "browser_type",
    description: "Type text into an element (appends, doesn't clear)",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector" },
        text: { type: "string", description: "Text to type" },
      },
      required: ["target", "text"],
    },
  },
  {
    name: "browser_press",
    description: "Press a key (Enter, Tab, Control+a, etc.)",
    inputSchema: {
      type: "object",
      properties: {
        key: { type: "string", description: "Key to press (Enter, Tab, Escape, Control+a, etc.)" },
      },
      required: ["key"],
    },
  },
  {
    name: "browser_scroll",
    description: "Scroll the page",
    inputSchema: {
      type: "object",
      properties: {
        direction: { type: "string", enum: ["up", "down", "left", "right"], default: "down" },
        pixels: { type: "number", description: "Pixels to scroll", default: 300 },
      },
    },
  },
  {
    name: "browser_hover",
    description: "Hover over an element",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector" },
      },
      required: ["target"],
    },
  },
  {
    name: "browser_select",
    description: "Select an option from a dropdown",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector" },
        value: { type: "string", description: "Option value to select" },
      },
      required: ["target", "value"],
    },
  },
  {
    name: "browser_check",
    description: "Check a checkbox",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector" },
      },
      required: ["target"],
    },
  },
  {
    name: "browser_uncheck",
    description: "Uncheck a checkbox",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector" },
      },
      required: ["target"],
    },
  },
  {
    name: "browser_get",
    description: "Get information from the page (text, html, value, attr, title, url, count)",
    inputSchema: {
      type: "object",
      properties: {
        what: { type: "string", enum: ["text", "html", "value", "attr", "title", "url", "count"], description: "What to get" },
        target: { type: "string", description: "Element ref or CSS selector (not needed for title/url)" },
        attribute: { type: "string", description: "Attribute name (only for attr)" },
      },
      required: ["what"],
    },
  },
  {
    name: "browser_wait",
    description: "Wait for an element, text, URL pattern, or time",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref/selector, or milliseconds (number as string)" },
        text: { type: "string", description: "Wait for this text to appear" },
        url: { type: "string", description: "Wait for URL pattern" },
        load: { type: "string", enum: ["load", "domcontentloaded", "networkidle"], description: "Wait for load state" },
      },
    },
  },
  {
    name: "browser_screenshot",
    description: "Take a screenshot of the page",
    inputSchema: {
      type: "object",
      properties: {
        path: { type: "string", description: "File path to save screenshot (optional)" },
        full: { type: "boolean", description: "Full page screenshot", default: false },
      },
    },
  },
  {
    name: "browser_back",
    description: "Go back in browser history",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "browser_forward",
    description: "Go forward in browser history",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "browser_reload",
    description: "Reload the current page",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "browser_close",
    description: "Close the browser",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "browser_tabs",
    description: "List open tabs or manage tabs",
    inputSchema: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["list", "new", "switch", "close"], default: "list" },
        index: { type: "number", description: "Tab index for switch/close" },
        url: { type: "string", description: "URL for new tab" },
      },
    },
  },
  {
    name: "browser_cookies",
    description: "Manage cookies",
    inputSchema: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["get", "set", "clear"], default: "get" },
        name: { type: "string", description: "Cookie name (for set)" },
        value: { type: "string", description: "Cookie value (for set)" },
      },
    },
  },
  {
    name: "browser_eval",
    description: "Execute JavaScript in the browser",
    inputSchema: {
      type: "object",
      properties: {
        script: { type: "string", description: "JavaScript code to execute" },
      },
      required: ["script"],
    },
  },
  {
    name: "browser_html",
    description: "Get the full page HTML source for analysis. Use this to analyze page structure, extract content, or debug rendering issues. For interactive element refs, use browser_snapshot first.",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector to get HTML of specific element. If omitted, returns full page HTML." },
      },
    },
  },
  {
    name: "browser_pause",
    description: "Pause and wait for user to manually complete a step (e.g., solve captcha, login, inspect). Waits indefinitely until user runs: agent-continue",
    inputSchema: {
      type: "object",
      properties: {
        message: { type: "string", description: "Message to display explaining what the user should do" },
      },
    },
  },
  {
    name: "browser_user_action",
    description: "Request user to perform a manual action in the browser, then continue. Creates a visible notification and waits indefinitely for user to run: agent-continue",
    inputSchema: {
      type: "object",
      properties: {
        action: { type: "string", description: "Description of what the user should do (e.g., 'solve the captcha', 'complete 2FA', 'login manually')" },
      },
      required: ["action"],
    },
  },
  {
    name: "browser_highlight",
    description: "Highlight an element in the browser for debugging/visual feedback",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "Element ref (@e1) or CSS selector to highlight" },
      },
      required: ["target"],
    },
  },
  {
    name: "browser_console",
    description: "Get browser console messages (logs, warnings, errors)",
    inputSchema: {
      type: "object",
      properties: {
        clear: { type: "boolean", description: "Clear console after getting messages", default: false },
      },
    },
  },
  {
    name: "browser_errors",
    description: "Get JavaScript errors from the page",
    inputSchema: {
      type: "object",
      properties: {
        clear: { type: "boolean", description: "Clear errors after getting", default: false },
      },
    },
  },
  {
    name: "browser_network",
    description: "Get network requests made by the page",
    inputSchema: {
      type: "object",
      properties: {
        filter: { type: "string", description: "Filter requests by URL pattern" },
        clear: { type: "boolean", description: "Clear requests after getting", default: false },
      },
    },
  },
  {
    name: "browser_set_viewport",
    description: "Set the browser viewport size",
    inputSchema: {
      type: "object",
      properties: {
        width: { type: "number", description: "Viewport width in pixels" },
        height: { type: "number", description: "Viewport height in pixels" },
      },
      required: ["width", "height"],
    },
  },
  {
    name: "browser_set_device",
    description: "Emulate a device (iPhone, iPad, Pixel, etc.)",
    inputSchema: {
      type: "object",
      properties: {
        device: { type: "string", description: "Device name (e.g., 'iPhone 14', 'iPad Pro', 'Pixel 5')" },
      },
      required: ["device"],
    },
  },
  {
    name: "browser_pdf",
    description: "Save the current page as PDF",
    inputSchema: {
      type: "object",
      properties: {
        path: { type: "string", description: "File path to save PDF" },
      },
      required: ["path"],
    },
  },
  {
    name: "browser_drag",
    description: "Drag an element to another location",
    inputSchema: {
      type: "object",
      properties: {
        source: { type: "string", description: "Source element ref or selector" },
        target: { type: "string", description: "Target element ref or selector" },
      },
      required: ["source", "target"],
    },
  },
  {
    name: "browser_upload",
    description: "Upload files to a file input",
    inputSchema: {
      type: "object",
      properties: {
        target: { type: "string", description: "File input element ref or selector" },
        files: { type: "array", items: { type: "string" }, description: "Array of file paths to upload" },
      },
      required: ["target", "files"],
    },
  },
  {
    name: "browser_storage",
    description: "Get or set localStorage/sessionStorage",
    inputSchema: {
      type: "object",
      properties: {
        type: { type: "string", enum: ["local", "session"], description: "Storage type" },
        action: { type: "string", enum: ["get", "set", "clear"], description: "Action to perform" },
        key: { type: "string", description: "Storage key (for get/set)" },
        value: { type: "string", description: "Value to set" },
      },
      required: ["type"],
    },
  },
];

// Server instructions for Cursor - returned in Initialize response
const SERVER_INSTRUCTIONS = `The agent-browser-mcp is an MCP server for browser automation via agent-browser CLI. Use for frontend/webapp development and testing code changes.

IMPORTANT - Before interacting with any page:
1. Use browser_tabs with action "list" to see open tabs and their URLs
2. Use browser_snapshot to get the page structure and element refs (@e1, @e2, etc.) before any interaction (click, type, hover, etc.)

IMPORTANT - Waiting strategy:
When waiting for page changes, prefer short incremental waits (1-3 seconds) with browser_snapshot checks in between rather than a single long wait.

Notes:
- Use browser_type to append text, browser_fill to clear and replace
- For nested scroll containers, use browser_scroll with scrollIntoView before clicking obscured elements
- Use browser_pause or browser_user_action when user needs to manually complete a step (e.g., solve captcha, login)`;

// Create server
const server = new Server(
  { name: "agent-browser-mcp", version: "1.0.0" },
  { capabilities: { tools: {} }, instructions: SERVER_INSTRUCTIONS }
);

// Handle list tools
server.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools,
}));

// Handle tool calls
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;

  let command = "";

  switch (name) {
    case "browser_open": {
      // When headed mode requested, close any existing daemon first so a fresh headed daemon is spawned.
      // (Existing daemon may be headless; ensure_daemon skips spawn when daemon already running)
      const headed = args.headed !== false;
      if (headed) {
        await runAgentBrowser("close"); // Ignore result - no-op if no daemon running
      }
      command = `open "${args.url}"${headed ? " --headed" : ""}`;
      break;
    }

    case "browser_snapshot": {
      command = "snapshot";
      if (args.interactive !== false) command += " -i";
      if (args.compact) command += " -c";
      if (args.selector) command += ` -s "${args.selector}"`;
      if (args.depth) command += ` -d ${args.depth}`;
      
      // If includeHtml is true, run both snapshot and get HTML
      if (args.includeHtml) {
        const snapshotResult = await runAgentBrowser(command);
        const htmlResult = await runAgentBrowser(`eval "document.documentElement.outerHTML"`);
        
        const output = [];
        output.push("=== ACCESSIBILITY SNAPSHOT (use @refs for interactions) ===\n");
        output.push(snapshotResult.success ? snapshotResult.output : `Error: ${snapshotResult.error}`);
        output.push("\n\n=== FULL PAGE HTML ===\n");
        output.push(htmlResult.success ? htmlResult.output : `Error: ${htmlResult.error}`);
        
        return {
          content: [{ type: "text", text: output.join("") }],
          isError: !snapshotResult.success && !htmlResult.success,
        };
      }
      break;
    }

    case "browser_click":
      command = `click ${args.target}`;
      break;

    case "browser_fill":
      command = `fill ${args.target} "${args.text}"`;
      break;

    case "browser_type":
      command = `type ${args.target} "${args.text}"`;
      break;

    case "browser_press":
      command = `press ${args.key}`;
      break;

    case "browser_scroll":
      command = `scroll ${args.direction || "down"} ${args.pixels || 300}`;
      break;

    case "browser_hover":
      command = `hover ${args.target}`;
      break;

    case "browser_select":
      command = `select ${args.target} "${args.value}"`;
      break;

    case "browser_check":
      command = `check ${args.target}`;
      break;

    case "browser_uncheck":
      command = `uncheck ${args.target}`;
      break;

    case "browser_get":
      if (args.what === "title" || args.what === "url") {
        command = `get ${args.what}`;
      } else if (args.what === "attr") {
        command = `get attr ${args.target} ${args.attribute}`;
      } else {
        command = `get ${args.what} ${args.target}`;
      }
      break;

    case "browser_wait":
      if (args.text) {
        command = `wait --text "${args.text}"`;
      } else if (args.url) {
        command = `wait --url "${args.url}"`;
      } else if (args.load) {
        command = `wait --load ${args.load}`;
      } else if (args.target) {
        command = `wait ${args.target}`;
      } else {
        command = "wait 1000";
      }
      break;

    case "browser_screenshot":
      command = "screenshot";
      if (args.path) command += ` "${args.path}"`;
      if (args.full) command += " --full";
      break;

    case "browser_back":
      command = "back";
      break;

    case "browser_forward":
      command = "forward";
      break;

    case "browser_reload":
      command = "reload";
      break;

    case "browser_close":
      command = "close";
      break;

    case "browser_tabs":
      if (args.action === "new") {
        command = args.url ? `tab new "${args.url}"` : "tab new";
      } else if (args.action === "switch") {
        command = `tab ${args.index}`;
      } else if (args.action === "close") {
        command = args.index !== undefined ? `tab close ${args.index}` : "tab close";
      } else {
        command = "tab";
      }
      break;

    case "browser_cookies":
      if (args.action === "set") {
        command = `cookies set ${args.name} ${args.value}`;
      } else if (args.action === "clear") {
        command = "cookies clear";
      } else {
        command = "cookies";
      }
      break;

    case "browser_eval":
      command = `eval "${args.script.replace(/"/g, '\\"')}"`;
      break;

    case "browser_html":
      if (args.target) {
        // Get HTML of specific element using agent-browser's get html command
        command = `get html ${args.target}`;
      } else {
        // Get full page HTML
        command = `eval "document.documentElement.outerHTML"`;
      }
      break;

    case "browser_pause": {
      const message = args.message || "Waiting for user to continue...";
      
      // Show notification in terminal
      console.error(`\n${"=".repeat(60)}`);
      console.error(`🛑 PAUSED: ${message}`);
      console.error(`\nTo continue, run: agent-continue`);
      console.error(`${"=".repeat(60)}\n`);
      
      const result = await waitForUserSignal(message);
      
      return {
        content: [{ type: "text", text: `User continued after ${Math.round(result.elapsed/1000)}s. Proceeding with automation.` }],
        isError: false,
      };
    }

    case "browser_user_action": {
      const action = args.action;
      
      // Inject a visible notification in the browser
      const notifyScript = `
        (function() {
          const existing = document.getElementById('agent-browser-notify');
          if (existing) existing.remove();
          
          const div = document.createElement('div');
          div.id = 'agent-browser-notify';
          div.innerHTML = \`
            <div style="position:fixed;top:20px;left:50%;transform:translateX(-50%);z-index:999999;
                        background:linear-gradient(135deg,#667eea 0%,#764ba2 100%);color:white;
                        padding:20px 30px;border-radius:12px;font-family:system-ui,sans-serif;
                        box-shadow:0 10px 40px rgba(0,0,0,0.3);max-width:500px;text-align:center;">
              <div style="font-size:24px;margin-bottom:10px;">🤖 Agent Waiting</div>
              <div style="font-size:16px;margin-bottom:15px;">\${action}</div>
              <div style="font-size:13px;opacity:0.9;">When done, run: <code style="background:rgba(0,0,0,0.3);padding:4px 8px;border-radius:4px;">agent-continue</code></div>
            </div>
          \`;
          div.innerHTML = div.innerHTML.replace('\${action}', '${action.replace(/'/g, "\\'")}');
          document.body.appendChild(div);
        })()
      `.replace(/\n/g, ' ');
      
      // Show notification in browser
      await runAgentBrowser(`eval "${notifyScript.replace(/"/g, '\\"')}"`);
      
      // Also log to terminal
      console.error(`\n${"=".repeat(60)}`);
      console.error(`🤖 USER ACTION REQUIRED: ${action}`);
      console.error(`\nTo continue, run: agent-continue`);
      console.error(`${"=".repeat(60)}\n`);
      
      const result = await waitForUserSignal(action);
      
      // Remove notification from browser
      await runAgentBrowser(`eval "const el = document.getElementById('agent-browser-notify'); if (el) el.remove();"`);
      
      return {
        content: [{ type: "text", text: `User completed action "${action}" after ${Math.round(result.elapsed/1000)}s. Continuing automation.` }],
        isError: false,
      };
    }

    case "browser_highlight":
      command = `highlight ${args.target}`;
      break;

    case "browser_console":
      command = args.clear ? "console --clear" : "console";
      break;

    case "browser_errors":
      command = args.clear ? "errors --clear" : "errors";
      break;

    case "browser_network":
      command = "network requests";
      if (args.filter) command += ` --filter "${args.filter}"`;
      if (args.clear) command += " --clear";
      break;

    case "browser_set_viewport":
      command = `set viewport ${args.width} ${args.height}`;
      break;

    case "browser_set_device":
      command = `set device "${args.device}"`;
      break;

    case "browser_pdf":
      command = `pdf "${args.path}"`;
      break;

    case "browser_drag":
      command = `drag ${args.source} ${args.target}`;
      break;

    case "browser_upload":
      command = `upload ${args.target} ${args.files.map(f => `"${f}"`).join(" ")}`;
      break;

    case "browser_storage": {
      const storageType = args.type || "local";
      if (args.action === "set" && args.key && args.value !== undefined) {
        command = `storage ${storageType} set "${args.key}" "${args.value}"`;
      } else if (args.action === "clear") {
        command = `storage ${storageType} clear`;
      } else if (args.key) {
        command = `storage ${storageType} get "${args.key}"`;
      } else {
        command = `storage ${storageType}`;
      }
      break;
    }

    default:
      return {
        content: [{ type: "text", text: `Unknown tool: ${name}` }],
        isError: true,
      };
  }

  const result = await runAgentBrowser(command);

  return {
    content: [
      {
        type: "text",
        text: result.success
          ? result.output || "Done"
          : `Error: ${result.error}\n${result.output || ""}`,
      },
    ],
    isError: !result.success,
  };
});

// Start server
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error("agent-browser MCP server running");
}

main().catch(console.error);
