#!/usr/bin/env node

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
  Tool,
} from '@modelcontextprotocol/sdk/types.js';
import { BrowserManager } from '../../dist/browser.js';

// Session-based browser managers
const browsers = new Map<string, BrowserManager>();

function getBrowser(session: string = 'default'): BrowserManager {
  if (!browsers.has(session)) {
    browsers.set(session, new BrowserManager());
  }
  return browsers.get(session)!;
}

/**
 * MCP Tools for agent-browser - ALL FEATURES
 */
const tools: Tool[] = [
  // ============= NAVIGATION =============
  {
    name: 'browser_navigate',
    description: 'Navigate to a URL in the browser',
    inputSchema: {
      type: 'object',
      properties: {
        url: { type: 'string', description: 'URL to navigate to' },
        session: { type: 'string', default: 'default' },
      },
      required: ['url'],
    },
  },
  {
    name: 'browser_back',
    description: 'Go back in browser history',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_forward',
    description: 'Go forward in browser history',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_reload',
    description: 'Reload the current page',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= INTERACTIONS =============
  {
    name: 'browser_click',
    description: 'Click an element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string', description: 'Selector (ref or CSS)' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_dblclick',
    description: 'Double-click an element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_fill',
    description: 'Clear and fill an input field',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        value: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector', 'value'],
    },
  },
  {
    name: 'browser_type',
    description: 'Type text into an element (without clearing)',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        text: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector', 'text'],
    },
  },
  {
    name: 'browser_press',
    description: 'Press a key (e.g., Enter, Tab, Control+a)',
    inputSchema: {
      type: 'object',
      properties: {
        key: { type: 'string', description: 'Key to press' },
        session: { type: 'string', default: 'default' },
      },
      required: ['key'],
    },
  },
  {
    name: 'browser_hover',
    description: 'Hover over an element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_focus',
    description: 'Focus an element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_select',
    description: 'Select option in a dropdown',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        value: { type: 'string', description: 'Option value to select' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector', 'value'],
    },
  },
  {
    name: 'browser_check',
    description: 'Check a checkbox',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_uncheck',
    description: 'Uncheck a checkbox',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_scroll',
    description: 'Scroll the page',
    inputSchema: {
      type: 'object',
      properties: {
        direction: {
          type: 'string',
          enum: ['up', 'down', 'left', 'right'],
          description: 'Direction to scroll',
        },
        pixels: { type: 'number', description: 'Pixels to scroll (default: 100)' },
        session: { type: 'string', default: 'default' },
      },
      required: ['direction'],
    },
  },
  {
    name: 'browser_scroll_into_view',
    description: 'Scroll element into view',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_drag',
    description: 'Drag and drop from source to target',
    inputSchema: {
      type: 'object',
      properties: {
        source: { type: 'string', description: 'Source selector' },
        target: { type: 'string', description: 'Target selector' },
        session: { type: 'string', default: 'default' },
      },
      required: ['source', 'target'],
    },
  },
  {
    name: 'browser_upload',
    description: 'Upload files to file input',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        files: {
          type: 'array',
          items: { type: 'string' },
          description: 'File paths to upload',
        },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector', 'files'],
    },
  },

  // ============= GET INFO =============
  {
    name: 'browser_snapshot',
    description: 'Get accessibility snapshot with refs',
    inputSchema: {
      type: 'object',
      properties: {
        interactive: { type: 'boolean', default: true },
        compact: { type: 'boolean', default: true },
        depth: { type: 'number' },
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_get_text',
    description: 'Get text content from element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_get_html',
    description: 'Get innerHTML of element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_get_value',
    description: 'Get value of input element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_get_attribute',
    description: 'Get attribute value from element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        attribute: { type: 'string', description: 'Attribute name' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector', 'attribute'],
    },
  },
  {
    name: 'browser_get_title',
    description: 'Get page title',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_get_url',
    description: 'Get current URL',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_get_count',
    description: 'Count matching elements',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_get_bounding_box',
    description: 'Get bounding box of element',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },

  // ============= CHECK STATE =============
  {
    name: 'browser_is_visible',
    description: 'Check if element is visible',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_is_enabled',
    description: 'Check if element is enabled',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_is_checked',
    description: 'Check if checkbox is checked',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },

  // ============= WAIT =============
  {
    name: 'browser_wait',
    description: 'Wait for element, time, or condition',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string', description: 'Wait for element' },
        timeout: { type: 'number', description: 'Timeout in ms' },
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= MOUSE =============
  {
    name: 'browser_mouse_move',
    description: 'Move mouse to coordinates',
    inputSchema: {
      type: 'object',
      properties: {
        x: { type: 'number' },
        y: { type: 'number' },
        session: { type: 'string', default: 'default' },
      },
      required: ['x', 'y'],
    },
  },

  // ============= SCREENSHOT & PDF =============
  {
    name: 'browser_screenshot',
    description: 'Take screenshot',
    inputSchema: {
      type: 'object',
      properties: {
        fullPage: { type: 'boolean', default: false },
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_pdf',
    description: 'Save page as PDF',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= EVALUATE =============
  {
    name: 'browser_evaluate',
    description: 'Execute JavaScript',
    inputSchema: {
      type: 'object',
      properties: {
        script: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['script'],
    },
  },

  // ============= TABS =============
  {
    name: 'browser_tab_new',
    description: 'Open new tab',
    inputSchema: {
      type: 'object',
      properties: {
        url: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_tab_switch',
    description: 'Switch to tab by index',
    inputSchema: {
      type: 'object',
      properties: {
        index: { type: 'number' },
        session: { type: 'string', default: 'default' },
      },
      required: ['index'],
    },
  },
  {
    name: 'browser_tab_close',
    description: 'Close tab',
    inputSchema: {
      type: 'object',
      properties: {
        index: { type: 'number' },
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_tab_list',
    description: 'List all tabs',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= COOKIES =============
  {
    name: 'browser_cookies_get',
    description: 'Get all cookies',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_cookies_set',
    description: 'Set a cookie',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string' },
        value: { type: 'string' },
        domain: { type: 'string' },
        path: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['name', 'value'],
    },
  },
  {
    name: 'browser_cookies_clear',
    description: 'Clear all cookies',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= STORAGE =============
  {
    name: 'browser_storage_get',
    description: 'Get localStorage or sessionStorage',
    inputSchema: {
      type: 'object',
      properties: {
        type: { type: 'string', enum: ['local', 'session'] },
        key: { type: 'string', description: 'Optional key' },
        session: { type: 'string', default: 'default' },
      },
      required: ['type'],
    },
  },
  {
    name: 'browser_storage_set',
    description: 'Set localStorage or sessionStorage value',
    inputSchema: {
      type: 'object',
      properties: {
        type: { type: 'string', enum: ['local', 'session'] },
        key: { type: 'string' },
        value: { type: 'string' },
        session: { type: 'string', default: 'default' },
      },
      required: ['type', 'key', 'value'],
    },
  },
  {
    name: 'browser_storage_clear',
    description: 'Clear storage',
    inputSchema: {
      type: 'object',
      properties: {
        type: { type: 'string', enum: ['local', 'session'] },
        session: { type: 'string', default: 'default' },
      },
      required: ['type'],
    },
  },

  // ============= FRAMES =============
  {
    name: 'browser_frame_switch',
    description: 'Switch to iframe',
    inputSchema: {
      type: 'object',
      properties: {
        selector: { type: 'string', description: 'Frame selector' },
        session: { type: 'string', default: 'default' },
      },
      required: ['selector'],
    },
  },
  {
    name: 'browser_frame_main',
    description: 'Switch back to main frame',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= DIALOGS =============
  {
    name: 'browser_dialog_accept',
    description: 'Accept dialog (alert/confirm/prompt)',
    inputSchema: {
      type: 'object',
      properties: {
        text: { type: 'string', description: 'Text for prompt' },
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_dialog_dismiss',
    description: 'Dismiss dialog',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= NETWORK =============
  {
    name: 'browser_network_requests',
    description: 'Get tracked network requests',
    inputSchema: {
      type: 'object',
      properties: {
        filter: { type: 'string', description: 'Filter by URL substring' },
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= SETTINGS =============
  {
    name: 'browser_set_viewport',
    description: 'Set viewport size',
    inputSchema: {
      type: 'object',
      properties: {
        width: { type: 'number' },
        height: { type: 'number' },
        session: { type: 'string', default: 'default' },
      },
      required: ['width', 'height'],
    },
  },
  {
    name: 'browser_set_geolocation',
    description: 'Set geolocation',
    inputSchema: {
      type: 'object',
      properties: {
        latitude: { type: 'number' },
        longitude: { type: 'number' },
        accuracy: { type: 'number', default: 0 },
        session: { type: 'string', default: 'default' },
      },
      required: ['latitude', 'longitude'],
    },
  },

  // ============= DEBUG =============
  {
    name: 'browser_console',
    description: 'Get console messages',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_errors',
    description: 'Get page errors',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },

  // ============= SESSION =============
  {
    name: 'browser_close',
    description: 'Close browser',
    inputSchema: {
      type: 'object',
      properties: {
        session: { type: 'string', default: 'default' },
      },
    },
  },
  {
    name: 'browser_session_list',
    description: 'List active sessions',
    inputSchema: {
      type: 'object',
      properties: {},
    },
  },
];

/**
 * Handle tool execution
 */
async function handleToolCall(name: string, args: any): Promise<any> {
  const session = args.session || 'default';
  const browser = getBrowser(session);

  try {
    // Auto-launch browser if needed
    if (!browser.isLaunched() && !name.includes('session_list') && name !== 'browser_close') {
      await browser.launch({
        id: 'auto',
        action: 'launch',
        headless: false,
      });
    }

    switch (name) {
      // ============= NAVIGATION =============
      case 'browser_navigate': {
        const page = browser.getPage();
        await page.goto(args.url, { waitUntil: 'load' });
        const title = await page.title();
        return {
          content: [{ type: 'text', text: `Navigated to ${args.url}\nTitle: ${title}` }],
        };
      }

      case 'browser_back': {
        const page = browser.getPage();
        await page.goBack();
        return { content: [{ type: 'text', text: 'Navigated back' }] };
      }

      case 'browser_forward': {
        const page = browser.getPage();
        await page.goForward();
        return { content: [{ type: 'text', text: 'Navigated forward' }] };
      }

      case 'browser_reload': {
        const page = browser.getPage();
        await page.reload();
        return { content: [{ type: 'text', text: 'Page reloaded' }] };
      }

      // ============= INTERACTIONS =============
      case 'browser_click': {
        const locator = browser.getLocator(args.selector);
        await locator.click();
        return { content: [{ type: 'text', text: `Clicked: ${args.selector}` }] };
      }

      case 'browser_dblclick': {
        const locator = browser.getLocator(args.selector);
        await locator.dblclick();
        return { content: [{ type: 'text', text: `Double-clicked: ${args.selector}` }] };
      }

      case 'browser_fill': {
        const locator = browser.getLocator(args.selector);
        await locator.fill(args.value);
        return {
          content: [{ type: 'text', text: `Filled ${args.selector} with: ${args.value}` }],
        };
      }

      case 'browser_type': {
        const locator = browser.getLocator(args.selector);
        await locator.pressSequentially(args.text);
        return { content: [{ type: 'text', text: `Typed into ${args.selector}` }] };
      }

      case 'browser_press': {
        const page = browser.getPage();
        await page.keyboard.press(args.key);
        return { content: [{ type: 'text', text: `Pressed: ${args.key}` }] };
      }

      case 'browser_hover': {
        const locator = browser.getLocator(args.selector);
        await locator.hover();
        return { content: [{ type: 'text', text: `Hovered: ${args.selector}` }] };
      }

      case 'browser_focus': {
        const locator = browser.getLocator(args.selector);
        await locator.focus();
        return { content: [{ type: 'text', text: `Focused: ${args.selector}` }] };
      }

      case 'browser_select': {
        const locator = browser.getLocator(args.selector);
        await locator.selectOption(args.value);
        return {
          content: [{ type: 'text', text: `Selected ${args.value} in ${args.selector}` }],
        };
      }

      case 'browser_check': {
        const locator = browser.getLocator(args.selector);
        await locator.check();
        return { content: [{ type: 'text', text: `Checked: ${args.selector}` }] };
      }

      case 'browser_uncheck': {
        const locator = browser.getLocator(args.selector);
        await locator.uncheck();
        return { content: [{ type: 'text', text: `Unchecked: ${args.selector}` }] };
      }

      case 'browser_scroll': {
        const page = browser.getPage();
        const pixels = args.pixels || 100;
        const scroll = {
          up: `window.scrollBy(0, -${pixels})`,
          down: `window.scrollBy(0, ${pixels})`,
          left: `window.scrollBy(-${pixels}, 0)`,
          right: `window.scrollBy(${pixels}, 0)`,
        };
        await page.evaluate(scroll[args.direction as keyof typeof scroll]);
        return {
          content: [{ type: 'text', text: `Scrolled ${args.direction} ${pixels}px` }],
        };
      }

      case 'browser_scroll_into_view': {
        const locator = browser.getLocator(args.selector);
        await locator.scrollIntoViewIfNeeded();
        return {
          content: [{ type: 'text', text: `Scrolled ${args.selector} into view` }],
        };
      }

      case 'browser_drag': {
        const source = browser.getLocator(args.source);
        const target = browser.getLocator(args.target);
        await source.dragTo(target);
        return {
          content: [{ type: 'text', text: `Dragged ${args.source} to ${args.target}` }],
        };
      }

      case 'browser_upload': {
        const locator = browser.getLocator(args.selector);
        await locator.setInputFiles(args.files);
        return {
          content: [
            {
              type: 'text',
              text: `Uploaded ${args.files.length} file(s) to ${args.selector}`,
            },
          ],
        };
      }

      // ============= GET INFO =============
      case 'browser_snapshot': {
        const snapshot = await browser.getSnapshot({
          interactive: args.interactive !== false,
          compact: args.compact !== false,
          maxDepth: args.depth,
        });
        return {
          content: [
            {
              type: 'text',
              text: `Page snapshot:\n\n${snapshot.tree}\n\nUse refs like @e1, @e2 to interact.`,
            },
          ],
        };
      }

      case 'browser_get_text': {
        const locator = browser.getLocator(args.selector);
        const text = await locator.textContent();
        return {
          content: [{ type: 'text', text: `Text: ${text || '(empty)'}` }],
        };
      }

      case 'browser_get_html': {
        const locator = browser.getLocator(args.selector);
        const html = await locator.innerHTML();
        return { content: [{ type: 'text', text: `HTML:\n${html}` }] };
      }

      case 'browser_get_value': {
        const locator = browser.getLocator(args.selector);
        const value = await locator.inputValue();
        return { content: [{ type: 'text', text: `Value: ${value}` }] };
      }

      case 'browser_get_attribute': {
        const locator = browser.getLocator(args.selector);
        const attr = await locator.getAttribute(args.attribute);
        return {
          content: [{ type: 'text', text: `${args.attribute}: ${attr || '(null)'}` }],
        };
      }

      case 'browser_get_title': {
        const page = browser.getPage();
        const title = await page.title();
        return { content: [{ type: 'text', text: `Title: ${title}` }] };
      }

      case 'browser_get_url': {
        const page = browser.getPage();
        const url = page.url();
        return { content: [{ type: 'text', text: `URL: ${url}` }] };
      }

      case 'browser_get_count': {
        const locator = browser.getLocator(args.selector);
        const count = await locator.count();
        return {
          content: [{ type: 'text', text: `Count: ${count} element(s)` }],
        };
      }

      case 'browser_get_bounding_box': {
        const locator = browser.getLocator(args.selector);
        const box = await locator.boundingBox();
        return {
          content: [
            {
              type: 'text',
              text: box
                ? `Box: x=${box.x}, y=${box.y}, width=${box.width}, height=${box.height}`
                : 'Element not visible',
            },
          ],
        };
      }

      // ============= CHECK STATE =============
      case 'browser_is_visible': {
        const locator = browser.getLocator(args.selector);
        const visible = await locator.isVisible();
        return { content: [{ type: 'text', text: `Visible: ${visible}` }] };
      }

      case 'browser_is_enabled': {
        const locator = browser.getLocator(args.selector);
        const enabled = await locator.isEnabled();
        return { content: [{ type: 'text', text: `Enabled: ${enabled}` }] };
      }

      case 'browser_is_checked': {
        const locator = browser.getLocator(args.selector);
        const checked = await locator.isChecked();
        return { content: [{ type: 'text', text: `Checked: ${checked}` }] };
      }

      // ============= WAIT =============
      case 'browser_wait': {
        if (args.selector) {
          const locator = browser.getLocator(args.selector);
          await locator.waitFor({
            state: 'visible',
            timeout: args.timeout,
          });
          return {
            content: [{ type: 'text', text: `Waited for ${args.selector} to be visible` }],
          };
        } else if (args.timeout) {
          await new Promise((resolve) => setTimeout(resolve, args.timeout));
          return {
            content: [{ type: 'text', text: `Waited ${args.timeout}ms` }],
          };
        }
        throw new Error('Either selector or timeout required');
      }

      // ============= MOUSE =============
      case 'browser_mouse_move': {
        const page = browser.getPage();
        await page.mouse.move(args.x, args.y);
        return {
          content: [{ type: 'text', text: `Moved mouse to (${args.x}, ${args.y})` }],
        };
      }

      // ============= SCREENSHOT & PDF =============
      case 'browser_screenshot': {
        const page = browser.getPage();
        const buffer = await page.screenshot({
          fullPage: args.fullPage || false,
          type: 'png',
        });
        return {
          content: [
            {
              type: 'image',
              data: buffer.toString('base64'),
              mimeType: 'image/png',
            },
          ],
        };
      }

      case 'browser_pdf': {
        const page = browser.getPage();
        const buffer = await page.pdf();
        return {
          content: [
            {
              type: 'resource',
              resource: {
                uri: `data:application/pdf;base64,${buffer.toString('base64')}`,
                mimeType: 'application/pdf',
              },
            },
          ],
        };
      }

      // ============= EVALUATE =============
      case 'browser_evaluate': {
        const page = browser.getPage();
        const result = await page.evaluate(args.script);
        return {
          content: [{ type: 'text', text: `Result:\n${JSON.stringify(result, null, 2)}` }],
        };
      }

      // ============= TABS =============
      case 'browser_tab_new': {
        const result = await browser.newTab();
        if (args.url) {
          const page = browser.getPage();
          await page.goto(args.url, { waitUntil: 'load' });
        }
        return {
          content: [
            {
              type: 'text',
              text: `New tab ${result.index} (total: ${result.total})${args.url ? `\nNavigated to: ${args.url}` : ''}`,
            },
          ],
        };
      }

      case 'browser_tab_switch': {
        const result = await browser.switchTo(args.index);
        return {
          content: [{ type: 'text', text: `Switched to tab ${result.index}\nURL: ${result.url}` }],
        };
      }

      case 'browser_tab_close': {
        const result = await browser.closeTab(args.index);
        return {
          content: [
            {
              type: 'text',
              text: `Closed tab ${result.closed}\nRemaining: ${result.remaining}`,
            },
          ],
        };
      }

      case 'browser_tab_list': {
        const tabs = await browser.listTabs();
        const list = tabs
          .map(
            (t) => `[${t.index}]${t.active ? ' *' : '  '} ${t.title || '(no title)'}\n     ${t.url}`
          )
          .join('\n');
        return {
          content: [{ type: 'text', text: `Tabs (* = active):\n\n${list}` }],
        };
      }

      // ============= COOKIES =============
      case 'browser_cookies_get': {
        const context = browser.getPage().context();
        const cookies = await context.cookies();
        return {
          content: [{ type: 'text', text: `Cookies:\n${JSON.stringify(cookies, null, 2)}` }],
        };
      }

      case 'browser_cookies_set': {
        const context = browser.getPage().context();
        await context.addCookies([
          {
            name: args.name,
            value: args.value,
            domain: args.domain || new URL(browser.getPage().url()).hostname,
            path: args.path || '/',
          },
        ]);
        return {
          content: [{ type: 'text', text: `Set cookie: ${args.name}` }],
        };
      }

      case 'browser_cookies_clear': {
        const context = browser.getPage().context();
        await context.clearCookies();
        return { content: [{ type: 'text', text: 'Cleared all cookies' }] };
      }

      // ============= STORAGE =============
      case 'browser_storage_get': {
        const page = browser.getPage();
        const storageType = args.type === 'local' ? 'localStorage' : 'sessionStorage';
        const result = await page.evaluate(
          ({ type, key }) => {
            const storage = type === 'local' ? localStorage : sessionStorage;
            if (key) {
              return storage.getItem(key);
            }
            return JSON.stringify(storage);
          },
          { type: args.type, key: args.key }
        );
        return {
          content: [{ type: 'text', text: `${storageType}:\n${result}` }],
        };
      }

      case 'browser_storage_set': {
        const page = browser.getPage();
        await page.evaluate(
          ({ type, key, value }) => {
            const storage = type === 'local' ? localStorage : sessionStorage;
            storage.setItem(key, value);
          },
          { type: args.type, key: args.key, value: args.value }
        );
        return {
          content: [{ type: 'text', text: `Set ${args.type}Storage[${args.key}]` }],
        };
      }

      case 'browser_storage_clear': {
        const page = browser.getPage();
        await page.evaluate(
          ({ type }) => {
            const storage = type === 'local' ? localStorage : sessionStorage;
            storage.clear();
          },
          { type: args.type }
        );
        return {
          content: [{ type: 'text', text: `Cleared ${args.type}Storage` }],
        };
      }

      // ============= FRAMES =============
      case 'browser_frame_switch': {
        await browser.switchToFrame({ selector: args.selector });
        return {
          content: [{ type: 'text', text: `Switched to frame: ${args.selector}` }],
        };
      }

      case 'browser_frame_main': {
        browser.switchToMainFrame();
        return { content: [{ type: 'text', text: 'Switched to main frame' }] };
      }

      // ============= DIALOGS =============
      case 'browser_dialog_accept': {
        browser.setDialogHandler('accept', args.text);
        return { content: [{ type: 'text', text: 'Dialog handler set to accept' }] };
      }

      case 'browser_dialog_dismiss': {
        browser.setDialogHandler('dismiss');
        return {
          content: [{ type: 'text', text: 'Dialog handler set to dismiss' }],
        };
      }

      // ============= NETWORK =============
      case 'browser_network_requests': {
        const requests = browser.getRequests(args.filter);
        const list = requests.map((r: any) => `${r.method} ${r.url}`).join('\n');
        return {
          content: [
            { type: 'text', text: `Requests (${requests.length}):\n\n${list || '(none)'}` },
          ],
        };
      }

      // ============= SETTINGS =============
      case 'browser_set_viewport': {
        const page = browser.getPage();
        await page.setViewportSize({
          width: args.width,
          height: args.height,
        });
        return {
          content: [{ type: 'text', text: `Viewport set to ${args.width}x${args.height}` }],
        };
      }

      case 'browser_set_geolocation': {
        const context = browser.getPage().context();
        await context.setGeolocation({
          latitude: args.latitude,
          longitude: args.longitude,
          accuracy: args.accuracy || 0,
        });
        return {
          content: [
            {
              type: 'text',
              text: `Geolocation set to (${args.latitude}, ${args.longitude})`,
            },
          ],
        };
      }

      // ============= DEBUG =============
      case 'browser_console': {
        const messages = browser.getConsoleMessages();
        const list = messages.map((m) => `[${m.type}] ${m.text}`).join('\n');
        return {
          content: [{ type: 'text', text: `Console:\n${list || '(empty)'}` }],
        };
      }

      case 'browser_errors': {
        const errors = browser.getPageErrors();
        const list = errors.map((e) => e.message).join('\n');
        return { content: [{ type: 'text', text: `Errors:\n${list || '(none)'}` }] };
      }

      // ============= SESSION =============
      case 'browser_close': {
        await browser.close();
        browsers.delete(session);
        return {
          content: [{ type: 'text', text: `Closed session: ${session}` }],
        };
      }

      case 'browser_session_list': {
        const sessions = Array.from(browsers.keys());
        return {
          content: [
            {
              type: 'text',
              text: `Active sessions:\n${sessions.length > 0 ? sessions.join('\n') : 'No active sessions'}`,
            },
          ],
        };
      }

      default:
        throw new Error(`Unknown tool: ${name}`);
    }
  } catch (error: any) {
    return {
      content: [{ type: 'text', text: `Error: ${error.message}` }],
      isError: true,
    };
  }
}

/**
 * Main MCP Server
 */
async function main() {
  const server = new Server(
    {
      name: 'agent-browser-mcp',
      version: '2.0.0',
    },
    {
      capabilities: {
        tools: {},
      },
    }
  );

  server.setRequestHandler(ListToolsRequestSchema, async () => {
    return { tools };
  });

  server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const result = await handleToolCall(request.params.name, request.params.arguments);
    return result;
  });

  const transport = new StdioServerTransport();
  await server.connect(transport);

  console.error('Agent-browser MCP server running (ALL FEATURES)');
}

main().catch((error) => {
  console.error('Fatal error:', error);
  process.exit(1);
});
