# Browser Automation API

Complete HTTP API for AI-powered browser automation. All endpoints support session isolation and are optimized for AI agents.

## Quick Start

### Health Check
```bash
curl http://localhost:8787/health
```

### Navigate to URL
```bash
curl -X POST http://localhost:8787/browser/navigate \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}'
```

### Take Screenshot
```bash
curl http://localhost:8787/browser/screenshot
```

### Get Page Content
```bash
curl http://localhost:8787/browser/content
```

## Browser Control Endpoints

### Navigation

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/browser/navigate` | POST | Navigate to URL |
| `/browser/goto` | POST | Alias for navigate |
| `/browser/back` | GET | Go back in history |
| `/browser/forward` | GET | Go forward in history |
| `/browser/reload` | GET | Reload current page |
| `/browser/url` | GET | Get current URL |
| `/browser/title` | GET | Get page title |

**Example - Navigate:**
```bash
curl -X POST http://localhost:8787/browser/navigate \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com",
    "waitUntil": "networkidle"
  }'
```

### Content & Screenshots

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/browser/content` | GET | Get page text content |
| `/browser/screenshot` | GET | Take screenshot (PNG) |
| `/browser/snapshot` | GET | Get interactive DOM snapshot |

**Example - Screenshot:**
```bash
curl "http://localhost:8787/browser/screenshot?fullPage=true" > page.png
```

**Example - Content:**
```bash
curl http://localhost:8787/browser/content
```

### Element Interaction

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/browser/click` | POST | Click element by selector |
| `/browser/type` | POST | Type text into element |
| `/browser/fill` | POST | Fill form field |
| `/browser/clear` | POST | Clear input field |
| `/browser/focus` | POST | Focus element |
| `/browser/hover` | POST | Hover over element |
| `/browser/dblclick` | POST | Double-click element |
| `/browser/check` | POST | Check checkbox |
| `/browser/uncheck` | POST | Uncheck checkbox |
| `/browser/select` | POST | Select option |
| `/browser/tap` | POST | Tap element (mobile) |
| `/browser/press` | POST | Press keyboard key |

**Example - Click:**
```bash
curl -X POST http://localhost:8787/browser/click \
  -H "Content-Type: application/json" \
  -d '{"selector": "button#submit"}'
```

**Example - Type:**
```bash
curl -X POST http://localhost:8787/browser/type \
  -H "Content-Type: application/json" \
  -d '{
    "selector": "input#email",
    "text": "user@example.com"
  }'
```

**Example - Fill:**
```bash
curl -X POST http://localhost:8787/browser/fill \
  -H "Content-Type: application/json" \
  -d '{
    "selector": "input[name=username]",
    "value": "myusername"
  }'
```

### Element Queries

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/browser/element/:selector/text` | GET | Get element text |
| `/browser/element/:selector/attribute` | GET | Get element attribute |
| `/browser/element/:selector/visible` | GET | Check if visible |
| `/browser/element/:selector/enabled` | GET | Check if enabled |
| `/browser/element/:selector/checked` | GET | Check if checked |
| `/browser/element/:selector/boundingbox` | GET | Get bounding box |
| `/browser/element/:selector/count` | GET | Count elements |

**Example - Get Text:**
```bash
curl "http://localhost:8787/browser/element/h1/text"
```

**Example - Count Elements:**
```bash
curl "http://localhost:8787/browser/element/a/count"
```

### Accessibility Queries (Best for AI)

These endpoints use semantic queries instead of selectors - perfect for AI agents!

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/browser/getbyrole` | POST | Find by ARIA role |
| `/browser/getbytext` | POST | Find by text content |
| `/browser/getbylabel` | POST | Find by label |
| `/browser/getbyplaceholder` | POST | Find by placeholder |
| `/browser/getbyalttext` | POST | Find by alt text |
| `/browser/getbytestid` | POST | Find by test ID |

**Example - Find by Role:**
```bash
curl -X POST http://localhost:8787/browser/getbyrole \
  -H "Content-Type: application/json" \
  -d '{
    "role": "button",
    "name": "Submit",
    "subaction": "click"
  }'
```

**Example - Find by Text:**
```bash
curl -X POST http://localhost:8787/browser/getbytext \
  -H "Content-Type: application/json" \
  -d '{
    "text": "Click here",
    "subaction": "click"
  }'
```

**Example - Find by Label:**
```bash
curl -X POST http://localhost:8787/browser/getbylabel \
  -H "Content-Type: application/json" \
  -d '{
    "label": "Email",
    "subaction": "fill",
    "value": "test@example.com"
  }'
```

### Wait & Conditions

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/browser/wait` | POST | Wait for element |
| `/browser/waitfor` | POST | Wait for function |
| `/browser/waitforloadstate` | POST | Wait for load state |

**Example - Wait for Element:**
```bash
curl -X POST http://localhost:8787/browser/wait \
  -H "Content-Type: application/json" \
  -d '{
    "selector": ".results",
    "state": "visible",
    "timeout": 5000
  }'
```

### Storage & Cookies

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/browser/cookies` | GET | Get all cookies |
| `/browser/cookies` | POST | Set cookies |
| `/browser/cookies` | DELETE | Clear cookies |
| `/browser/storage` | GET | Get storage values |
| `/browser/storage` | POST | Set storage values |
| `/browser/storage` | DELETE | Clear storage |

**Example - Get Cookies:**
```bash
curl http://localhost:8787/browser/cookies
```

**Example - Set Cookie:**
```bash
curl -X POST http://localhost:8787/browser/cookies \
  -H "Content-Type: application/json" \
  -d '{
    "cookies": [{
      "name": "sessionId",
      "value": "abc123",
      "domain": "example.com"
    }]
  }'
```

### JavaScript Execution

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/browser/evaluate` | POST | Execute JavaScript |

**Example - Evaluate:**
```bash
curl -X POST http://localhost:8787/browser/evaluate \
  -H "Content-Type: application/json" \
  -d '{
    "script": "document.title",
    "args": []
  }'
```

## AI-Specific Endpoints

Simplified endpoints optimized for AI agent consumption:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/ai/understand` | POST | Analyze page structure |
| `/ai/find` | POST | Find element by text |
| `/ai/interact` | POST | Click element |
| `/ai/fill` | POST | Fill form |
| `/ai/extract` | POST | Extract page data |
| `/ai/analyze` | POST | Run custom analysis |

**Example - Understand Page:**
```bash
curl -X POST http://localhost:8787/ai/understand
```

**Example - Find and Interact:**
```bash
curl -X POST http://localhost:8787/ai/find \
  -H "Content-Type: application/json" \
  -d '{
    "text": "Login Button",
    "action": "click"
  }'
```

## Skills Endpoints

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

## Session Management

### Session ID
Isolate browser instances by session:

```bash
# Method 1: Query parameter
curl http://localhost:8787/browser/navigate?session=user-123 \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}'

# Method 2: Header
curl http://localhost:8787/browser/navigate \
  -H "X-Session-ID: user-123" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}'
```

Each session gets its own browser instance and state.

## Request Format

Most POST endpoints accept JSON body:

```json
{
  "selector": "button.submit",
  "timeout": 5000,
  "waitUntil": "networkidle"
}
```

Query parameters can also be used:
```
POST /browser/click?selector=button.submit&timeout=5000
```

## Response Format

Success response:
```json
{
  "success": true,
  "data": { /* command result */ }
}
```

Error response:
```json
{
  "success": false,
  "error": "Element not found"
}
```

## Best Practices for AI

1. **Use accessibility queries** instead of selectors
   ```bash
   # Good - AI-friendly
   POST /browser/getbytext
   POST /browser/getbyrole

   # Less ideal
   POST /browser/click with selector
   ```

2. **Take snapshots for analysis**
   ```bash
   GET /browser/snapshot  # Get interactive DOM tree
   ```

3. **Wait for conditions**
   ```bash
   POST /browser/waitforloadstate  # Wait for network idle
   ```

4. **Use meaningful content extraction**
   ```bash
   GET /browser/content  # Get page text
   GET /browser/snapshot  # Get DOM structure
   ```

5. **Session isolation**
   ```bash
   # Each user/task gets isolated session
   ?session=agent-task-123
   ```

## Error Handling

Common error codes:
- `400` - Bad request (invalid parameters)
- `404` - Element not found
- `500` - Internal error

All errors return JSON with `error` field.

## Rate Limits

No built-in rate limits in development. In production, configure based on your needs.

## Examples

### Complete Workflow
```bash
# 1. Navigate to site
curl -X POST http://localhost:8787/browser/navigate \
  -d '{"url": "https://example.com"}'

# 2. Find and fill form
curl -X POST http://localhost:8787/browser/getbylabel \
  -d '{"label": "Email", "subaction": "fill", "value": "test@example.com"}'

# 3. Click submit
curl -X POST http://localhost:8787/browser/getbyrole \
  -d '{"role": "button", "name": "Submit", "subaction": "click"}'

# 4. Wait for result
curl -X POST http://localhost:8787/browser/wait \
  -d '{"selector": ".success-message", "state": "visible"}'

# 5. Extract content
curl http://localhost:8787/browser/content

# 6. Take screenshot
curl http://localhost:8787/browser/screenshot > result.png
```

### AI Agent Pattern
```bash
# 1. Get page structure
curl http://localhost:8787/browser/snapshot

# 2. Find interactive elements
curl -X POST http://localhost:8787/browser/getbyrole \
  -d '{"role": "button"}'

# 3. Interact with element
curl -X POST http://localhost:8787/browser/getbyrole \
  -d '{"role": "button", "name": "Next", "subaction": "click"}'

# 4. Analyze result
curl http://localhost:8787/browser/snapshot
```

## See Also

- [SKILLS.md](./SKILLS.md) - Skills and plugins system
- [CLOUDFLARE_WORKER.md](./CLOUDFLARE_WORKER.md) - Worker deployment
- [protocol.ts](./src/protocol.ts) - Full command reference
