import { BrowserManager } from './browser.js';
import { parseCommand, serializeResponse, errorResponse } from './protocol.js';
import { executeCommand } from './actions.js';

/**
 * HTTP Server adapter for agent-browser daemon
 * Provides HTTP endpoints to the existing daemon functionality
 */
export class HttpServer {
  private browser: BrowserManager;
  private sessionId: string;
  private shuttingDown: boolean = false;

  constructor(sessionId: string = 'default') {
    this.browser = new BrowserManager();
    this.sessionId = sessionId;
  }

  /**
   * Handle incoming HTTP request
   */
  async handleRequest(request: Request): Promise<Response> {
    // Parse URL and get path
    const url = new URL(request.url);
    const path = url.pathname;

    // Route handling
    if (request.method === 'POST' && path === '/execute') {
      return this.handleExecute(request);
    } else if (request.method === 'GET' && path === '/status') {
      return this.handleStatus();
    } else if (request.method === 'POST' && path === '/close') {
      return this.handleClose();
    } else if (request.method === 'GET' && path === '/health') {
      return this.handleHealth();
    } else {
      return new Response(
        JSON.stringify({ error: 'Not found' }),
        { status: 404, headers: { 'Content-Type': 'application/json' } }
      );
    }
  }

  /**
   * Handle command execution request
   * POST /execute
   * Body: JSON command object
   */
  private async handleExecute(request: Request): Promise<Response> {
    try {
      const body = await request.text();
      const parseResult = parseCommand(body);

      if (!parseResult.success) {
        const resp = errorResponse(parseResult.id ?? 'unknown', parseResult.error);
        return new Response(serializeResponse(resp), {
          status: 400,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Auto-launch browser if not already launched (except for launch/close commands)
      if (
        !this.browser.isLaunched() &&
        parseResult.command.action !== 'launch' &&
        parseResult.command.action !== 'close'
      ) {
        const extensions = process.env.AGENT_BROWSER_EXTENSIONS
          ? process.env.AGENT_BROWSER_EXTENSIONS.split(',')
              .map((p) => p.trim())
              .filter(Boolean)
          : undefined;

        await this.browser.launch({
          id: 'auto',
          action: 'launch',
          headless: process.env.AGENT_BROWSER_HEADED !== '1',
          executablePath: process.env.AGENT_BROWSER_EXECUTABLE_PATH,
          extensions: extensions,
        });
      }

      // Handle close command specially
      if (parseResult.command.action === 'close') {
        const response = await executeCommand(parseResult.command, this.browser);
        this.shuttingDown = true;
        return new Response(serializeResponse(response), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        });
      }

      // Execute the command
      const response = await executeCommand(parseResult.command, this.browser);
      return new Response(serializeResponse(response), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      const errorResp = errorResponse('error', message);
      return new Response(serializeResponse(errorResp), {
        status: 500,
        headers: { 'Content-Type': 'application/json' },
      });
    }
  }

  /**
   * Handle status request
   * GET /status
   */
  private handleStatus(): Response {
    const status = {
      sessionId: this.sessionId,
      isLaunched: this.browser.isLaunched(),
      shuttingDown: this.shuttingDown,
    };

    return new Response(JSON.stringify(status), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  }

  /**
   * Handle close request
   * POST /close
   */
  private async handleClose(): Promise<Response> {
    try {
      await this.browser.close();
      this.shuttingDown = true;

      return new Response(
        JSON.stringify({ success: true, message: 'Browser closed' }),
        {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }
      );
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      return new Response(JSON.stringify({ success: false, error: message }), {
        status: 500,
        headers: { 'Content-Type': 'application/json' },
      });
    }
  }

  /**
   * Handle health check
   * GET /health
   */
  private handleHealth(): Response {
    return new Response(
      JSON.stringify({ status: 'ok', sessionId: this.sessionId }),
      {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }
    );
  }

  /**
   * Close the browser and cleanup
   */
  async cleanup(): Promise<void> {
    if (!this.shuttingDown) {
      this.shuttingDown = true;
      await this.browser.close();
    }
  }
}
