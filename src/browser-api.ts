/**
 * Browser HTTP API Handler
 * Provides HTTP endpoints for browser automation commands
 */

import { parseCommand, serializeResponse, errorResponse } from './protocol.js';
import type { Command } from './types.js';

/**
 * Convert HTTP request to browser command
 */
export function httpRequestToCommand(
  method: string,
  path: string,
  body: string,
  queryParams: Record<string, string>
): Command | null {
  // Extract selector from path if present (e.g., /browser/element/:selector/text)
  const selectorMatch = path.match(/\/browser\/element\/([^/]+)/);
  const selector = selectorMatch ? decodeURIComponent(selectorMatch[1]) : undefined;

  // Simple command map for common operations
  const commandMap: Record<string, string> = {
    'POST /browser/navigate': 'navigate',
    'POST /browser/goto': 'navigate',
    'GET /browser/back': 'back',
    'GET /browser/forward': 'forward',
    'GET /browser/reload': 'reload',
    'GET /browser/url': 'url',
    'GET /browser/title': 'title',
    'GET /browser/content': 'content',
    'GET /browser/screenshot': 'screenshot',
    'POST /browser/evaluate': 'evaluate',
    'GET /browser/snapshot': 'snapshot',
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
    'POST /browser/wait': 'wait',
    'GET /browser/cookies': 'cookies_get',
    'POST /browser/cookies': 'cookies_set',
    'DELETE /browser/cookies': 'cookies_clear',
  };

  const route = `${method} ${path}`;
  const action = commandMap[route];

  if (!action) {
    return null;
  }

  // Parse request body
  let params: Record<string, unknown> = {};
  if (body) {
    try {
      params = JSON.parse(body);
    } catch {
      // Invalid JSON, continue with empty params
    }
  }

  // Add selector if present in path
  if (selector && !params.selector) {
    params.selector = selector;
  }

  // Build command
  const command: Command = {
    id: queryParams['id'] || `cmd-${Date.now()}`,
    action: action as any,
    ...params,
  };

  return command;
}

/**
 * Create response from command result
 */
export function createResponse(id: string, success: boolean, data?: unknown, error?: string) {
  if (success) {
    return serializeResponse({ id, success: true, data });
  } else {
    return serializeResponse(errorResponse(id, error || 'Unknown error'));
  }
}

/**
 * Get AI-friendly response format
 */
export function getAIResponse(data: unknown): unknown {
  // Format response for AI consumption
  if (typeof data === 'string') {
    return { text: data };
  }
  if (typeof data === 'object' && data !== null) {
    return data;
  }
  return { result: data };
}

/**
 * Parse query string
 */
export function parseQueryString(url: string): Record<string, string> {
  const params: Record<string, string> = {};
  const urlObj = new URL(url);
  urlObj.searchParams.forEach((value, key) => {
    params[key] = value;
  });
  return params;
}

/**
 * Extract path from full URL
 */
export function extractPath(url: string): string {
  const urlObj = new URL(url);
  return urlObj.pathname;
}

/**
 * Extract query parameters from URL
 */
export function extractQueryParams(url: string): Record<string, string> {
  const urlObj = new URL(url);
  const params: Record<string, string> = {};
  urlObj.searchParams.forEach((value, key) => {
    params[key] = value;
  });
  return params;
}

/**
 * Format command for logging
 */
export function formatCommand(command: Command): string {
  const { id, action, ...params } = command;
  const paramStr = Object.entries(params)
    .map(([k, v]) => {
      if (typeof v === 'string' && v.length > 50) {
        return `${k}="${v.substring(0, 47)}..."`;
      }
      return `${k}=${JSON.stringify(v)}`;
    })
    .join(' ');
  return `[${id}] ${action}${paramStr ? ' ' + paramStr : ''}`;
}
