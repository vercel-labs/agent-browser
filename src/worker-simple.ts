/**
 * Simplified Cloudflare Worker for agent-browser
 * This worker exposes the skills/plugins API and communicates with a separate daemon
 *
 * For local development with browser automation, use: npm run dev
 * For Cloudflare deployment, ensure you have a running daemon instance
 */

import { SkillsManager, createContentPlugin } from './skills-manager.js';

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
