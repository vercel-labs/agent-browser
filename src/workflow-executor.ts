/**
 * Workflow Step Executor for Cloudflare Worker
 * Executes workflow steps by calling browser API endpoints
 */

import { StepExecutor } from './workflow.js';

/**
 * Maps workflow step actions to browser API endpoints
 */
const actionMapping: Record<string, string> = {
  // Navigation
  navigate: 'POST /browser/navigate',
  goto: 'POST /browser/navigate',
  'go-back': 'POST /browser/back',
  'go-forward': 'POST /browser/forward',
  reload: 'POST /browser/reload',

  // Interaction
  click: 'POST /browser/click',
  dblclick: 'POST /browser/dblclick',
  fill: 'POST /browser/fill',
  type: 'POST /browser/type',
  check: 'POST /browser/check',
  uncheck: 'POST /browser/uncheck',
  select: 'POST /browser/selectOption',
  upload: 'POST /browser/upload',
  drag: 'POST /browser/drag',
  focus: 'POST /browser/focus',
  blur: 'POST /browser/blur',
  hover: 'POST /browser/hover',

  // Waiting
  'wait-for-selector': 'POST /browser/waitForSelector',
  'wait-for-url': 'POST /browser/waitForURL',
  'wait-for-load': 'POST /browser/waitForLoadState',
  'wait-for-function': 'POST /browser/waitForFunction',
  'wait-ms': 'POST /browser/wait',

  // Queries
  'get-text': 'GET /browser/getText',
  'get-value': 'GET /browser/getValue',
  'is-visible': 'GET /browser/isVisible',
  'is-enabled': 'GET /browser/isEnabled',
  'is-checked': 'GET /browser/isChecked',
  'get-attribute': 'GET /browser/getAttribute',
  'query-all': 'GET /browser/queryAll',
  'query-selector': 'GET /browser/querySelector',

  // Screenshots
  screenshot: 'POST /browser/screenshot',
  pdf: 'POST /browser/pdf',

  // Evaluation
  eval: 'POST /browser/evaluate',
  'eval-all': 'POST /browser/evaluateAll',

  // Content
  'get-content': 'GET /browser/getPageContent',
  'get-html': 'GET /browser/getPageHTML',

  // Accessibility
  'get-role': 'GET /browser/getElementByRole',
  'get-label': 'GET /browser/getElementByLabel',
  'get-placeholder': 'GET /browser/getElementByPlaceholder',
};

/**
 * Maps workflow action parameters to browser API parameter names
 */
const parameterMapping: Record<string, Record<string, string>> = {
  navigate: { url: 'url', waitUntil: 'waitUntil', headers: 'headers' },
  click: { selector: 'selector', button: 'button', clickCount: 'clickCount', delay: 'delay' },
  fill: { selector: 'selector', value: 'value' },
  type: { selector: 'selector', text: 'text', delay: 'delay', clear: 'clear' },
  select: { selector: 'selector', value: 'value' },
  'wait-ms': { ms: 'ms' },
  'wait-for-selector': { selector: 'selector', timeout: 'timeout' },
  'wait-for-url': { url: 'url', timeout: 'timeout' },
};

/**
 * Worker-based step executor
 * Executes workflow steps by calling the worker's browser API endpoints
 */
export class WorkerStepExecutor implements StepExecutor {
  private sessionId: string;
  private baseUrl: string;

  constructor(sessionId: string = 'default', baseUrl: string = '/browser') {
    this.sessionId = sessionId;
    this.baseUrl = baseUrl;
  }

  /**
   * Execute a workflow step action
   */
  async execute(
    action: string,
    params: Record<string, unknown>,
    variables?: Record<string, unknown>
  ): Promise<unknown> {
    // Resolve variables in parameters
    const resolvedParams = this.resolveVariables(params, variables);

    // Get the API endpoint for this action
    const endpoint = actionMapping[action];
    if (!endpoint) {
      throw new Error(`Unknown workflow action: ${action}`);
    }

    // Parse endpoint
    const [method, path] = endpoint.split(' ');

    // Map workflow parameters to API parameters
    const apiParams = this.mapParameters(action, resolvedParams);

    // Build full URL
    const fullPath = `${this.baseUrl}${path}?session=${this.sessionId}`;

    // Make API call
    try {
      const response = await fetch(fullPath, {
        method,
        headers: {
          'Content-Type': 'application/json',
          'X-Session-ID': this.sessionId,
        },
        body: method === 'GET' ? undefined : JSON.stringify(apiParams),
      });

      if (!response.ok) {
        const errorData = await response.json().catch(() => ({}));
        throw new Error(
          `API call failed: ${response.status} ${(errorData as any).error || response.statusText}`
        );
      }

      const data = await response.json();
      return (data as any).data || (data as any).result || data;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      throw new Error(`Failed to execute ${action}: ${message}`);
    }
  }

  /**
   * Resolve variables in parameters
   * Replaces {{ varName }} with values from variables map
   */
  private resolveVariables(
    params: Record<string, unknown>,
    variables?: Record<string, unknown>
  ): Record<string, unknown> {
    if (!variables) return params;

    const resolved: Record<string, unknown> = {};

    for (const [key, value] of Object.entries(params)) {
      if (typeof value === 'string' && value.match(/^\{\{.*\}\}$/)) {
        const varName = value.slice(2, -2).trim();
        resolved[key] = variables[varName] ?? value;
      } else if (typeof value === 'object' && value !== null) {
        resolved[key] = this.resolveVariables(value as Record<string, unknown>, variables);
      } else {
        resolved[key] = value;
      }
    }

    return resolved;
  }

  /**
   * Map workflow action parameters to browser API parameters
   */
  private mapParameters(action: string, params: Record<string, unknown>): Record<string, unknown> {
    const mapping = parameterMapping[action];
    if (!mapping) return params;

    const mapped: Record<string, unknown> = {};

    for (const [workflowParam, apiParam] of Object.entries(mapping)) {
      if (workflowParam in params) {
        mapped[apiParam] = params[workflowParam];
      }
    }

    // Include any unmapped parameters
    for (const [key, value] of Object.entries(params)) {
      if (!mapping[key]) {
        mapped[key] = value;
      }
    }

    return mapped;
  }
}
