/**
 * Cloudflare Worker Bindings Integration
 * Use KV storage, Durable Objects, and R2 for workflow persistence
 */

import type {
  KVNamespace,
  R2Bucket,
  R2Object,
  R2ObjectBody,
  D1Database,
  DurableObjectNamespace,
} from '@cloudflare/workers-types';

/**
 * Bindings types for Cloudflare Workers
 */
export interface WorkerBindings {
  // KV Namespaces for data storage
  WORKFLOWS?: KVNamespace; // Store workflows
  EXECUTIONS?: KVNamespace; // Store execution history
  CACHE?: KVNamespace; // Cache screenshots and results
  SESSIONS?: KVNamespace; // Session data

  // Durable Objects for state management
  WorkflowQueue?: DurableObjectNamespace; // Queue for workflow executions

  // R2 Bucket for file storage
  STORAGE?: R2Bucket; // Store screenshots, PDFs, etc.

  // D1 Database for structured data
  DB?: D1Database; // Structured workflow data
}

/**
 * KV Storage Helper for Workflows
 */
export class WorkflowKVStorage {
  constructor(private kv: KVNamespace) {}

  /**
   * Save workflow to KV
   */
  async saveWorkflow(id: string, workflow: any): Promise<void> {
    await this.kv.put(`workflow:${id}`, JSON.stringify(workflow), {
      metadata: {
        type: 'workflow',
        createdAt: new Date().toISOString(),
      },
    });
  }

  /**
   * Get workflow from KV
   */
  async getWorkflow(id: string): Promise<any | null> {
    const data = await this.kv.get(`workflow:${id}`);
    return data ? JSON.parse(data) : null;
  }

  /**
   * List all workflows
   */
  async listWorkflows(): Promise<any[]> {
    const list = await this.kv.list({ prefix: 'workflow:' });
    const workflows: any[] = [];

    for (const key of list.keys) {
      const data = await this.kv.get(key.name);
      if (data) {
        workflows.push(JSON.parse(data));
      }
    }

    return workflows;
  }

  /**
   * Delete workflow from KV
   */
  async deleteWorkflow(id: string): Promise<void> {
    await this.kv.delete(`workflow:${id}`);
  }

  /**
   * Save execution history
   */
  async saveExecution(workflowId: string, executionId: string, execution: any): Promise<void> {
    await this.kv.put(`execution:${workflowId}:${executionId}`, JSON.stringify(execution), {
      expirationTtl: 7 * 24 * 60 * 60, // 7 days
      metadata: {
        type: 'execution',
        workflowId,
        createdAt: new Date().toISOString(),
      },
    });
  }

  /**
   * Get execution history
   */
  async getExecution(workflowId: string, executionId: string): Promise<any | null> {
    const data = await this.kv.get(`execution:${workflowId}:${executionId}`);
    return data ? JSON.parse(data) : null;
  }

  /**
   * List executions for workflow
   */
  async listExecutions(workflowId: string): Promise<any[]> {
    const list = await this.kv.list({ prefix: `execution:${workflowId}:` });
    const executions: any[] = [];

    for (const key of list.keys) {
      const data = await this.kv.get(key.name);
      if (data) {
        executions.push(JSON.parse(data));
      }
    }

    return executions;
  }

  /**
   * Cache screenshot
   */
  async cacheScreenshot(executionId: string, filename: string, data: string): Promise<void> {
    await this.kv.put(`screenshot:${executionId}:${filename}`, data, {
      expirationTtl: 24 * 60 * 60, // 24 hours
    });
  }

  /**
   * Get cached screenshot
   */
  async getScreenshot(executionId: string, filename: string): Promise<string | null> {
    return await this.kv.get(`screenshot:${executionId}:${filename}`);
  }

  /**
   * Store session data
   */
  async saveSession(sessionId: string, data: any): Promise<void> {
    await this.kv.put(`session:${sessionId}`, JSON.stringify(data), {
      expirationTtl: 30 * 60, // 30 minutes
    });
  }

  /**
   * Get session data
   */
  async getSession(sessionId: string): Promise<any | null> {
    const data = await this.kv.get(`session:${sessionId}`);
    return data ? JSON.parse(data) : null;
  }
}

/**
 * R2 Storage Helper for Files
 */
export class WorkflowR2Storage {
  constructor(private r2: R2Bucket) {}

  /**
   * Upload file to R2
   */
  async uploadFile(
    path: string,
    data: ArrayBuffer | ReadableStream<any> | string,
    contentType: string = 'application/octet-stream'
  ): Promise<R2Object | null> {
    return await this.r2.put(path, data as string | ArrayBuffer, {
      httpMetadata: {
        contentType,
      },
      customMetadata: {
        uploadedAt: new Date().toISOString(),
      },
    });
  }

  /**
   * Download file from R2
   */
  async downloadFile(path: string): Promise<R2ObjectBody | null> {
    return await this.r2.get(path);
  }

  /**
   * Delete file from R2
   */
  async deleteFile(path: string): Promise<void> {
    await this.r2.delete(path);
  }

  /**
   * List files in path
   */
  async listFiles(prefix: string): Promise<R2Object[]> {
    const result = await this.r2.list({ prefix });
    return result.objects;
  }

  /**
   * Save workflow screenshot
   */
  async saveScreenshot(
    workflowId: string,
    executionId: string,
    filename: string,
    imageData: ArrayBuffer
  ): Promise<string> {
    const path = `workflows/${workflowId}/${executionId}/${filename}`;
    await this.uploadFile(path, imageData, 'image/png');
    return path;
  }

  /**
   * Save workflow export
   */
  async saveWorkflowExport(workflowId: string, jsonData: string): Promise<string> {
    const path = `exports/workflows/${workflowId}-${Date.now()}.json`;
    await this.uploadFile(path, jsonData, 'application/json');
    return path;
  }

  /**
   * Save execution report
   */
  async saveExecutionReport(
    workflowId: string,
    executionId: string,
    htmlReport: string
  ): Promise<string> {
    const path = `reports/${workflowId}/${executionId}.html`;
    await this.uploadFile(path, htmlReport, 'text/html');
    return path;
  }
}

/**
 * Helper to use bindings in handler
 */
export function getKVStorage(bindings: WorkerBindings): WorkflowKVStorage | null {
  if (!bindings.WORKFLOWS) return null;
  return new WorkflowKVStorage(bindings.WORKFLOWS);
}

export function getR2Storage(bindings: WorkerBindings): WorkflowR2Storage | null {
  if (!bindings.STORAGE) return null;
  return new WorkflowR2Storage(bindings.STORAGE);
}

/**
 * Example wrangler.toml configuration for bindings:
 *
 * # KV Namespaces
 * [[kv_namespaces]]
 * binding = "WORKFLOWS"
 * id = "your-kv-namespace-id"
 *
 * [[kv_namespaces]]
 * binding = "EXECUTIONS"
 * id = "your-kv-namespace-id"
 *
 * # R2 Bucket
 * [[r2_buckets]]
 * binding = "STORAGE"
 * bucket_name = "agent-browser-storage"
 *
 * # D1 Database
 * [[d1_databases]]
 * binding = "DB"
 * database_name = "agent-browser"
 * database_id = "your-database-id"
 */
