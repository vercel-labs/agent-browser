# Skills and Plugins System

Agent Browser Worker supports a pluggable skills system that allows you to extend functionality through plugins.

## Architecture

- **Skills**: Individual capabilities that can be executed (e.g., "take-screenshot", "extract-text")
- **Plugins**: Collections of related skills bundled together (e.g., "screenshot", "pdf", "content")
- **SkillsManager**: Manages the lifecycle of skills and plugins

## Available Endpoints

### List All Skills

```bash
GET /skills?session=my-session
```

Response:
```json
{
  "skills": [
    {
      "id": "take-screenshot",
      "name": "Take Screenshot",
      "version": "1.0.0",
      "description": "Capture a screenshot of the current page",
      "enabled": true,
      "plugin": "screenshot"
    }
  ]
}
```

### Get Specific Skill

```bash
GET /skills/take-screenshot?session=my-session
```

Response:
```json
{
  "id": "take-screenshot",
  "name": "Take Screenshot",
  "version": "1.0.0",
  "description": "Capture a screenshot of the current page",
  "enabled": true
}
```

### Execute a Skill

```bash
POST /skills/take-screenshot/execute?session=my-session
Content-Type: application/json

{
  "path": "screenshot.png",
  "fullPage": true
}
```

Response:
```json
{
  "success": true,
  "result": {
    "path": "screenshot.png",
    "size": 102400
  }
}
```

### List All Plugins

```bash
GET /plugins?session=my-session
```

Response:
```json
{
  "plugins": [
    {
      "id": "screenshot",
      "name": "Screenshot Plugin",
      "version": "1.0.0",
      "description": "Take screenshots of the browser viewport",
      "enabled": true,
      "skillCount": 1
    }
  ]
}
```

### Enable Plugin

```bash
POST /plugins/screenshot/enable?session=my-session
```

Response:
```json
{
  "success": true,
  "message": "Plugin screenshot enabled"
}
```

### Disable Plugin

```bash
POST /plugins/screenshot/disable?session=my-session
```

Response:
```json
{
  "success": true,
  "message": "Plugin screenshot disabled"
}
```

## Built-in Plugins

### Content Plugin
Extract page content:
- `extract-text`: Extract all text content
- `extract-html`: Extract HTML structure

### Screenshot Plugin (for future use)
Capture page screenshots:
- `take-screenshot`: Capture current viewport or full page

### PDF Plugin (for future use)
Export pages as PDF:
- `export-pdf`: Convert page to PDF

## Creating Custom Plugins

```typescript
import { Plugin } from './skills-manager.js';

const customPlugin: Plugin = {
  id: 'my-plugin',
  name: 'My Custom Plugin',
  version: '1.0.0',
  description: 'Does something cool',
  enabled: true,
  skills: [
    {
      id: 'my-skill',
      name: 'My Skill',
      version: '1.0.0',
      description: 'Performs a task',
      enabled: true,
      execute: async (params) => {
        // Implement your logic here
        return { result: 'success' };
      },
    },
  ],
  initialize: async () => {
    // Optional: Setup code
    console.log('Plugin initialized');
  },
  destroy: async () => {
    // Optional: Cleanup code
    console.log('Plugin destroyed');
  },
};
```

Register the plugin:

```typescript
const skillsManager = server.getSkillsManager();
await skillsManager.registerPlugin(customPlugin);
```

## Session Management

Skills and plugins are managed per session. Use the `session` query parameter or `X-Session-ID` header to specify which session to use:

```bash
# Using query parameter
curl http://localhost:8787/skills?session=user-123

# Using header
curl -H "X-Session-ID: user-123" http://localhost:8787/skills
```

Each session maintains its own browser instance and plugin state.

## Environment Variables

- `AGENT_BROWSER_ENABLE_PLUGINS` - Enable/disable plugin system (default: true)
- `AGENT_BROWSER_LOG_LEVEL` - Logging level: debug, info, warn, error
- `AGENT_BROWSER_HEADLESS` - Run browser in headless mode (default: true)

## Best Practices

1. **Error Handling**: Always handle errors in skill execution
2. **Resource Cleanup**: Implement `destroy()` method for plugins that allocate resources
3. **Plugin Isolation**: Keep plugins focused on a single domain
4. **Versioning**: Use semantic versioning for plugins and skills
5. **Documentation**: Document skill parameters and return values

## Examples

### Take a Screenshot Using Skills

```bash
POST /skills/take-screenshot/execute?session=default
Content-Type: application/json

{
  "path": "page.png",
  "fullPage": false
}
```

### Extract Page Content

```bash
POST /skills/extract-text/execute?session=default
Content-Type: application/json

{}
```

### Manage Plugin Lifecycle

```bash
# Disable all content extraction skills
POST /plugins/content/disable?session=default

# Re-enable when needed
POST /plugins/content/enable?session=default
```

## Troubleshooting

### Plugin Not Registering
- Check that the plugin ID is unique
- Verify the plugin object structure matches the interface
- Check browser logs for initialization errors

### Skill Execution Fails
- Verify the skill is enabled
- Check that required parameters are provided
- Review the skill's error response for details

### Session Not Found
- Ensure you're using the correct session ID
- Create a new session if needed (it's auto-created on first request)
