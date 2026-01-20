/**
 * Workflow HTTP Routes
 * CRUD operations for workflow management
 */

import { Workflow, WorkflowStep, WorkflowManager, workflowTemplates } from './workflow.js';

/**
 * Parse workflow request
 */
export function parseWorkflowRequest(body: string): any {
  try {
    return JSON.parse(body);
  } catch {
    return null;
  }
}

/**
 * Workflow route definitions
 */
export const workflowRoutes = {
  // Workflow CRUD
  'GET /workflows': 'list_workflows',
  'POST /workflows': 'create_workflow',
  'GET /workflows/:id': 'get_workflow',
  'PUT /workflows/:id': 'update_workflow',
  'DELETE /workflows/:id': 'delete_workflow',
  'POST /workflows/:id/clone': 'clone_workflow',

  // Workflow Execution
  'POST /workflows/:id/execute': 'execute_workflow',
  'GET /workflows/:id/executions': 'list_executions',
  'GET /workflows/:id/executions/:executionId': 'get_execution',
  'DELETE /workflows/:id/executions/:executionId': 'cancel_execution',

  // Workflow Templates
  'GET /workflows/templates': 'list_templates',
  'GET /workflows/templates/:templateId': 'get_template',
  'POST /workflows/from-template': 'create_from_template',

  // Workflow Import/Export
  'GET /workflows/:id/export': 'export_workflow',
  'POST /workflows/import': 'import_workflow',

  // Workflow Status & Analytics
  'GET /workflows/:id/status': 'get_workflow_status',
  'GET /workflows/stats': 'get_workflow_stats',
};

/**
 * Workflow response types
 */
export interface WorkflowResponse {
  success: boolean;
  data?: any;
  error?: string;
  code?: string;
}

/**
 * Create workflow request body
 */
export interface CreateWorkflowRequest {
  name: string;
  description: string;
  steps: WorkflowStep[];
  tags?: string[];
  enabled?: boolean;
  metadata?: Record<string, unknown>;
}

/**
 * Update workflow request body
 */
export interface UpdateWorkflowRequest {
  name?: string;
  description?: string;
  steps?: WorkflowStep[];
  tags?: string[];
  enabled?: boolean;
  metadata?: Record<string, unknown>;
}

/**
 * Execute workflow request body
 */
export interface ExecuteWorkflowRequest {
  sessionId: string;
  variables?: Record<string, unknown>;
  parallel?: boolean;
}

/**
 * Helper to create workflow response
 */
export function createWorkflowResponse(
  success: boolean,
  data?: any,
  error?: string
): WorkflowResponse {
  return {
    success,
    data: success ? data : undefined,
    error: success ? undefined : error,
  };
}

/**
 * Validate workflow steps
 */
export function validateWorkflowSteps(steps: WorkflowStep[]): { valid: boolean; error?: string } {
  if (!Array.isArray(steps) || steps.length === 0) {
    return { valid: false, error: 'Workflow must have at least one step' };
  }

  for (let i = 0; i < steps.length; i++) {
    const step = steps[i];

    if (!step.id) {
      return { valid: false, error: `Step ${i} missing id` };
    }

    if (!step.action) {
      return { valid: false, error: `Step ${step.id} missing action` };
    }

    if (!step.params || typeof step.params !== 'object') {
      return { valid: false, error: `Step ${step.id} missing or invalid params` };
    }
  }

  return { valid: true };
}

/**
 * Common workflow patterns
 */
export const workflowPatterns = {
  /**
   * Create login workflow
   */
  login: (params: {
    loginUrl: string;
    emailSelector: string;
    passwordSelector: string;
    submitSelector: string;
  }): Workflow => ({
    id: `wf-login-${Date.now()}`,
    name: 'Login Workflow',
    description: 'Automated login',
    version: '1.0.0',
    tags: ['authentication', 'login'],
    enabled: true,
    steps: [
      {
        id: 'navigate',
        action: 'navigate',
        params: { url: params.loginUrl },
      },
      {
        id: 'fill-email',
        action: 'fill',
        params: { selector: params.emailSelector, value: '{{ email }}' },
      },
      {
        id: 'fill-password',
        action: 'fill',
        params: { selector: params.passwordSelector, value: '{{ password }}' },
      },
      {
        id: 'submit',
        action: 'click',
        params: { selector: params.submitSelector },
      },
      {
        id: 'wait',
        action: 'waitforloadstate',
        params: { state: 'networkidle' },
      },
    ],
    createdAt: Date.now(),
    updatedAt: Date.now(),
  }),

  /**
   * Create data extraction workflow
   */
  extract: (params: { targetUrl: string; selectors: Record<string, string> }): Workflow => ({
    id: `wf-extract-${Date.now()}`,
    name: 'Data Extraction Workflow',
    description: 'Extract structured data',
    version: '1.0.0',
    tags: ['extraction', 'data'],
    enabled: true,
    steps: [
      {
        id: 'navigate',
        action: 'navigate',
        params: { url: params.targetUrl },
      },
      {
        id: 'wait',
        action: 'waitforloadstate',
        params: { state: 'networkidle' },
      },
      {
        id: 'snapshot',
        action: 'snapshot',
        params: { interactive: true },
      },
      ...Object.entries(params.selectors).map(([key, selector]) => ({
        id: `extract-${key}`,
        action: 'gettext',
        params: { selector },
      })),
    ],
    createdAt: Date.now(),
    updatedAt: Date.now(),
  }),

  /**
   * Create monitoring workflow
   */
  monitor: (params: { pageUrl: string; checkScript: string; interval: number }): Workflow => ({
    id: `wf-monitor-${Date.now()}`,
    name: 'Monitoring Workflow',
    description: 'Monitor page for changes',
    version: '1.0.0',
    tags: ['monitoring', 'check'],
    enabled: true,
    steps: [
      {
        id: 'navigate',
        action: 'navigate',
        params: { url: params.pageUrl },
      },
      {
        id: 'screenshot-before',
        action: 'screenshot',
        params: { fullPage: true },
      },
      {
        id: 'evaluate',
        action: 'evaluate',
        params: { script: params.checkScript },
      },
    ],
    createdAt: Date.now(),
    updatedAt: Date.now(),
  }),
};

/**
 * Workflow execution steps mapping to browser actions
 */
export const stepActions = {
  navigate: 'POST /browser/navigate',
  click: 'POST /browser/click',
  type: 'POST /browser/type',
  fill: 'POST /browser/fill',
  clear: 'POST /browser/clear',
  screenshot: 'GET /browser/screenshot',
  snapshot: 'GET /browser/snapshot',
  evaluate: 'POST /browser/evaluate',
  gettext: 'GET /browser/element/:selector/text',
  getbytext: 'POST /browser/getbytext',
  getbyrole: 'POST /browser/getbyrole',
  wait: 'POST /browser/wait',
  waitforloadstate: 'POST /browser/waitforloadstate',
  select: 'POST /browser/select',
  check: 'POST /browser/check',
  uncheck: 'POST /browser/uncheck',
  hover: 'POST /browser/hover',
  press: 'POST /browser/press',
  back: 'GET /browser/back',
  forward: 'GET /browser/forward',
  reload: 'GET /browser/reload',
  cookies_get: 'GET /browser/cookies',
  cookies_set: 'POST /browser/cookies',
  cookies_clear: 'DELETE /browser/cookies',
};

/**
 * Workflow scheduling patterns
 */
export interface WorkflowSchedule {
  type: 'once' | 'interval' | 'cron';
  interval?: number; // milliseconds for interval
  cron?: string; // cron expression
  timezone?: string;
}

/**
 * Workflow trigger patterns
 */
export interface WorkflowTrigger {
  type: 'manual' | 'webhook' | 'schedule' | 'event';
  schedule?: WorkflowSchedule;
  webhookUrl?: string;
  event?: string;
}
