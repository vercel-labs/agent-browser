# Workflow Management API

Complete workflow orchestration for browser automation, data extraction, and monitoring tasks.

## Overview

The Workflow API enables you to:
- ✅ Create, edit, delete, and list workflows
- ✅ Execute workflows with session isolation
- ✅ Track execution history and results
- ✅ Use pre-built workflow templates
- ✅ Chain multiple browser actions together
- ✅ Persist workflows in Cloudflare KV storage
- ✅ Store screenshots and results in R2
- ✅ Import/export workflows as JSON

## Quick Start

### Create a Workflow
```bash
curl -X POST http://localhost:8787/workflows \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Login Workflow",
    "description": "Automated login flow",
    "steps": [
      {
        "id": "navigate",
        "action": "navigate",
        "params": {"url": "https://example.com/login"}
      },
      {
        "id": "fill-email",
        "action": "fill",
        "params": {"selector": "input[type=email]", "value": "user@example.com"}
      },
      {
        "id": "fill-password",
        "action": "fill",
        "params": {"selector": "input[type=password]", "value": "password123"}
      },
      {
        "id": "submit",
        "action": "click",
        "params": {"selector": "button[type=submit]"}
      }
    ]
  }'
```

### Execute a Workflow
```bash
curl -X POST http://localhost:8787/workflows/{workflowId}/execute \
  -H "Content-Type: application/json" \
  -d '{
    "sessionId": "user-123"
  }'
```

### List Workflows
```bash
curl http://localhost:8787/workflows
```

## Workflow Endpoints

### Workflow CRUD

#### Create Workflow
```bash
POST /workflows
Content-Type: application/json

{
  "name": "Workflow Name",
  "description": "What this workflow does",
  "steps": [
    {
      "id": "step-1",
      "action": "navigate",
      "params": {"url": "https://example.com"},
      "timeout": 5000,
      "retries": 1
    }
  ],
  "tags": ["automation", "login"],
  "enabled": true,
  "metadata": {
    "author": "ai-agent",
    "version": "1.0"
  }
}
```

Response:
```json
{
  "success": true,
  "data": {
    "id": "wf-1234567890",
    "name": "Workflow Name",
    "createdAt": 1234567890000,
    "updatedAt": 1234567890000
  }
}
```

#### Get Workflow
```bash
GET /workflows/{workflowId}
```

#### List Workflows
```bash
GET /workflows?tags=login&enabled=true&createdBy=ai-agent
```

#### Update Workflow
```bash
PUT /workflows/{workflowId}
Content-Type: application/json

{
  "name": "Updated Name",
  "enabled": false,
  "steps": [...]
}
```

#### Delete Workflow
```bash
DELETE /workflows/{workflowId}
```

#### Clone Workflow
```bash
POST /workflows/{workflowId}/clone
Content-Type: application/json

{
  "newName": "Login Workflow - Copy"
}
```

### Workflow Execution

#### Execute Workflow
```bash
POST /workflows/{workflowId}/execute
Content-Type: application/json

{
  "sessionId": "user-123",
  "variables": {
    "email": "test@example.com",
    "password": "secret123"
  },
  "parallel": false
}
```

Response:
```json
{
  "success": true,
  "data": {
    "executionId": "exec-1234567890",
    "status": "running",
    "startedAt": 1234567890000
  }
}
```

#### Get Execution Status
```bash
GET /workflows/{workflowId}/executions/{executionId}
```

Response:
```json
{
  "success": true,
  "data": {
    "id": "exec-1234567890",
    "workflowId": "wf-1234567890",
    "status": "success",
    "startedAt": 1234567890000,
    "completedAt": 1234567891000,
    "results": {
      "screenshot": "data:image/png;base64,...",
      "content": "Page content here"
    },
    "errors": []
  }
}
```

#### List Executions
```bash
GET /workflows/{workflowId}/executions
```

#### Cancel Execution
```bash
DELETE /workflows/{workflowId}/executions/{executionId}
```

### Workflow Templates

#### List Templates
```bash
GET /workflows/templates
```

Available templates:
- `login` - Login automation
- `formFill` - Form submission
- `dataExtraction` - Data scraping
- `monitoring` - Page monitoring
- `search` - Search and results extraction

#### Get Template
```bash
GET /workflows/templates/{templateId}
```

#### Create from Template
```bash
POST /workflows/from-template
Content-Type: application/json

{
  "templateId": "login",
  "name": "My Login Workflow",
  "variables": {
    "loginUrl": "https://myapp.com/login",
    "emailSelector": "input#email",
    "passwordSelector": "input#password",
    "submitSelector": "button.login",
    "email": "user@example.com",
    "password": "secret"
  }
}
```

### Import/Export

#### Export Workflow
```bash
GET /workflows/{workflowId}/export
```

Response: JSON file download
```json
{
  "id": "wf-1234567890",
  "name": "Login Workflow",
  "description": "...",
  "steps": [...]
}
```

#### Import Workflow
```bash
POST /workflows/import
Content-Type: application/json

{
  "json": "{\"id\":\"...\",\"name\":\"...\",\"steps\":[...]}"
}
```

## Workflow Steps

Each step in a workflow represents a browser action.

### Step Properties

```typescript
{
  id: string;              // Unique step identifier
  action: string;          // Action to perform (navigate, click, fill, etc.)
  params: object;          // Action parameters
  condition?: object;      // Optional conditional execution
  retries?: number;        // Number of retries on failure
  timeout?: number;        // Timeout in milliseconds
}
```

### Available Actions

#### Navigation
- `navigate` - Go to URL
- `back` - Go back
- `forward` - Go forward
- `reload` - Reload page

#### Element Interaction
- `click` - Click element
- `type` - Type text
- `fill` - Fill input (with clear)
- `clear` - Clear input
- `focus` - Focus element
- `hover` - Hover element
- `select` - Select option
- `check` - Check checkbox
- `uncheck` - Uncheck checkbox
- `press` - Press key

#### Content Extraction
- `gettext` - Get element text
- `getbytext` - Find by text
- `getbyrole` - Find by role
- `snapshot` - Get DOM snapshot
- `screenshot` - Take screenshot
- `evaluate` - Execute JavaScript

#### Waiting
- `wait` - Wait for element
- `waitforloadstate` - Wait for load

#### Storage
- `cookies_get` - Get cookies
- `cookies_set` - Set cookies
- `storage_get` - Get storage

### Step Examples

#### Navigate
```json
{
  "id": "step-1",
  "action": "navigate",
  "params": {
    "url": "https://example.com",
    "waitUntil": "networkidle"
  },
  "timeout": 10000
}
```

#### Click with Retries
```json
{
  "id": "step-2",
  "action": "click",
  "params": {
    "selector": "button.submit"
  },
  "retries": 3,
  "timeout": 5000
}
```

#### Fill Form
```json
{
  "id": "step-3",
  "action": "fill",
  "params": {
    "selector": "input#email",
    "value": "{{ email }}"
  }
}
```

#### Conditional Step
```json
{
  "id": "step-4",
  "action": "click",
  "params": {
    "selector": "button.logout"
  },
  "condition": {
    "type": "if",
    "field": "loggedIn",
    "value": true
  }
}
```

## Built-in Templates

### Login Template
```json
{
  "id": "template-login",
  "name": "Login Workflow",
  "description": "Automated login flow",
  "variables": {
    "loginUrl": "https://example.com/login",
    "emailSelector": "input[type=email]",
    "passwordSelector": "input[type=password]",
    "submitSelector": "button[type=submit]",
    "email": "user@example.com",
    "password": "password123"
  }
}
```

### Data Extraction Template
```json
{
  "id": "template-extract",
  "name": "Data Extraction Workflow",
  "description": "Navigate and extract structured data",
  "variables": {
    "targetUrl": "https://example.com",
    "selectors": {
      "title": "h1",
      "description": "p.desc",
      "price": "span.price"
    }
  }
}
```

### Monitoring Template
```json
{
  "id": "template-monitor",
  "name": "Monitoring Workflow",
  "description": "Monitor page for changes",
  "variables": {
    "pageUrl": "https://example.com",
    "monitoringScript": "document.querySelectorAll('.item').length"
  }
}
```

### Search Template
```json
{
  "id": "template-search",
  "name": "Search Workflow",
  "description": "Search and extract results",
  "variables": {
    "searchUrl": "https://example.com/search",
    "searchSelector": "input#q",
    "query": "{{ searchTerm }}"
  }
}
```

### Form Fill Template
```json
{
  "id": "template-form",
  "name": "Form Fill Workflow",
  "description": "Fill and submit a form",
  "variables": {
    "formUrl": "https://example.com/form",
    "fields": {
      "name": "input#name",
      "email": "input#email",
      "country": "select#country"
    }
  }
}
```

## Cloudflare Bindings Configuration

Store workflows persistently using Cloudflare bindings.

### wrangler.toml Setup

```toml
# KV Namespaces for workflow storage
[[kv_namespaces]]
binding = "WORKFLOWS"
id = "your-workflows-namespace-id"

[[kv_namespaces]]
binding = "EXECUTIONS"
id = "your-executions-namespace-id"

[[kv_namespaces]]
binding = "CACHE"
id = "your-cache-namespace-id"

# R2 Bucket for screenshots and exports
[[r2_buckets]]
binding = "STORAGE"
bucket_name = "agent-browser-storage"

# D1 Database for structured data
[[d1_databases]]
binding = "DB"
database_name = "agent-browser"
database_id = "your-database-id"
```

### Using Bindings in Worker

```typescript
import { WorkflowKVStorage, WorkflowR2Storage } from './worker-bindings.js';

export default {
  async fetch(request: Request, env: any): Promise<Response> {
    // Create KV storage helper
    const kvStorage = new WorkflowKVStorage(env.WORKFLOWS);

    // Save workflow
    await kvStorage.saveWorkflow(workflowId, workflow);

    // Get workflow
    const saved = await kvStorage.getWorkflow(workflowId);

    // Create R2 storage helper
    const r2Storage = new WorkflowR2Storage(env.STORAGE);

    // Save screenshot
    await r2Storage.saveScreenshot(
      workflowId,
      executionId,
      'screenshot.png',
      imageData
    );

    // ...
  }
};
```

## Execution Flow

### Step-by-Step Execution
```
1. Receive /workflows/:id/execute request
2. Validate workflow exists and is enabled
3. Create execution record
4. For each step:
   a. Check condition (if present)
   b. Execute action
   c. Store result
   d. On error: retry or fail
5. Complete execution
6. Store results in KV/R2
7. Return execution status
```

### Execution Results

Each execution stores:
```json
{
  "id": "exec-123",
  "workflowId": "wf-123",
  "sessionId": "user-123",
  "status": "success|failed|cancelled",
  "startedAt": 1234567890000,
  "completedAt": 1234567891000,
  "results": {
    "step-1": { "url": "https://example.com" },
    "step-2": { "clicked": true },
    "step-3": { "screenshot": "..." }
  },
  "errors": [
    {
      "stepId": "step-4",
      "error": "Element not found",
      "timestamp": 1234567891000
    }
  ]
}
```

## Use Cases

### 1. Login Automation
```bash
# Use login template
POST /workflows/from-template
{
  "templateId": "login",
  "name": "Login My App",
  "variables": {
    "loginUrl": "https://myapp.com/login",
    "email": "bot@example.com",
    "password": "secret"
  }
}

# Execute workflow
POST /workflows/{workflowId}/execute
{
  "sessionId": "bot-session-1"
}
```

### 2. Data Extraction
```bash
# Create extraction workflow
POST /workflows
{
  "name": "Product List Extraction",
  "steps": [
    {"id": "nav", "action": "navigate", "params": {"url": "..."}},
    {"id": "wait", "action": "waitforloadstate", "params": {"state": "networkidle"}},
    {"id": "extract", "action": "snapshot", "params": {"interactive": true}},
    {"id": "screenshot", "action": "screenshot", "params": {}}
  ]
}

# Execute and get results
POST /workflows/{workflowId}/execute
```

### 3. Monitoring
```bash
# Create monitoring workflow
POST /workflows/from-template
{
  "templateId": "monitoring",
  "name": "Price Monitor",
  "variables": {
    "pageUrl": "https://shop.com/product",
    "monitoringScript": "document.querySelector('.price').textContent"
  }
}

# Execute periodically via scheduled triggers
```

### 4. Testing
```bash
# Create test workflow
POST /workflows
{
  "name": "Sign-up Flow Test",
  "steps": [
    {"id": "nav", "action": "navigate", "params": {"url": "..."}},
    {"id": "fill-email", "action": "fill", "params": {"selector": "...", "value": "..."}},
    {"id": "submit", "action": "click", "params": {"selector": "..."}},
    {"id": "verify", "action": "screenshot", "params": {"fullPage": true}}
  ]
}
```

## Performance Tuning

### Parallel Execution
```bash
POST /workflows/{workflowId}/execute
{
  "parallel": true
}
```

### Timeouts
```json
{
  "id": "step-1",
  "action": "navigate",
  "params": {"url": "..."},
  "timeout": 10000
}
```

### Retries
```json
{
  "id": "step-2",
  "action": "click",
  "params": {"selector": "..."},
  "retries": 3
}
```

## API Response Codes

- `200` - Success
- `201` - Created
- `202` - Accepted (execution started)
- `400` - Bad request
- `404` - Workflow not found
- `409` - Conflict (already exists)
- `500` - Internal error

## Storage

### KV Storage (default)
```
workflow:{id}          -> Workflow JSON
execution:{wfId}:{execId} -> Execution results
screenshot:{execId}:{file} -> Screenshot base64
session:{sessionId}    -> Session data
```

### R2 Storage (optional)
```
workflows/{workflowId}/{executionId}/screenshot.png
exports/workflows/{workflowId}-{timestamp}.json
reports/{workflowId}/{executionId}.html
```

## Error Handling

Execution errors include:
- Step execution timeout
- Element not found
- Network error
- Script evaluation error
- Invalid parameters

Errors are stored and included in execution results.

## Workflow Versions

Track workflow versions:
```json
{
  "id": "wf-123",
  "version": "1.0.0",
  "previousVersions": ["0.9.0", "0.8.0"]
}
```

## Audit Trail

All workflow changes are tracked:
```json
{
  "workflowId": "wf-123",
  "action": "updated",
  "changedBy": "ai-agent",
  "timestamp": 1234567890000,
  "changes": {
    "enabled": false
  }
}
```

## Examples

### Complete Login & Capture Flow
```bash
# 1. Create workflow
curl -X POST http://localhost:8787/workflows \
  -d '{
    "name": "Login and Capture",
    "steps": [
      {"id": "nav", "action": "navigate", "params": {"url": "https://example.com/login"}},
      {"id": "email", "action": "fill", "params": {"selector": "input#email", "value": "{{ email }}"}},
      {"id": "pass", "action": "fill", "params": {"selector": "input#password", "value": "{{ password }}"}},
      {"id": "click", "action": "click", "params": {"selector": "button[type=submit]"}},
      {"id": "wait", "action": "waitforloadstate", "params": {"state": "networkidle"}},
      {"id": "screenshot", "action": "screenshot", "params": {"fullPage": true}}
    ]
  }'

# 2. Execute workflow
curl -X POST http://localhost:8787/workflows/{workflowId}/execute \
  -d '{
    "sessionId": "user-123",
    "variables": {"email": "user@example.com", "password": "secret"}
  }'

# 3. Get results
curl http://localhost:8787/workflows/{workflowId}/executions/{executionId}
```

## See Also

- [BROWSER_API.md](./BROWSER_API.md) - Available browser actions
- [API_INDEX.md](./API_INDEX.md) - All endpoints
- [SCREENCAST_API.md](./SCREENCAST_API.md) - Real-time monitoring
