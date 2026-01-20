/**
 * Simplified Cloudflare Worker for agent-browser
 * This worker exposes the skills/plugins API and communicates with a separate daemon
 *
 * For local development with browser automation, use: npm run dev
 * For Cloudflare deployment, ensure you have a running daemon instance
 */

import { SkillsManager, createContentPlugin } from './skills-manager.js';
import { WorkflowManager } from './workflow.js';

// Store instances per session
const skillsManagers = new Map<string, SkillsManager>();
const workflowManagers = new Map<string, WorkflowManager>();
const initializedSessions = new Set<string>();

/**
 * Get or create SkillsManager instance for a session
 */
function getSkillsManager(sessionId: string = 'default'): SkillsManager {
  if (!skillsManagers.has(sessionId)) {
    skillsManagers.set(sessionId, new SkillsManager());
  }
  return skillsManagers.get(sessionId)!;
}

/**
 * Get or create WorkflowManager instance for a session
 */
function getWorkflowManager(sessionId: string = 'default'): WorkflowManager {
  if (!workflowManagers.has(sessionId)) {
    workflowManagers.set(sessionId, new WorkflowManager());
  }
  return workflowManagers.get(sessionId)!;
}

/**
 * Initialize plugins for a session
 */
async function initializePlugins(sessionId: string): Promise<void> {
  if (initializedSessions.has(sessionId)) {
    return;
  }

  const manager = getSkillsManager(sessionId);
  try {
    // Register built-in plugins
    await manager.registerPlugin(createContentPlugin());
    console.log(`[Worker] Plugins initialized for session: ${sessionId}`);
    initializedSessions.add(sessionId);
  } catch (err) {
    console.error(`[Worker] Failed to initialize plugins:`, err);
  }
}

/**
 * Handle API requests
 */
export default {
  async fetch(request: Request): Promise<Response> {
    try {
      const url = new URL(request.url);
      const path = url.pathname;
      const sessionId =
        url.searchParams.get('session') || request.headers.get('X-Session-ID') || 'default';

      // Initialize plugins on first request
      await initializePlugins(sessionId);
      const manager = getSkillsManager(sessionId);

      // Routes
      if (request.method === 'GET' && path === '/health') {
        return new Response(
          JSON.stringify({
            status: 'ok',
            version: '0.6.0',
            session: sessionId,
          }),
          { status: 200, headers: { 'Content-Type': 'application/json' } }
        );
      }

      if (request.method === 'GET' && path === '/skills') {
        const summary = manager.getSkillsSummary();
        return new Response(JSON.stringify({ skills: summary }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      if (request.method === 'GET' && path.match(/^\/skills\/[\w-]+$/)) {
        const skillId = path.split('/')[2];
        const skill = manager.getSkill(skillId);

        if (!skill) {
          return new Response(JSON.stringify({ error: `Skill ${skillId} not found` }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          });
        }

        return new Response(
          JSON.stringify({
            id: skill.id,
            name: skill.name,
            version: skill.version,
            description: skill.description,
            enabled: skill.enabled,
          }),
          { status: 200, headers: { 'Content-Type': 'application/json' } }
        );
      }

      if (request.method === 'POST' && path.match(/^\/skills\/[\w-]+\/execute$/)) {
        try {
          const skillId = path.split('/')[2];
          const body = await request.text();
          let params: Record<string, unknown> = {};

          if (body) {
            params = JSON.parse(body);
          }

          const result = await manager.executeSkill(skillId, params);

          return new Response(JSON.stringify({ success: true, result }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          return new Response(JSON.stringify({ error: message }), {
            status: 400,
            headers: { 'Content-Type': 'application/json' },
          });
        }
      }

      if (request.method === 'GET' && path === '/plugins') {
        const summary = manager.getPluginsSummary();
        return new Response(JSON.stringify({ plugins: summary }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      if (request.method === 'POST' && path.match(/^\/plugins\/[\w-]+\/enable$/)) {
        const pluginId = path.split('/')[2];
        const success = manager.enablePlugin(pluginId);

        if (!success) {
          return new Response(JSON.stringify({ error: `Plugin ${pluginId} not found` }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          });
        }

        return new Response(
          JSON.stringify({ success: true, message: `Plugin ${pluginId} enabled` }),
          { status: 200, headers: { 'Content-Type': 'application/json' } }
        );
      }

      if (request.method === 'POST' && path.match(/^\/plugins\/[\w-]+\/disable$/)) {
        const pluginId = path.split('/')[2];
        const success = manager.disablePlugin(pluginId);

        if (!success) {
          return new Response(JSON.stringify({ error: `Plugin ${pluginId} not found` }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          });
        }

        return new Response(
          JSON.stringify({ success: true, message: `Plugin ${pluginId} disabled` }),
          { status: 200, headers: { 'Content-Type': 'application/json' } }
        );
      }

      // ============ WORKFLOW ENDPOINTS ============
      const workflowManager = getWorkflowManager(sessionId);

      // List templates (must be before GET /workflows/:id)
      if (request.method === 'GET' && path === '/workflows/templates') {
        const templates = workflowManager.getTemplates();
        return new Response(JSON.stringify({ success: true, data: templates }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Get template (must be before GET /workflows/:id)
      if (request.method === 'GET' && path.match(/^\/workflows\/templates\/[\w-]+$/)) {
        const templateId = path.split('/')[3];
        const template = workflowManager.getTemplate(templateId);

        if (!template) {
          return new Response(JSON.stringify({ success: false, error: 'Template not found' }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          });
        }

        return new Response(JSON.stringify({ success: true, data: template }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Create from template (must be before generic POST /workflows)
      if (request.method === 'POST' && path === '/workflows/from-template') {
        try {
          const body = await request.text();
          const payload = JSON.parse(body);

          if (!payload.templateId || !payload.name) {
            return new Response(
              JSON.stringify({
                success: false,
                error: 'Missing required fields: templateId, name',
              }),
              { status: 400, headers: { 'Content-Type': 'application/json' } }
            );
          }

          const workflow = workflowManager.createFromTemplate(
            payload.templateId,
            payload.name,
            payload.variables
          );

          if (!workflow) {
            return new Response(JSON.stringify({ success: false, error: 'Template not found' }), {
              status: 404,
              headers: { 'Content-Type': 'application/json' },
            });
          }

          return new Response(JSON.stringify({ success: true, data: workflow }), {
            status: 201,
            headers: { 'Content-Type': 'application/json' },
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          return new Response(JSON.stringify({ success: false, error: message }), {
            status: 400,
            headers: { 'Content-Type': 'application/json' },
          });
        }
      }

      // List workflows
      if (request.method === 'GET' && path === '/workflows') {
        const workflows = workflowManager.listWorkflows();
        return new Response(JSON.stringify({ success: true, data: workflows }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Create workflow
      if (request.method === 'POST' && path === '/workflows') {
        try {
          const body = await request.text();
          const payload = JSON.parse(body);

          if (!payload.name || !payload.description || !payload.steps) {
            return new Response(
              JSON.stringify({
                success: false,
                error: 'Missing required fields: name, description, steps',
              }),
              { status: 400, headers: { 'Content-Type': 'application/json' } }
            );
          }

          const workflow = workflowManager.createWorkflow(
            payload.name,
            payload.description,
            payload.steps,
            {
              tags: payload.tags,
              enabled: payload.enabled,
              metadata: payload.metadata,
              createdBy: payload.createdBy,
            }
          );

          return new Response(JSON.stringify({ success: true, data: workflow }), {
            status: 201,
            headers: { 'Content-Type': 'application/json' },
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          return new Response(JSON.stringify({ success: false, error: message }), {
            status: 400,
            headers: { 'Content-Type': 'application/json' },
          });
        }
      }

      // Get workflow
      if (request.method === 'GET' && path.match(/^\/workflows\/[\w-]+$/)) {
        const workflowId = path.split('/')[2];
        const workflow = workflowManager.getWorkflow(workflowId);

        if (!workflow) {
          return new Response(JSON.stringify({ success: false, error: 'Workflow not found' }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          });
        }

        return new Response(JSON.stringify({ success: true, data: workflow }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Update workflow
      if (request.method === 'PUT' && path.match(/^\/workflows\/[\w-]+$/)) {
        try {
          const workflowId = path.split('/')[2];
          const body = await request.text();
          const updates = JSON.parse(body);

          const workflow = workflowManager.updateWorkflow(workflowId, updates);

          if (!workflow) {
            return new Response(JSON.stringify({ success: false, error: 'Workflow not found' }), {
              status: 404,
              headers: { 'Content-Type': 'application/json' },
            });
          }

          return new Response(JSON.stringify({ success: true, data: workflow }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          return new Response(JSON.stringify({ success: false, error: message }), {
            status: 400,
            headers: { 'Content-Type': 'application/json' },
          });
        }
      }

      // Delete workflow
      if (request.method === 'DELETE' && path.match(/^\/workflows\/[\w-]+$/)) {
        const workflowId = path.split('/')[2];
        const deleted = workflowManager.deleteWorkflow(workflowId);

        if (!deleted) {
          return new Response(JSON.stringify({ success: false, error: 'Workflow not found' }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          });
        }

        return new Response(JSON.stringify({ success: true, message: 'Workflow deleted' }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Clone workflow
      if (request.method === 'POST' && path.match(/^\/workflows\/[\w-]+\/clone$/)) {
        try {
          const workflowId = path.split('/')[2];
          const body = await request.text();
          const payload = JSON.parse(body);

          if (!payload.newName) {
            return new Response(
              JSON.stringify({ success: false, error: 'Missing required field: newName' }),
              { status: 400, headers: { 'Content-Type': 'application/json' } }
            );
          }

          const cloned = workflowManager.cloneWorkflow(workflowId, payload.newName);

          if (!cloned) {
            return new Response(JSON.stringify({ success: false, error: 'Workflow not found' }), {
              status: 404,
              headers: { 'Content-Type': 'application/json' },
            });
          }

          return new Response(JSON.stringify({ success: true, data: cloned }), {
            status: 201,
            headers: { 'Content-Type': 'application/json' },
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          return new Response(JSON.stringify({ success: false, error: message }), {
            status: 400,
            headers: { 'Content-Type': 'application/json' },
          });
        }
      }

      // Execute workflow
      if (request.method === 'POST' && path.match(/^\/workflows\/[\w-]+\/execute$/)) {
        try {
          const workflowId = path.split('/')[2];
          const body = await request.text();
          const payload = JSON.parse(body);

          if (!payload.sessionId) {
            return new Response(
              JSON.stringify({ success: false, error: 'Missing required field: sessionId' }),
              { status: 400, headers: { 'Content-Type': 'application/json' } }
            );
          }

          const execution = workflowManager.startExecution(workflowId, payload.sessionId);

          if (!execution) {
            return new Response(
              JSON.stringify({ success: false, error: 'Workflow not found or disabled' }),
              {
                status: 404,
                headers: { 'Content-Type': 'application/json' },
              }
            );
          }

          return new Response(JSON.stringify({ success: true, data: execution }), {
            status: 202,
            headers: { 'Content-Type': 'application/json' },
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          return new Response(JSON.stringify({ success: false, error: message }), {
            status: 400,
            headers: { 'Content-Type': 'application/json' },
          });
        }
      }

      // List executions
      if (request.method === 'GET' && path.match(/^\/workflows\/[\w-]+\/executions$/)) {
        const workflowId = path.split('/')[2];
        const executions = workflowManager.listExecutions(workflowId);

        return new Response(JSON.stringify({ success: true, data: executions }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Get execution
      if (request.method === 'GET' && path.match(/^\/workflows\/[\w-]+\/executions\/[\w-]+$/)) {
        const parts = path.split('/');
        const executionId = parts[4];
        const execution = workflowManager.getExecution(executionId);

        if (!execution) {
          return new Response(JSON.stringify({ success: false, error: 'Execution not found' }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          });
        }

        return new Response(JSON.stringify({ success: true, data: execution }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Export workflow
      if (request.method === 'GET' && path.match(/^\/workflows\/[\w-]+\/export$/)) {
        const workflowId = path.split('/')[2];
        const exported = workflowManager.exportWorkflow(workflowId);

        if (!exported) {
          return new Response(JSON.stringify({ success: false, error: 'Workflow not found' }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          });
        }

        return new Response(exported, {
          status: 200,
          headers: {
            'Content-Type': 'application/json',
            'Content-Disposition': 'attachment; filename=workflow.json',
          },
        });
      }

      // Import workflow
      if (request.method === 'POST' && path === '/workflows/import') {
        try {
          const body = await request.text();
          const payload = JSON.parse(body);

          if (!payload.json) {
            return new Response(
              JSON.stringify({ success: false, error: 'Missing required field: json' }),
              { status: 400, headers: { 'Content-Type': 'application/json' } }
            );
          }

          const workflow = workflowManager.importWorkflow(payload.json);

          if (!workflow) {
            return new Response(
              JSON.stringify({ success: false, error: 'Invalid workflow JSON' }),
              {
                status: 400,
                headers: { 'Content-Type': 'application/json' },
              }
            );
          }

          return new Response(JSON.stringify({ success: true, data: workflow }), {
            status: 201,
            headers: { 'Content-Type': 'application/json' },
          });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          return new Response(JSON.stringify({ success: false, error: message }), {
            status: 400,
            headers: { 'Content-Type': 'application/json' },
          });
        }
      }

      // 404
      return new Response(JSON.stringify({ error: 'Not found', path }), {
        status: 404,
        headers: { 'Content-Type': 'application/json' },
      });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error('[Worker] Error:', message);

      return new Response(
        JSON.stringify({
          error: 'Internal server error',
          message: message,
        }),
        {
          status: 500,
          headers: { 'Content-Type': 'application/json' },
        }
      );
    }
  },
};
