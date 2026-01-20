/**
 * Full-featured Cloudflare Worker for Agent Browser
 * Includes browser automation API endpoints for AI agents
 *
 * Endpoints:
 * - Browser control: /browser/navigate, /browser/click, etc.
 * - Skills: /skills, /skills/:id/execute
 * - Plugins: /plugins, /plugins/:id/enable/disable
 * - Sessions: /session (for future use)
 */

import { SkillsManager, createContentPlugin } from './skills-manager.js';
import {
  httpRequestToCommand,
  extractPath,
  extractQueryParams,
  createResponse,
  getAIResponse,
  formatCommand,
} from './browser-api.js';
import { parseCommand, serializeResponse, errorResponse } from './protocol.js';

// Store instances per session
const skillsManagers = new Map<string, SkillsManager>();
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
      const path = extractPath(request.url);
      const queryParams = extractQueryParams(request.url);
      const sessionId = queryParams['session'] || request.headers.get('X-Session-ID') || 'default';

      // Initialize plugins on first request
      await initializePlugins(sessionId);

      // Health check (no authentication needed)
      if (request.method === 'GET' && path === '/health') {
        return new Response(
          JSON.stringify({
            status: 'ok',
            version: '0.6.0',
            session: sessionId,
            endpoints: ['browser', 'skills', 'plugins'],
          }),
          { status: 200, headers: { 'Content-Type': 'application/json' } }
        );
      }

      // ============ SKILLS ENDPOINTS ============
      if (request.method === 'GET' && path === '/skills') {
        const manager = getSkillsManager(sessionId);
        const summary = manager.getSkillsSummary();
        return new Response(JSON.stringify({ skills: summary }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      if (request.method === 'GET' && path.match(/^\/skills\/[\w-]+$/)) {
        const manager = getSkillsManager(sessionId);
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
          const manager = getSkillsManager(sessionId);
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

      // ============ PLUGINS ENDPOINTS ============
      if (request.method === 'GET' && path === '/plugins') {
        const manager = getSkillsManager(sessionId);
        const summary = manager.getPluginsSummary();
        return new Response(JSON.stringify({ plugins: summary }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      if (request.method === 'POST' && path.match(/^\/plugins\/[\w-]+\/enable$/)) {
        const manager = getSkillsManager(sessionId);
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
        const manager = getSkillsManager(sessionId);
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

      // ============ BROWSER ENDPOINTS (Command Routing) ============
      // Route browser commands to the daemon via protocol
      if (path.startsWith('/browser/') || path.startsWith('/ai/')) {
        const body = await request.text();
        const command = httpRequestToCommand(request.method, path, body, queryParams);

        if (!command) {
          return new Response(
            JSON.stringify({
              error: 'Unsupported endpoint',
              path,
              method: request.method,
            }),
            { status: 404, headers: { 'Content-Type': 'application/json' } }
          );
        }

        // For now, return a placeholder response
        // In production, this would connect to a daemon or execute the command
        console.log(`[Worker] Command: ${formatCommand(command)}`);

        return new Response(
          JSON.stringify({
            success: true,
            command: command.action,
            message: 'Command queued for execution',
            note: 'Connect to daemon for actual execution',
          }),
          { status: 202, headers: { 'Content-Type': 'application/json' } }
        );
      }

      // ============ DEFAULT 404 ============
      return new Response(
        JSON.stringify({
          error: 'Not found',
          path,
          availableEndpoints: {
            health: 'GET /health',
            skills: 'GET /skills, GET /skills/:id, POST /skills/:id/execute',
            plugins: 'GET /plugins, POST /plugins/:id/enable, POST /plugins/:id/disable',
            browser: 'POST /browser/navigate, POST /browser/click, GET /browser/screenshot, etc.',
          },
        }),
        { status: 404, headers: { 'Content-Type': 'application/json' } }
      );
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
