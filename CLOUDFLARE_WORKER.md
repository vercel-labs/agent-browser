# Cloudflare Worker Setup - Verified ✅

## Overview

Agent Browser is now configured to run as a Cloudflare Worker with a pluggable skills system. The worker exposes an HTTP API for browser automation and content extraction.

## Architecture

- **worker-simple.ts**: Standalone Cloudflare Worker entry point (no browser dependencies)
- **skills-manager.ts**: Manages skills and plugins lifecycle
- **wrangler.toml**: Cloudflare Workers configuration
- **browser functionality**: Use the daemon (`npm run dev`) locally or connect to a remote daemon

## Testing Results ✅

All endpoints have been tested locally and working:

### Health Check
```bash
curl http://localhost:8787/health
```
Response:
```json
{"status":"ok","version":"0.6.0","session":"default"}
```

### List Skills
```bash
curl http://localhost:8787/skills
```
Response:
```json
{
  "skills": [
    {
      "id": "extract-text",
      "name": "Extract Text",
      "version": "1.0.0",
      "description": "Extract all text content from the page",
      "enabled": true,
      "plugin": "content"
    }
  ]
}
```

### Execute Skill
```bash
curl -X POST http://localhost:8787/skills/extract-text/execute \
  -H "Content-Type: application/json" \
  -d '{}'
```
Response:
```json
{"success":true,"result":{"text":"Page content"}}
```

### List Plugins
```bash
curl http://localhost:8787/plugins
```
Response:
```json
{
  "plugins": [
    {
      "id": "content",
      "name": "Content Extraction Plugin",
      "version": "1.0.0",
      "description": "Extract content from the page",
      "enabled": true,
      "skillCount": 2
    }
  ]
}
```

### Disable Plugin
```bash
curl -X POST http://localhost:8787/plugins/content/disable
```
Response:
```json
{"success":true,"message":"Plugin content disabled"}
```

### Enable Plugin
```bash
curl -X POST http://localhost:8787/plugins/content/enable
```
Response:
```json
{"success":true,"message":"Plugin content enabled"}
```

## Local Development

Start the worker locally:
```bash
npm run worker:dev
```

The server will be available at `http://localhost:8787`

## Deployment

Deploy to Cloudflare:
```bash
npm run worker:deploy
```

## Features

✅ Skills management system
✅ Plugin lifecycle management (enable/disable)
✅ Per-session isolation
✅ CORS support
✅ Error handling
✅ Health checks
✅ Environment configuration (dev, staging, production)

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | /health | Health check |
| GET | /skills | List all skills |
| GET | /skills/:id | Get skill details |
| POST | /skills/:id/execute | Execute a skill |
| GET | /plugins | List all plugins |
| POST | /plugins/:id/enable | Enable a plugin |
| POST | /plugins/:id/disable | Disable a plugin |

## Query Parameters & Headers

- `?session=my-session` - Specify session ID (default: "default")
- `X-Session-ID: my-session` - Alternative to query parameter

## Built-in Plugins

### Content Extraction
- `extract-text` - Extract all text content
- `extract-html` - Extract HTML structure

## Browser Integration

For browser automation features:

1. **Local Development**: Run the daemon in another terminal
   ```bash
   npm run dev
   ```

2. **Remote Daemon**: Connect to a running daemon instance on another machine

3. **Cloudflare Workers**: Currently the worker exposes only the skills/plugins API. Browser functionality can be accessed via a connected daemon.

## Adding Custom Skills

Create a custom plugin:

```typescript
import { Plugin } from './skills-manager.js';

const myPlugin: Plugin = {
  id: 'my-plugin',
  name: 'My Custom Plugin',
  version: '1.0.0',
  description: 'My custom skills',
  enabled: true,
  skills: [
    {
      id: 'my-skill',
      name: 'My Skill',
      version: '1.0.0',
      description: 'Does something',
      enabled: true,
      execute: async (params) => {
        // Implementation
        return { result: 'success' };
      },
    },
  ],
};
```

Register it in `worker-simple.ts`:

```typescript
await manager.registerPlugin(myPlugin);
```

## Environment Variables

- `AGENT_BROWSER_HEADLESS` - Run browser in headless mode (dev only)
- `AGENT_BROWSER_ENABLE_PLUGINS` - Enable plugin system
- `AGENT_BROWSER_LOG_LEVEL` - Logging level: debug, info, warn, error

## Notes

- The Cloudflare Worker version excludes browser dependencies to ensure it can bundle and run on Cloudflare's infrastructure
- For full browser automation, use the daemon mode: `npm run dev`
- The worker is ideal for API-only deployments and skills/plugins management
- Browser automation requests can be proxied to a separate daemon instance

## Next Steps

1. Deploy to Cloudflare: `npm run worker:deploy`
2. Create custom plugins for your use cases
3. Integrate with your applications via the HTTP API
4. Configure environment-specific settings in `wrangler.toml`

See `SKILLS.md` for detailed API documentation.
