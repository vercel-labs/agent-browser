import { HttpServer } from './http-server.js';
import {
  createScreenshotPlugin,
  createPdfPlugin,
  createContentPlugin,
} from './skills-manager.js';

/**
 * Cloudflare Worker entry point for agent-browser
 * Handles HTTP requests and routes them to the HTTP server adapter
 */

// Store instances per session for multiple concurrent requests
const serverInstances = new Map<string, HttpServer>();

// Track which sessions have had plugins initialized
const initializedSessions = new Set<string>();

/**
 * Initialize default plugins for a server instance
 */
async function initializeDefaultPlugins(server: HttpServer, sessionId: string): Promise<void> {
  // Only initialize once per session
  if (initializedSessions.has(sessionId)) {
    return;
  }

  const skillsManager = server.getSkillsManager();

  try {
    // Register built-in plugins
    await skillsManager.registerPlugin(createContentPlugin());

    console.log(`[Worker] Initialized default plugins for session ${sessionId}`);
    initializedSessions.add(sessionId);
  } catch (err) {
    console.error(`[Worker] Failed to initialize plugins for session ${sessionId}:`, err);
  }
}

/**
 * Get or create HTTP server instance for a session
 */
function getServerInstance(sessionId: string = 'default'): HttpServer {
  if (!serverInstances.has(sessionId)) {
    serverInstances.set(sessionId, new HttpServer(sessionId));
  }
  return serverInstances.get(sessionId)!;
}

/**
 * Main worker request handler
 */
export default {
  async fetch(request: Request, env: any, ctx: any): Promise<Response> {
    try {
      // Extract session ID from query parameter or header
      const url = new URL(request.url);
      const sessionId = url.searchParams.get('session') ||
                        request.headers.get('X-Session-ID') ||
                        'default';

      // Get or create server instance for this session
      const server = getServerInstance(sessionId);

      // Initialize plugins on first use
      await initializeDefaultPlugins(server, sessionId);

      // Handle the request
      const response = await server.handleRequest(request);

      // Handle CORS if needed
      response.headers.set('Access-Control-Allow-Origin', '*');
      response.headers.set('Access-Control-Allow-Methods', 'GET, POST, PUT, DELETE, OPTIONS');
      response.headers.set('Access-Control-Allow-Headers', 'Content-Type, X-Session-ID');

      return response;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error('Worker error:', message);

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

  async scheduled(event: any, env: any, ctx: any): Promise<void> {
    // Optional: Handle scheduled tasks for cleanup
    console.log('[Worker] Scheduled event triggered');
  },
};
