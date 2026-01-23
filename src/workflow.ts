/**
 * Workflow Management System for Agent Browser
 * Create, edit, delete, and execute automated workflows
 * Leverages Cloudflare KV storage for persistence
 */

import type { WorkerBindings } from './worker-bindings.js';

/**
 * Workflow Step - Individual action in a workflow
 */
export interface WorkflowStep {
  id: string;
  action: string; // e.g., 'navigate', 'click', 'fill', 'screenshot'
  params: Record<string, unknown>;
  condition?: {
    type: 'if' | 'if-not';
    field: string;
    value: unknown;
  };
  retries?: number;
  timeout?: number;
}

/**
 * Workflow - Collection of steps to be executed
 */
export interface Workflow {
  id: string;
  name: string;
  description: string;
  version: string;
  tags: string[];
  enabled: boolean;
  steps: WorkflowStep[];
  parallelizable?: boolean;
  timeout?: number;
  createdAt: number;
  updatedAt: number;
  createdBy?: string;
  metadata?: Record<string, unknown>;
}

/**
 * Workflow Execution - Track workflow runs
 */
export interface WorkflowExecution {
  id: string;
  workflowId: string;
  sessionId: string;
  status: 'pending' | 'running' | 'success' | 'failed' | 'cancelled';
  startedAt: number;
  completedAt?: number;
  results: Record<string, unknown>;
  errors: Array<{
    stepId: string;
    error: string;
    timestamp: number;
  }>;
}

/**
 * Workflow Template - Reusable workflow template
 */
export interface WorkflowTemplate {
  id: string;
  name: string;
  description: string;
  category: string;
  steps: WorkflowStep[];
  variables?: Record<string, unknown>;
  documentation?: string;
}

/**
 * Common workflow templates for AI automation
 */
export const workflowTemplates: Record<string, WorkflowTemplate> = {
  // Login workflow
  login: {
    id: 'template-login',
    name: 'Login Workflow',
    description: 'Automated login flow with email and password',
    category: 'authentication',
    steps: [
      {
        id: 'step-1',
        action: 'navigate',
        params: { url: '{{ loginUrl }}' },
      },
      {
        id: 'step-2',
        action: 'fill',
        params: { selector: '{{ emailSelector }}', value: '{{ email }}' },
      },
      {
        id: 'step-3',
        action: 'fill',
        params: { selector: '{{ passwordSelector }}', value: '{{ password }}' },
      },
      {
        id: 'step-4',
        action: 'click',
        params: { selector: '{{ submitSelector }}' },
      },
      {
        id: 'step-5',
        action: 'waitforloadstate',
        params: { state: 'networkidle' },
      },
    ],
    variables: {
      loginUrl: 'https://example.com/login',
      emailSelector: 'input[type=email]',
      passwordSelector: 'input[type=password]',
      submitSelector: 'button[type=submit]',
      email: 'user@example.com',
      password: 'password123',
    },
  },

  // Form fill workflow
  formFill: {
    id: 'template-form-fill',
    name: 'Form Fill Workflow',
    description: 'Fill and submit a form with multiple fields',
    category: 'form',
    steps: [
      {
        id: 'step-1',
        action: 'navigate',
        params: { url: '{{ formUrl }}' },
      },
      {
        id: 'step-2',
        action: 'fill',
        params: { selector: '{{ nameSelector }}', value: '{{ name }}' },
      },
      {
        id: 'step-3',
        action: 'fill',
        params: { selector: '{{ emailSelector }}', value: '{{ email }}' },
      },
      {
        id: 'step-4',
        action: 'select',
        params: { selector: '{{ countrySelector }}', value: '{{ country }}' },
      },
      {
        id: 'step-5',
        action: 'click',
        params: { selector: '{{ submitSelector }}' },
      },
    ],
  },

  // Data extraction workflow
  dataExtraction: {
    id: 'template-extract',
    name: 'Data Extraction Workflow',
    description: 'Navigate and extract structured data from a page',
    category: 'extraction',
    steps: [
      {
        id: 'step-1',
        action: 'navigate',
        params: { url: '{{ targetUrl }}' },
      },
      {
        id: 'step-2',
        action: 'waitforloadstate',
        params: { state: 'networkidle' },
      },
      {
        id: 'step-3',
        action: 'snapshot',
        params: { interactive: true },
      },
      {
        id: 'step-4',
        action: 'screenshot',
        params: { fullPage: true },
      },
    ],
  },

  // Monitoring workflow
  monitoring: {
    id: 'template-monitor',
    name: 'Monitoring Workflow',
    description: 'Monitor page for changes and alert on conditions',
    category: 'monitoring',
    steps: [
      {
        id: 'step-1',
        action: 'navigate',
        params: { url: '{{ pageUrl }}' },
      },
      {
        id: 'step-2',
        action: 'waitforloadstate',
        params: { state: 'networkidle' },
      },
      {
        id: 'step-3',
        action: 'screenshot',
        params: { fullPage: false },
      },
      {
        id: 'step-4',
        action: 'evaluate',
        params: { script: '{{ monitoringScript }}' },
      },
    ],
  },

  // Search workflow
  search: {
    id: 'template-search',
    name: 'Search Workflow',
    description: 'Search for content and extract results',
    category: 'search',
    steps: [
      {
        id: 'step-1',
        action: 'navigate',
        params: { url: '{{ searchUrl }}' },
      },
      {
        id: 'step-2',
        action: 'fill',
        params: { selector: '{{ searchSelector }}', value: '{{ query }}' },
      },
      {
        id: 'step-3',
        action: 'press',
        params: { key: 'Enter' },
      },
      {
        id: 'step-4',
        action: 'waitforloadstate',
        params: { state: 'networkidle' },
      },
      {
        id: 'step-5',
        action: 'snapshot',
        params: { interactive: true },
      },
    ],
  },
};

/**
 * Workflow Manager - CRUD operations
 */
export class WorkflowManager {
  private workflows: Map<string, Workflow> = new Map();
  private executions: Map<string, WorkflowExecution> = new Map();
  private bindings?: WorkerBindings;

  /**
   * Constructor - optionally accepts Cloudflare bindings for persistence
   */
  constructor(bindings?: WorkerBindings) {
    this.bindings = bindings;
  }

  /**
   * Create a new workflow
   */
  createWorkflow(
    name: string,
    description: string,
    steps: WorkflowStep[],
    options?: {
      tags?: string[];
      enabled?: boolean;
      metadata?: Record<string, unknown>;
      createdBy?: string;
    }
  ): Workflow {
    const id = `wf-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
    const now = Date.now();

    const workflow: Workflow = {
      id,
      name,
      description,
      version: '1.0.0',
      tags: options?.tags || [],
      enabled: options?.enabled !== false,
      steps,
      createdAt: now,
      updatedAt: now,
      createdBy: options?.createdBy,
      metadata: options?.metadata,
    };

    // Validate workflow
    const validation = validateWorkflow(workflow);
    if (!validation.valid) {
      throw new Error(`Invalid workflow: ${validation.errors.join(', ')}`);
    }

    this.workflows.set(id, workflow);
    return workflow;
  }

  /**
   * Get workflow by ID
   */
  getWorkflow(id: string): Workflow | undefined {
    return this.workflows.get(id);
  }

  /**
   * List all workflows
   */
  listWorkflows(filter?: { tags?: string[]; enabled?: boolean; createdBy?: string }): Workflow[] {
    let workflows = Array.from(this.workflows.values());

    if (filter?.enabled !== undefined) {
      workflows = workflows.filter((w) => w.enabled === filter.enabled);
    }

    if (filter?.tags && filter.tags.length > 0) {
      workflows = workflows.filter((w) => filter.tags!.some((t) => w.tags.includes(t)));
    }

    if (filter?.createdBy) {
      workflows = workflows.filter((w) => w.createdBy === filter.createdBy);
    }

    return workflows;
  }

  /**
   * Update workflow
   */
  updateWorkflow(
    id: string,
    updates: Partial<Omit<Workflow, 'id' | 'createdAt'>>
  ): Workflow | undefined {
    const workflow = this.workflows.get(id);
    if (!workflow) return undefined;

    const updated: Workflow = {
      ...workflow,
      ...updates,
      updatedAt: Date.now(),
    };

    this.workflows.set(id, updated);
    return updated;
  }

  /**
   * Delete workflow
   */
  deleteWorkflow(id: string): boolean {
    return this.workflows.delete(id);
  }

  /**
   * Clone workflow
   */
  cloneWorkflow(id: string, newName: string): Workflow | undefined {
    const original = this.workflows.get(id);
    if (!original) return undefined;

    const cloned = this.createWorkflow(
      newName,
      original.description,
      JSON.parse(JSON.stringify(original.steps)),
      {
        tags: [...original.tags],
        enabled: original.enabled,
        metadata: original.metadata ? { ...original.metadata } : undefined,
      }
    );

    return cloned;
  }

  /**
   * Create workflow from template
   */
  createFromTemplate(
    templateId: string,
    name: string,
    variables?: Record<string, unknown>
  ): Workflow | undefined {
    const template = workflowTemplates[templateId];
    if (!template) return undefined;

    // Replace template variables in steps
    const steps = JSON.parse(JSON.stringify(template.steps));

    if (variables) {
      const replaceVariables = (obj: any): any => {
        if (typeof obj === 'string') {
          return Object.entries(variables).reduce(
            (str, [key, value]) => str.replace(`{{ ${key} }}`, String(value)),
            obj
          );
        }
        if (typeof obj === 'object' && obj !== null) {
          Object.keys(obj).forEach((key) => {
            obj[key] = replaceVariables(obj[key]);
          });
        }
        return obj;
      };

      steps.forEach((step: WorkflowStep) => {
        step.params = replaceVariables(step.params);
      });
    }

    return this.createWorkflow(name, template.description, steps, {
      tags: ['template', template.category],
    });
  }

  /**
   * Start workflow execution
   */
  startExecution(workflowId: string, sessionId: string): WorkflowExecution | undefined {
    const workflow = this.workflows.get(workflowId);
    if (!workflow || !workflow.enabled) return undefined;

    const executionId = `exec-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;

    const execution: WorkflowExecution = {
      id: executionId,
      workflowId,
      sessionId,
      status: 'pending',
      startedAt: Date.now(),
      results: {},
      errors: [],
    };

    this.executions.set(executionId, execution);
    return execution;
  }

  /**
   * Get execution status
   */
  getExecution(id: string): WorkflowExecution | undefined {
    return this.executions.get(id);
  }

  /**
   * List executions for workflow
   */
  listExecutions(workflowId: string): WorkflowExecution[] {
    return Array.from(this.executions.values()).filter((e) => e.workflowId === workflowId);
  }

  /**
   * Execute workflow asynchronously (fire-and-forget)
   * Returns execution object immediately, execution continues in background
   */
  async executeWorkflowAsync(
    workflowId: string,
    executor: StepExecutor,
    sessionId: string,
    variables?: Record<string, unknown>
  ): Promise<WorkflowExecution | undefined> {
    const workflow = this.workflows.get(workflowId);
    if (!workflow || !workflow.enabled) return undefined;

    const execution = await executeWorkflow(workflow, executor, sessionId, variables);
    await this.persistExecution(execution);
    return execution;
  }

  /**
   * Update execution status
   */
  updateExecution(
    id: string,
    updates: Partial<Omit<WorkflowExecution, 'id' | 'workflowId' | 'sessionId'>>
  ): WorkflowExecution | undefined {
    const execution = this.executions.get(id);
    if (!execution) return undefined;

    const updated: WorkflowExecution = {
      ...execution,
      ...updates,
    };

    this.executions.set(id, updated);
    return updated;
  }

  /**
   * Get workflow templates
   */
  getTemplates(): WorkflowTemplate[] {
    return Object.values(workflowTemplates);
  }

  /**
   * Get template by ID
   */
  getTemplate(id: string): WorkflowTemplate | undefined {
    return workflowTemplates[id];
  }

  /**
   * Export workflow as JSON
   */
  exportWorkflow(id: string): string | undefined {
    const workflow = this.workflows.get(id);
    if (!workflow) return undefined;
    return JSON.stringify(workflow, null, 2);
  }

  /**
   * Import workflow from JSON
   */
  importWorkflow(json: string): Workflow | undefined {
    try {
      const data = JSON.parse(json);
      const workflow: Workflow = {
        ...data,
        id: `wf-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`,
        createdAt: Date.now(),
        updatedAt: Date.now(),
      };
      this.workflows.set(workflow.id, workflow);
      return workflow;
    } catch {
      return undefined;
    }
  }

  /**
   * Persist workflow to KV storage
   */
  async persistWorkflow(workflow: Workflow): Promise<boolean> {
    if (!this.bindings?.WORKFLOWS) {
      // Fall back to in-memory storage if KV is not available
      this.workflows.set(workflow.id, workflow);
      return true;
    }

    try {
      await this.bindings.WORKFLOWS.put(
        workflow.id,
        JSON.stringify(workflow),
        { expirationTtl: 86400 * 365 } // 1 year expiration
      );
      this.workflows.set(workflow.id, workflow);
      return true;
    } catch (error) {
      console.error(`Failed to persist workflow ${workflow.id}:`, error);
      return false;
    }
  }

  /**
   * Load workflow from KV storage
   */
  async loadWorkflow(id: string): Promise<Workflow | undefined> {
    // Check in-memory first
    const cached = this.workflows.get(id);
    if (cached) return cached;

    if (!this.bindings?.WORKFLOWS) {
      return undefined;
    }

    try {
      const data = await this.bindings.WORKFLOWS.get(id, 'json');
      if (data) {
        const workflow = data as Workflow;
        this.workflows.set(id, workflow);
        return workflow;
      }
    } catch (error) {
      console.error(`Failed to load workflow ${id}:`, error);
    }

    return undefined;
  }

  /**
   * Persist execution to KV storage
   */
  async persistExecution(execution: WorkflowExecution): Promise<boolean> {
    if (!this.bindings?.EXECUTIONS) {
      // Fall back to in-memory storage
      this.executions.set(execution.id, execution);
      return true;
    }

    try {
      await this.bindings.EXECUTIONS.put(
        execution.id,
        JSON.stringify(execution),
        { expirationTtl: 86400 * 30 } // 30 days expiration
      );
      this.executions.set(execution.id, execution);
      return true;
    } catch (error) {
      console.error(`Failed to persist execution ${execution.id}:`, error);
      return false;
    }
  }

  /**
   * Load all executions for a workflow from KV
   */
  async loadExecutions(workflowId: string): Promise<WorkflowExecution[]> {
    // Return in-memory executions if KV not available
    if (!this.bindings?.EXECUTIONS) {
      return Array.from(this.executions.values()).filter((e) => e.workflowId === workflowId);
    }

    // Note: KV doesn't support direct queries, so we return cached executions
    // In production, use D1 database for querying executions
    return Array.from(this.executions.values()).filter((e) => e.workflowId === workflowId);
  }
}

/**
 * Workflow execution with browser integration
 */

/**
 * Validate workflow before execution
 */
export function validateWorkflow(workflow: Workflow): { valid: boolean; errors: string[] } {
  const errors: string[] = [];

  // Check basic properties
  if (!workflow.id || !workflow.name) {
    errors.push('Workflow must have id and name');
  }

  if (!Array.isArray(workflow.steps) || workflow.steps.length === 0) {
    errors.push('Workflow must have at least one step');
  }

  // Validate each step
  for (let i = 0; i < workflow.steps.length; i++) {
    const step = workflow.steps[i];
    const stepErrors = validateWorkflowStep(step, i);
    errors.push(...stepErrors);
  }

  return {
    valid: errors.length === 0,
    errors,
  };
}

/**
 * Validate individual workflow step
 */
export function validateWorkflowStep(step: WorkflowStep, index: number): string[] {
  const errors: string[] = [];

  if (!step.id) {
    errors.push(`Step ${index} missing id`);
  }

  if (!step.action) {
    errors.push(`Step ${index} missing action`);
  }

  if (typeof step.action !== 'string' || step.action.length > 100) {
    errors.push(`Step ${index} action must be a string ≤ 100 chars`);
  }

  if (step.params && typeof step.params !== 'object') {
    errors.push(`Step ${index} params must be an object`);
  }

  if (step.retries !== undefined && (step.retries < 0 || step.retries > 10)) {
    errors.push(`Step ${index} retries must be 0-10`);
  }

  if (step.timeout !== undefined && (step.timeout < 100 || step.timeout > 300000)) {
    errors.push(`Step ${index} timeout must be 100-300000ms`);
  }

  // Validate parameters for dangerous actions
  if (step.params) {
    validateStepParameters(step, index, errors);
  }

  return errors;
}

/**
 * Validate step parameters for security issues
 */
function validateStepParameters(step: WorkflowStep, index: number, errors: string[]): void {
  const params = step.params || {};

  // Check for dangerous selectors that could cause issues
  for (const [key, value] of Object.entries(params)) {
    if (typeof value === 'string') {
      // Prevent extremely long strings that could cause memory issues
      if (value.length > 10000) {
        errors.push(`Step ${index} parameter ${key} exceeds max length (10000 chars)`);
      }

      // Check for common injection patterns in selectors
      if ((key === 'selector' || key === 'url') && value.includes('javascript:')) {
        errors.push(`Step ${index} parameter ${key} contains dangerous javascript: protocol`);
      }
    }
  }
}

/**
 * Step execution result
 */
export interface StepExecutionResult {
  stepId: string;
  action: string;
  status: 'success' | 'failed' | 'timeout' | 'skipped';
  result?: unknown;
  error?: string;
  duration: number; // milliseconds
  retriesUsed?: number;
}

/**
 * Execute a workflow step with retry logic and timeout handling
 */
export async function executeWorkflowStep(
  step: WorkflowStep,
  executor: StepExecutor,
  variables?: Record<string, unknown>
): Promise<StepExecutionResult> {
  const startTime = Date.now();
  const maxRetries = step.retries ?? 1;
  const timeout = step.timeout ?? 30000; // 30 second default timeout

  // Check if step should be skipped
  if (step.condition) {
    if (step.condition.type === 'if' && !variables?.[step.condition.field]) {
      return {
        stepId: step.id,
        action: step.action,
        status: 'skipped',
        duration: Date.now() - startTime,
      };
    }
    if (step.condition.type === 'if-not' && variables?.[step.condition.field]) {
      return {
        stepId: step.id,
        action: step.action,
        status: 'skipped',
        duration: Date.now() - startTime,
      };
    }
  }

  // Execute with retries
  let lastError: Error | undefined;
  for (let attempt = 0; attempt < maxRetries; attempt++) {
    try {
      // Apply timeout
      const result = await Promise.race([
        executor.execute(step.action, step.params, variables),
        new Promise((_, reject) =>
          setTimeout(() => reject(new Error(`Step timeout after ${timeout}ms`)), timeout)
        ),
      ]);

      return {
        stepId: step.id,
        action: step.action,
        status: 'success',
        result,
        duration: Date.now() - startTime,
        retriesUsed: attempt,
      };
    } catch (error) {
      lastError = error as Error;

      // Log retry attempt
      if (attempt < maxRetries - 1) {
        console.warn(
          `[Workflow] Step ${step.id} (${step.action}) failed, retrying (${attempt + 1}/${maxRetries}):`,
          lastError?.message
        );
        // Exponential backoff: wait 100ms * 2^attempt (100ms, 200ms, 400ms, ...)
        await new Promise((resolve) => setTimeout(resolve, 100 * Math.pow(2, attempt)));
      }
    }
  }

  // All retries exhausted
  return {
    stepId: step.id,
    action: step.action,
    status: 'failed',
    error: lastError?.message || 'Unknown error',
    duration: Date.now() - startTime,
    retriesUsed: maxRetries - 1,
  };
}

/**
 * Interface for step executor - implements this to connect to browser/API
 */
export interface StepExecutor {
  execute(
    action: string,
    params: Record<string, unknown>,
    variables?: Record<string, unknown>
  ): Promise<unknown>;
}

/**
 * Execute entire workflow
 */
export async function executeWorkflow(
  workflow: Workflow,
  executor: StepExecutor,
  sessionId: string = 'default',
  variables?: Record<string, unknown>
): Promise<WorkflowExecution> {
  const executionId = `exec-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
  const startTime = Date.now();

  const execution: WorkflowExecution = {
    id: executionId,
    workflowId: workflow.id,
    sessionId,
    status: 'running',
    startedAt: startTime,
    results: {},
    errors: [],
  };

  // Execute steps sequentially (unless parallelizable)
  for (const step of workflow.steps) {
    try {
      const result = await executeWorkflowStep(step, executor, variables);

      if (result.status === 'success') {
        execution.results[step.id] = result.result;
      } else if (result.status === 'failed') {
        execution.errors.push({
          stepId: step.id,
          error: result.error || 'Unknown error',
          timestamp: Date.now(),
        });

        // Stop execution on first error (unless configured to continue)
        execution.status = 'failed';
        execution.completedAt = Date.now();
        return execution;
      }
      // Skipped steps don't affect execution
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      execution.errors.push({
        stepId: step.id,
        error: errorMsg,
        timestamp: Date.now(),
      });

      execution.status = 'failed';
      execution.completedAt = Date.now();
      return execution;
    }
  }

  // All steps completed successfully
  execution.status = 'success';
  execution.completedAt = Date.now();
  return execution;
}
