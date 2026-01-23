/**
 * API Routes for AI Browser Automation
 * Exposes core browser commands as HTTP endpoints
 */

export interface BrowserCommandRequest {
  [key: string]: unknown;
}

export interface BrowserCommandResponse {
  success: boolean;
  data?: unknown;
  error?: string;
}

/**
 * Core browser operations for AI automation
 */
export const browserRoutes = {
  // Navigation
  'POST /browser/navigate': 'navigate',
  'POST /browser/goto': 'navigate',
  'GET /browser/back': 'back',
  'GET /browser/forward': 'forward',
  'GET /browser/reload': 'reload',
  'GET /browser/url': 'url',
  'GET /browser/title': 'title',

  // Content & DOM
  'GET /browser/content': 'content',
  'GET /browser/screenshot': 'screenshot',
  'POST /browser/evaluate': 'evaluate',
  'GET /browser/snapshot': 'snapshot',

  // Element Interaction
  'POST /browser/click': 'click',
  'POST /browser/type': 'type',
  'POST /browser/fill': 'fill',
  'POST /browser/clear': 'clear',
  'POST /browser/focus': 'focus',
  'POST /browser/hover': 'hover',
  'POST /browser/check': 'check',
  'POST /browser/uncheck': 'uncheck',
  'POST /browser/select': 'select',
  'POST /browser/dblclick': 'dblclick',
  'POST /browser/tap': 'tap',
  'POST /browser/press': 'press',

  // Element Queries
  'POST /browser/query': 'content',
  'GET /browser/element/:selector/text': 'gettext',
  'GET /browser/element/:selector/attribute': 'getattribute',
  'GET /browser/element/:selector/visible': 'isvisible',
  'GET /browser/element/:selector/enabled': 'isenabled',
  'GET /browser/element/:selector/checked': 'ischecked',
  'GET /browser/element/:selector/boundingbox': 'boundingbox',
  'GET /browser/element/:selector/count': 'count',

  // Accessibility Queries
  'POST /browser/getbyrole': 'getbyrole',
  'POST /browser/getbytext': 'getbytext',
  'POST /browser/getbylabel': 'getbylabel',
  'POST /browser/getbyplaceholder': 'getbyplaceholder',
  'POST /browser/getbyalttext': 'getbyalttext',
  'POST /browser/getbytestid': 'getbytestid',

  // Wait & Conditions
  'POST /browser/wait': 'wait',
  'POST /browser/waitfor': 'waitforfunction',
  'POST /browser/waitforloadstate': 'waitforloadstate',

  // Storage & Cookies
  'GET /browser/cookies': 'cookies_get',
  'POST /browser/cookies': 'cookies_set',
  'DELETE /browser/cookies': 'cookies_clear',
  'GET /browser/storage': 'storage_get',
  'POST /browser/storage': 'storage_set',
  'DELETE /browser/storage': 'storage_clear',

  // Page Utilities
  'POST /browser/pdf': 'pdf',
  'GET /browser/har': 'har_stop',
  'POST /browser/trace': 'trace_start',
  'GET /browser/requests': 'requests',
};

/**
 * AI-specific helper endpoints
 */
export const aiRoutes = {
  'POST /ai/understand': 'content',
  'POST /ai/find': 'getbytext',
  'POST /ai/interact': 'click',
  'POST /ai/fill': 'fill',
  'POST /ai/extract': 'snapshot',
  'POST /ai/analyze': 'evaluate',
};

/**
 * Session management endpoints
 */
export const sessionRoutes = {
  'POST /session': 'create',
  'GET /session': 'list',
  'GET /session/:id': 'get',
  'DELETE /session/:id': 'delete',
  'POST /session/:id/launch': 'launch',
  'POST /session/:id/close': 'close',
};

/**
 * Map HTTP request to protocol command
 */
export function mapRouteToCommand(method: string, path: string): string | null {
  const route = `${method} ${path}`;
  return (browserRoutes as Record<string, string>)[route] || null;
}

/**
 * Parse browser command request
 */
export function parseBrowserRequest(body: string, selector?: string): BrowserCommandRequest {
  let params: BrowserCommandRequest = {};

  if (body) {
    try {
      params = JSON.parse(body);
    } catch {
      // Empty body is ok
    }
  }

  if (selector) {
    params.selector = selector;
  }

  return params;
}

/**
 * Common browser operation helpers
 */
export const browserHelpers = {
  /**
   * Get page text content
   */
  getPageText: {
    action: 'content',
  },

  /**
   * Find element by text
   */
  findByText: (text: string) => ({
    action: 'getbytext',
    text,
    subaction: 'click',
  }),

  /**
   * Find element by label
   */
  findByLabel: (label: string) => ({
    action: 'getbylabel',
    label,
    subaction: 'click',
  }),

  /**
   * Find element by role
   */
  findByRole: (role: string, name?: string) => ({
    action: 'getbyrole',
    role,
    ...(name && { name }),
    subaction: 'click',
  }),

  /**
   * Click and wait for navigation
   */
  clickAndWait: {
    action: 'click',
  },

  /**
   * Fill form field
   */
  fillField: (selector: string, value: string) => ({
    action: 'fill',
    selector,
    value,
  }),

  /**
   * Get accessibility tree
   */
  getA11yTree: {
    action: 'snapshot',
    interactive: true,
  },

  /**
   * Evaluate JavaScript
   */
  evaluate: (script: string, args?: unknown[]) => ({
    action: 'evaluate',
    script,
    args,
  }),

  /**
   * Take screenshot
   */
  screenshot: (fullPage = false) => ({
    action: 'screenshot',
    fullPage,
    format: 'png',
  }),

  /**
   * Get page DOM snapshot
   */
  snapshot: {
    action: 'snapshot',
    interactive: true,
    maxDepth: 10,
  },
};
