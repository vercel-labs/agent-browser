#!/usr/bin/env node
/**
 * MCP Server for Agentic Browsing with Patchright (Chromium only)
 *
 * This server provides browser automation tools through the Model Context Protocol.
 * It uses Patchright (Playwright fork) for stealth browsing capabilities.
 */

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
  type Tool,
} from '@modelcontextprotocol/sdk/types.js';
import { BrowserManager } from './browser.js';

// Single browser instance for the session
const browser = new BrowserManager();

// Server instructions for AI agents
const SERVER_INSTRUCTIONS = `Patchright Browser MCP Server - Agentic browsing with Chromium.

WORKFLOW:
1. Use browser_snapshot to get page structure with element refs (@e1, @e2, etc.)
2. Use refs for interactions: browser_click, browser_fill, browser_type
3. Use browser_snapshot again after actions to see updated state

TIPS:
- browser_fill clears input first, browser_type appends
- Use browser_wait for dynamic content
- browser_screenshot for visual debugging
- browser_evaluate for custom JavaScript`;

// Define MCP tools
const tools: Tool[] = [
  {
    name: 'browser_navigate',
    description: 'Navigate to a URL. Launches browser if not started.',
    inputSchema: {
      type: 'object',
      properties: {
        url: { type: 'string', description: 'URL to navigate to' },
        waitUntil: {
          type: 'string',
          enum: ['load', 'domcontentloaded', 'networkidle'],
          description: 'When to consider navigation complete',
        },
      },
      required: ['url'],
    },
  },
  {
    name: 'browser_snapshot',
    description:
      'Get accessibility tree with element refs (@e1, @e2, etc.). Always call this before interacting with elements.',
    inputSchema: {
      type: 'object',
      properties: {
        interactive: {
          type: 'boolean',
          description: 'Only show interactive elements (buttons, links, inputs)',
          default: true,
        },
        selector: { type: 'string', description: 'CSS selector to scope snapshot' },
      },
    },
  },
  {
    name: 'browser_click',
    description: 'Click an element by ref (@e1) or CSS selector',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
      },
      required: ['target'],
    },
  },
  {
    name: 'browser_fill',
    description: 'Clear and fill an input field',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
        value: { type: 'string', description: 'Text to fill' },
      },
      required: ['target', 'value'],
    },
  },
  {
    name: 'browser_type',
    description: 'Type text into an element (appends, does not clear)',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
        text: { type: 'string', description: 'Text to type' },
        delay: { type: 'number', description: 'Delay between keystrokes in ms' },
      },
      required: ['target', 'text'],
    },
  },
  {
    name: 'browser_press',
    description: 'Press a key (Enter, Tab, Escape, Control+a, etc.)',
    inputSchema: {
      type: 'object',
      properties: {
        key: { type: 'string', description: 'Key to press' },
        target: { type: 'string', description: 'Optional element to focus first' },
      },
      required: ['key'],
    },
  },
  {
    name: 'browser_scroll',
    description: 'Scroll the page or element',
    inputSchema: {
      type: 'object',
      properties: {
        direction: { type: 'string', enum: ['up', 'down', 'left', 'right'] },
        amount: { type: 'number', description: 'Pixels to scroll', default: 300 },
        selector: { type: 'string', description: 'Element to scroll into view' },
      },
    },
  },
  {
    name: 'browser_hover',
    description: 'Hover over an element',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
      },
      required: ['target'],
    },
  },
  {
    name: 'browser_select',
    description: 'Select option(s) from a dropdown',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
        values: {
          oneOf: [{ type: 'string' }, { type: 'array', items: { type: 'string' } }],
          description: 'Option value(s) to select',
        },
      },
      required: ['target', 'values'],
    },
  },
  {
    name: 'browser_check',
    description: 'Check a checkbox',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
      },
      required: ['target'],
    },
  },
  {
    name: 'browser_uncheck',
    description: 'Uncheck a checkbox',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
      },
      required: ['target'],
    },
  },
  {
    name: 'browser_screenshot',
    description: 'Take a screenshot of the page or element',
    inputSchema: {
      type: 'object',
      properties: {
        path: { type: 'string', description: 'File path to save screenshot' },
        fullPage: { type: 'boolean', description: 'Capture full page' },
        selector: { type: 'string', description: 'Element to screenshot' },
      },
    },
  },
  {
    name: 'browser_wait',
    description: 'Wait for element, time, or page load state',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string', description: 'Element selector to wait for' },
        timeout: { type: 'number', description: 'Timeout in milliseconds' },
        state: {
          type: 'string',
          enum: ['attached', 'detached', 'visible', 'hidden'],
          description: 'Element state to wait for',
        },
      },
    },
  },
  {
    name: 'browser_evaluate',
    description: 'Execute JavaScript in the browser',
    inputSchema: {
      type: 'object',
      properties: {
        script: { type: 'string', description: 'JavaScript code to execute' },
      },
      required: ['script'],
    },
  },
  {
    name: 'browser_back',
    description: 'Go back in browser history',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'browser_forward',
    description: 'Go forward in browser history',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'browser_reload',
    description: 'Reload the current page',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'browser_tabs',
    description: 'List all open tabs',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'browser_tab_new',
    description: 'Open a new tab',
    inputSchema: {
      type: 'object',
      properties: {
        url: { type: 'string', description: 'URL to open in new tab' },
      },
    },
  },
  {
    name: 'browser_tab_switch',
    description: 'Switch to a specific tab',
    inputSchema: {
      type: 'object',
      properties: {
        index: { type: 'number', description: 'Tab index (0-based)' },
      },
      required: ['index'],
    },
  },
  {
    name: 'browser_tab_close',
    description: 'Close a tab',
    inputSchema: {
      type: 'object',
      properties: {
        index: { type: 'number', description: 'Tab index to close (current if omitted)' },
      },
    },
  },
  {
    name: 'browser_get_url',
    description: 'Get current page URL',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'browser_get_title',
    description: 'Get current page title',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'browser_get_text',
    description: 'Get text content of an element',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
      },
      required: ['target'],
    },
  },
  {
    name: 'browser_get_attribute',
    description: 'Get attribute value of an element',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'Element ref (@e1) or CSS selector' },
        attribute: { type: 'string', description: 'Attribute name' },
      },
      required: ['target', 'attribute'],
    },
  },
  {
    name: 'browser_get_html',
    description: 'Get HTML content of page or element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string', description: 'Element selector (full page if omitted)' },
      },
    },
  },
  {
    name: 'browser_upload',
    description: 'Upload files to a file input',
    inputSchema: {
      type: 'object',
      properties: {
        target: { type: 'string', description: 'File input element' },
        files: {
          oneOf: [{ type: 'string' }, { type: 'array', items: { type: 'string' } }],
          description: 'File path(s) to upload',
        },
      },
      required: ['target', 'files'],
    },
  },
  {
    name: 'browser_close',
    description: 'Close the browser',
    inputSchema: { type: 'object', properties: {} },
  },
];

// Create MCP server
const server = new Server(
  { name: 'patchright-browser', version: '1.0.0' },
  { capabilities: { tools: {} }, instructions: SERVER_INSTRUCTIONS }
);

// List tools handler
server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools }));

// Auto-launch browser if needed
async function ensureBrowser(headless: boolean = true): Promise<void> {
  if (!browser.isLaunched()) {
    await browser.launch({
      id: 'auto',
      action: 'launch',
      browser: 'chromium',
      headless: process.env.PATCHRIGHT_HEADED !== '1' && headless,
      executablePath: process.env.PATCHRIGHT_EXECUTABLE_PATH,
      userAgent: process.env.PATCHRIGHT_USER_AGENT,
      proxy: process.env.PATCHRIGHT_PROXY ? { server: process.env.PATCHRIGHT_PROXY } : undefined,
    });
  }
}

// Format response helper
function formatResult(data: unknown): string {
  if (typeof data === 'string') return data;
  return JSON.stringify(data, null, 2);
}

// Tool call handler
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;
  const params = (args ?? {}) as Record<string, unknown>;

  try {
    switch (name) {
      case 'browser_navigate': {
        await ensureBrowser();
        const page = browser.getPage();
        await page.goto(params.url as string, {
          waitUntil: (params.waitUntil as 'load' | 'domcontentloaded' | 'networkidle') ?? 'load',
        });
        return {
          content: [{ type: 'text', text: `Navigated to ${page.url()}` }],
        };
      }

      case 'browser_snapshot': {
        await ensureBrowser();
        const snapshot = await browser.getSnapshot({
          interactive: params.interactive !== false,
          selector: params.selector as string | undefined,
        });
        return {
          content: [{ type: 'text', text: snapshot.tree || '(empty page)' }],
        };
      }

      case 'browser_click': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        await locator.click();
        return { content: [{ type: 'text', text: `Clicked ${params.target}` }] };
      }

      case 'browser_fill': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        await locator.fill(params.value as string);
        return { content: [{ type: 'text', text: `Filled ${params.target}` }] };
      }

      case 'browser_type': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        await locator.pressSequentially(params.text as string, {
          delay: params.delay as number | undefined,
        });
        return { content: [{ type: 'text', text: `Typed into ${params.target}` }] };
      }

      case 'browser_press': {
        await ensureBrowser();
        const page = browser.getPage();
        if (params.target) {
          await page.press(params.target as string, params.key as string);
        } else {
          await page.keyboard.press(params.key as string);
        }
        return { content: [{ type: 'text', text: `Pressed ${params.key}` }] };
      }

      case 'browser_scroll': {
        await ensureBrowser();
        const page = browser.getPage();
        if (params.selector) {
          const element = page.locator(params.selector as string);
          await element.scrollIntoViewIfNeeded();
          return { content: [{ type: 'text', text: `Scrolled ${params.selector} into view` }] };
        }
        const amount = (params.amount as number) ?? 300;
        let deltaX = 0,
          deltaY = 0;
        switch (params.direction) {
          case 'up':
            deltaY = -amount;
            break;
          case 'down':
            deltaY = amount;
            break;
          case 'left':
            deltaX = -amount;
            break;
          case 'right':
            deltaX = amount;
            break;
          default:
            deltaY = amount;
        }
        await page.evaluate(`window.scrollBy(${deltaX}, ${deltaY})`);
        return {
          content: [{ type: 'text', text: `Scrolled ${params.direction ?? 'down'} ${amount}px` }],
        };
      }

      case 'browser_hover': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        await locator.hover();
        return { content: [{ type: 'text', text: `Hovered over ${params.target}` }] };
      }

      case 'browser_select': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        const values = Array.isArray(params.values) ? params.values : [params.values];
        await locator.selectOption(values as string[]);
        return { content: [{ type: 'text', text: `Selected ${JSON.stringify(values)}` }] };
      }

      case 'browser_check': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        await locator.check();
        return { content: [{ type: 'text', text: `Checked ${params.target}` }] };
      }

      case 'browser_uncheck': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        await locator.uncheck();
        return { content: [{ type: 'text', text: `Unchecked ${params.target}` }] };
      }

      case 'browser_screenshot': {
        await ensureBrowser();
        const page = browser.getPage();
        const options: { path?: string; fullPage?: boolean } = {};
        if (params.path) options.path = params.path as string;
        if (params.fullPage) options.fullPage = true;

        if (params.selector) {
          const locator = browser.getLocator(params.selector as string);
          const buffer = await locator.screenshot(options);
          if (!params.path) {
            return {
              content: [
                { type: 'text', text: 'Screenshot captured' },
                { type: 'image', data: buffer.toString('base64'), mimeType: 'image/png' },
              ],
            };
          }
        } else {
          const buffer = await page.screenshot(options);
          if (!params.path) {
            return {
              content: [
                { type: 'text', text: 'Screenshot captured' },
                { type: 'image', data: buffer.toString('base64'), mimeType: 'image/png' },
              ],
            };
          }
        }
        return { content: [{ type: 'text', text: `Screenshot saved to ${params.path}` }] };
      }

      case 'browser_wait': {
        await ensureBrowser();
        const page = browser.getPage();
        if (params.selector) {
          await page.waitForSelector(params.selector as string, {
            state: (params.state as 'attached' | 'detached' | 'visible' | 'hidden') ?? 'visible',
            timeout: params.timeout as number | undefined,
          });
          return { content: [{ type: 'text', text: `Found ${params.selector}` }] };
        }
        if (params.timeout) {
          await page.waitForTimeout(params.timeout as number);
          return { content: [{ type: 'text', text: `Waited ${params.timeout}ms` }] };
        }
        await page.waitForLoadState('load');
        return { content: [{ type: 'text', text: 'Page loaded' }] };
      }

      case 'browser_evaluate': {
        await ensureBrowser();
        const page = browser.getPage();
        const result = await page.evaluate(params.script as string);
        return { content: [{ type: 'text', text: formatResult(result) }] };
      }

      case 'browser_back': {
        await ensureBrowser();
        const page = browser.getPage();
        await page.goBack();
        return { content: [{ type: 'text', text: `Navigated back to ${page.url()}` }] };
      }

      case 'browser_forward': {
        await ensureBrowser();
        const page = browser.getPage();
        await page.goForward();
        return { content: [{ type: 'text', text: `Navigated forward to ${page.url()}` }] };
      }

      case 'browser_reload': {
        await ensureBrowser();
        const page = browser.getPage();
        await page.reload();
        return { content: [{ type: 'text', text: `Reloaded ${page.url()}` }] };
      }

      case 'browser_tabs': {
        await ensureBrowser();
        const tabs = await browser.listTabs();
        const tabList = tabs
          .map((t) => `${t.active ? '>' : ' '} [${t.index}] ${t.title || t.url}`)
          .join('\n');
        return { content: [{ type: 'text', text: tabList || 'No tabs open' }] };
      }

      case 'browser_tab_new': {
        await ensureBrowser();
        const result = await browser.newTab();
        if (params.url) {
          const page = browser.getPage();
          await page.goto(params.url as string);
        }
        return { content: [{ type: 'text', text: `Opened tab ${result.index}` }] };
      }

      case 'browser_tab_switch': {
        await ensureBrowser();
        const result = await browser.switchTo(params.index as number);
        return {
          content: [{ type: 'text', text: `Switched to tab ${result.index}: ${result.url}` }],
        };
      }

      case 'browser_tab_close': {
        await ensureBrowser();
        const result = await browser.closeTab(params.index as number | undefined);
        return {
          content: [
            { type: 'text', text: `Closed tab ${result.closed}, ${result.remaining} remaining` },
          ],
        };
      }

      case 'browser_get_url': {
        await ensureBrowser();
        const page = browser.getPage();
        return { content: [{ type: 'text', text: page.url() }] };
      }

      case 'browser_get_title': {
        await ensureBrowser();
        const page = browser.getPage();
        const title = await page.title();
        return { content: [{ type: 'text', text: title }] };
      }

      case 'browser_get_text': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        const text = await locator.textContent();
        return { content: [{ type: 'text', text: text ?? '' }] };
      }

      case 'browser_get_attribute': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        const value = await locator.getAttribute(params.attribute as string);
        return { content: [{ type: 'text', text: value ?? '(null)' }] };
      }

      case 'browser_get_html': {
        await ensureBrowser();
        const page = browser.getPage();
        let html: string;
        if (params.selector) {
          html = await page.locator(params.selector as string).innerHTML();
        } else {
          html = await page.content();
        }
        return { content: [{ type: 'text', text: html }] };
      }

      case 'browser_upload': {
        await ensureBrowser();
        const locator = browser.getLocator(params.target as string);
        const files = Array.isArray(params.files) ? params.files : [params.files];
        await locator.setInputFiles(files as string[]);
        return { content: [{ type: 'text', text: `Uploaded ${files.length} file(s)` }] };
      }

      case 'browser_close': {
        await browser.close();
        return { content: [{ type: 'text', text: 'Browser closed' }] };
      }

      default:
        return {
          content: [{ type: 'text', text: `Unknown tool: ${name}` }],
          isError: true,
        };
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      content: [{ type: 'text', text: `Error: ${message}` }],
      isError: true,
    };
  }
});

// Cleanup on exit
process.on('SIGINT', async () => {
  await browser.close();
  process.exit(0);
});

process.on('SIGTERM', async () => {
  await browser.close();
  process.exit(0);
});

// Start server
async function main(): Promise<void> {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error('Patchright Browser MCP Server running');
}

main().catch((err) => {
  console.error('MCP Server error:', err);
  process.exit(1);
});
