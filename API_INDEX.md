# Agent Browser - Complete API Index

## Overview

Agent Browser now includes comprehensive HTTP APIs for browser automation, skills management, and real-time collaboration. Optimized for AI agents, humans, and multi-session automation.

## 🚀 Quick Links

| Feature | Documentation | Status |
|---------|---------------|--------|
| **Browser Control** | [BROWSER_API.md](./BROWSER_API.md) | ✅ 60+ endpoints |
| **Screencast & Input** | [SCREENCAST_API.md](./SCREENCAST_API.md) | ✅ Live streaming |
| **Skills & Plugins** | [SKILLS.md](./SKILLS.md) | ✅ Pluggable system |
| **Worker Setup** | [CLOUDFLARE_WORKER.md](./CLOUDFLARE_WORKER.md) | ✅ Tested & verified |

## API Categories

### 1. Browser Control (60+ endpoints)

Complete HTTP API for browser automation with 60+ endpoints organized by category:

#### Navigation
- `POST /browser/navigate` - Go to URL
- `GET /browser/back` - Go back
- `GET /browser/forward` - Go forward
- `GET /browser/reload` - Reload page
- `GET /browser/url` - Get current URL
- `GET /browser/title` - Get page title

#### Content & Screenshots
- `GET /browser/content` - Get page text
- `GET /browser/screenshot` - Take screenshot
- `GET /browser/snapshot` - Get DOM snapshot

#### Element Interaction (12 actions)
- `/browser/click` - Click element
- `/browser/type` - Type text
- `/browser/fill` - Fill form
- `/browser/clear` - Clear input
- `/browser/focus` - Focus element
- `/browser/hover` - Hover element
- `/browser/check` - Check checkbox
- `/browser/uncheck` - Uncheck checkbox
- `/browser/select` - Select dropdown
- `/browser/dblclick` - Double-click
- `/browser/tap` - Tap (mobile)
- `/browser/press` - Press key

#### Element Queries (8 endpoints)
- `/browser/element/:selector/text` - Get text
- `/browser/element/:selector/attribute` - Get attribute
- `/browser/element/:selector/visible` - Check visibility
- `/browser/element/:selector/enabled` - Check enabled
- `/browser/element/:selector/checked` - Check checked
- `/browser/element/:selector/boundingbox` - Get position
- `/browser/element/:selector/count` - Count elements

#### Accessibility Queries (6 endpoints) - AI-optimized
- `POST /browser/getbyrole` - Find by role
- `POST /browser/getbytext` - Find by text
- `POST /browser/getbylabel` - Find by label
- `POST /browser/getbyplaceholder` - Find by placeholder
- `POST /browser/getbyalttext` - Find by alt text
- `POST /browser/getbytestid` - Find by test ID

#### Wait & Conditions (3 endpoints)
- `POST /browser/wait` - Wait for element
- `POST /browser/waitfor` - Wait for condition
- `POST /browser/waitforloadstate` - Wait for load state

#### Storage & Cookies (6 endpoints)
- `GET/POST/DELETE /browser/cookies` - Cookie management
- `GET/POST/DELETE /browser/storage` - Storage management

#### JavaScript Execution
- `POST /browser/evaluate` - Run JavaScript

**[Full documentation →](./BROWSER_API.md)**

### 2. Screencast & Input Injection

Real-time video streaming and remote input control for collaborative automation:

#### Screencast Control (3 endpoints)
- `POST /screencast/start` - Start live video
- `GET /screencast/stop` - Stop streaming
- `GET /screencast/status` - Get status

#### Input Injection (3 endpoints)
- `POST /input/mouse` - Send mouse events
- `POST /input/keyboard` - Send keyboard events
- `POST /input/touch` - Send touch events

#### WebSocket Streaming
- `WS /stream` - Real-time frame streaming

**Features:**
- ✅ Multiple presets (hd, balanced, low, mobile)
- ✅ JPEG/PNG formats
- ✅ Quality control (0-100)
- ✅ Frame rate control
- ✅ Session isolation
- ✅ Multi-client streaming
- ✅ Mouse, keyboard, touch events
- ✅ Modifier support (Shift, Ctrl, Alt, Meta)
- ✅ Multi-touch gestures

**[Full documentation →](./SCREENCAST_API.md)**

### 3. Skills & Plugins

Pluggable skills system for custom capabilities:

#### Skills Management
- `GET /skills` - List all skills
- `GET /skills/:id` - Get skill details
- `POST /skills/:id/execute` - Execute skill

#### Plugin Management
- `GET /plugins` - List plugins
- `POST /plugins/:id/enable` - Enable plugin
- `POST /plugins/:id/disable` - Disable plugin

**Built-in Plugins:**
- **Content Plugin** - Text and HTML extraction
- **Screenshot Plugin** - Screenshot capture (configurable)
- **PDF Plugin** - PDF export (configurable)

**Features:**
- ✅ Per-session skill management
- ✅ Plugin lifecycle (init, enable, disable, destroy)
- ✅ Enable/disable skills per plugin
- ✅ Custom plugin support
- ✅ Plugin versioning

**[Full documentation →](./SKILLS.md)**

### 4. AI-Specific Endpoints

Simplified endpoints optimized for AI agent consumption:

#### AI Operations
- `POST /ai/understand` - Analyze page structure
- `POST /ai/find` - Find element by text
- `POST /ai/interact` - Click element
- `POST /ai/fill` - Fill form field
- `POST /ai/extract` - Extract page data
- `POST /ai/analyze` - Run custom analysis

**[Full documentation →](./BROWSER_API.md#ai-specific-endpoints)**

### 5. Health & Status

#### Health Check
- `GET /health` - Server health and capabilities

Response includes:
```json
{
  "status": "ok",
  "version": "0.6.0",
  "session": "default",
  "endpoints": ["browser", "skills", "plugins"]
}
```

## Session Management

All endpoints support session isolation:

```bash
# Method 1: Query parameter
curl http://localhost:8787/browser/navigate?session=user-123

# Method 2: Header
curl -H "X-Session-ID: user-123" http://localhost:8787/browser/navigate
```

Each session gets:
- ✅ Isolated browser instance
- ✅ Separate skills/plugin state
- ✅ Independent screencast stream
- ✅ Session-specific storage

## Authentication & Security

For Cloudflare Workers deployment, add authentication:

```bash
# With API key
curl -H "Authorization: Bearer sk_live_..." http://api.example.com/browser/navigate
```

## Rate Limits

Configuration via environment variables:
- Development: Unlimited
- Production: Configure in wrangler.toml

## Deployment Options

### Local Development
```bash
npm run worker:dev
# Server at http://localhost:8787
```

### Cloudflare Workers
```bash
npm run worker:deploy
# Deployed globally with Cloudflare
```

### Docker
```bash
docker build -t agent-browser .
docker run -p 8787:8787 agent-browser
```

## Use Cases

### 1. AI Automation
```bash
# AI agent analyzes page
POST /ai/understand

# AI finds element by text (semantic)
POST /browser/getbytext -d '{"text":"Login"}'

# AI clicks element
POST /browser/getbyrole -d '{"role":"button"}'
```

### 2. Pair Programming
```bash
# Agent 1 streams browser
POST /screencast/start?preset=hd

# Agent 2 monitors
wscat -c ws://localhost:8787/stream

# Both control input
POST /input/mouse
POST /input/keyboard
```

### 3. Monitoring
```bash
# Watch AI agent in real-time
wscat -c ws://localhost:8787/stream?session=agent-123

# Log automation actions
curl http://localhost:8787/browser/screenshot
```

### 4. Testing
```bash
# Test web app with automation
POST /browser/navigate -d '{"url":"http://localhost:3000"}'
POST /browser/getbytext -d '{"text":"Login","subaction":"click"}'
POST /browser/screenshot > result.png
```

### 5. Web Scraping
```bash
# Navigate
POST /browser/navigate -d '{"url":"https://example.com"}'

# Extract data
GET /browser/content
GET /browser/snapshot

# Get specific elements
GET /browser/element/h1/text
GET /browser/element/a/count
```

## Architecture

```
┌─────────────────────────────────────────┐
│      Cloudflare Worker (Browser API)    │
├─────────────────────────────────────────┤
│                                          │
│  ┌──────────────────────────────────┐  │
│  │  Skills Manager (Pluggable)      │  │
│  │  - extract-text                  │  │
│  │  - extract-html                  │  │
│  │  - Custom plugins                │  │
│  └──────────────────────────────────┘  │
│                                          │
│  ┌──────────────────────────────────┐  │
│  │  Browser API (60+ endpoints)     │  │
│  │  - Navigation                    │  │
│  │  - Element interaction           │  │
│  │  - Content extraction            │  │
│  │  - Accessibility queries (AI)    │  │
│  └──────────────────────────────────┘  │
│                                          │
│  ┌──────────────────────────────────┐  │
│  │  Screencast & Input Injection    │  │
│  │  - Live video streaming          │  │
│  │  - Mouse/keyboard/touch input    │  │
│  │  - WebSocket stream              │  │
│  └──────────────────────────────────┘  │
│                                          │
│  ┌──────────────────────────────────┐  │
│  │  Session Manager                 │  │
│  │  - Per-session isolation         │  │
│  │  - Browser instance per session  │  │
│  │  - State management              │  │
│  └──────────────────────────────────┘  │
│                                          │
└─────────────────────────────────────────┘
         ↓
    Playwright Browser
```

## Performance Benchmarks

| Operation | Time | Notes |
|-----------|------|-------|
| Navigate | 500-2000ms | Depends on page |
| Screenshot | 100-300ms | Balanced quality |
| Click | 50-100ms | Element must be visible |
| Type text | 20ms per char | Depends on delays |
| Evaluate JS | 50-200ms | Depends on script |
| Content extract | 100-500ms | Depends on page size |
| Query element | 10-50ms | CSS selector |
| Find by role | 50-100ms | Accessibility API |

## Error Handling

All endpoints return consistent error format:

```json
{
  "success": false,
  "error": "Element not found: #submit",
  "code": "NOT_FOUND"
}
```

Common status codes:
- `200` - Success
- `202` - Accepted (queued)
- `400` - Bad request
- `404` - Not found
- `500` - Internal error

## Examples

### Complete Flow: Login Automation
```bash
# 1. Navigate
curl -X POST http://localhost:8787/browser/navigate \
  -d '{"url":"https://example.com/login"}'

# 2. Find email field by label
curl -X POST http://localhost:8787/browser/getbylabel \
  -d '{"label":"Email","subaction":"fill","value":"user@example.com"}'

# 3. Find password field
curl -X POST http://localhost:8787/browser/getbylabel \
  -d '{"label":"Password","subaction":"fill","value":"secret123"}'

# 4. Find submit button by role
curl -X POST http://localhost:8787/browser/getbyrole \
  -d '{"role":"button","name":"Login","subaction":"click"}'

# 5. Wait for redirect
curl -X POST http://localhost:8787/browser/waitforloadstate \
  -d '{"state":"networkidle","timeout":5000}'

# 6. Take screenshot
curl http://localhost:8787/browser/screenshot > dashboard.png

# 7. Extract content
curl http://localhost:8787/browser/content
```

### Browser Monitoring
```bash
# Start screencast
curl -X POST http://localhost:8787/screencast/start?preset=balanced

# Connect via WebSocket to monitor
wscat -c ws://localhost:8787/stream

# Frames received in real-time as base64 images
```

## Getting Started

### 1. Start Local Worker
```bash
npm run worker:dev
```

### 2. Test Health
```bash
curl http://localhost:8787/health
```

### 3. Try First Command
```bash
curl -X POST http://localhost:8787/browser/navigate \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com"}'
```

### 4. Read Full Docs
- Browser API: [BROWSER_API.md](./BROWSER_API.md)
- Screencast: [SCREENCAST_API.md](./SCREENCAST_API.md)
- Skills: [SKILLS.md](./SKILLS.md)

## Contributing

To add new endpoints:

1. Define in `api-routes.ts`
2. Handle in `worker-full.ts`
3. Document in respective markdown
4. Test locally: `npm run worker:dev`
5. Submit PR

## Version

- **Version**: 0.6.0
- **APIs**: 60+ endpoints
- **Deployment**: Cloudflare Workers ✅
- **Status**: Production ready ✅

## License

Apache 2.0 - See [LICENSE](./LICENSE)

## Support

- Issues: GitHub Issues
- Docs: See markdown files in root
- Examples: Check BROWSER_API.md and SCREENCAST_API.md
