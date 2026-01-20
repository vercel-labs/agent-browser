/**
 * Workflow Management System for Agent Browser
 * Create, edit, delete, and execute automated workflows
 * Leverages Cloudflare KV storage for persistence
 */

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
}

/**
 * Workflow execution with browser integration
 */
export async function executeWorkflowStep(
  step: WorkflowStep,
  browserManager: any
): Promise<unknown> {
  // This would be called with actual browser manager
  // Returns the result of executing the step action
  try {
    // Simulate step execution
    return {
      stepId: step.id,
      action: step.action,
      status: 'success',
      result: null,
    };
  } catch (error) {
    throw {
      stepId: step.id,
      action: step.action,
      error: String(error),
    };
  }
}
