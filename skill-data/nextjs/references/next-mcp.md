# Next.js dev-server MCP bridge

Next.js exposes a JSON-RPC endpoint at `/_next/mcp` (StreamableHTTP
transport, SSE response) with tools for build errors, runtime errors, dev
server logs, route metadata, and server action lookup. This is separate
from the browser - the skill talks to the dev server directly over HTTP.

**Related**: [SKILL.md](../SKILL.md), [templates/next-mcp.sh](../templates/next-mcp.sh).

## Helper script

```bash
bash "$(agent-browser skills path nextjs)/templates/next-mcp.sh" \
  <origin> <tool> [args-json]
```

The helper POSTs a JSON-RPC request to `<origin>/_next/mcp`, parses the
SSE-framed response, and prints the result. It requires `curl`.

## Tools

<table>
<tr><th>Tool</th><th>Description</th><th>Args</th></tr>
<tr>
<td><code>get_errors</code></td>
<td>Build and runtime errors for the current session. Includes config errors, build errors per URL, and runtime console/React errors with stack traces.</td>
<td>none</td>
</tr>
<tr>
<td><code>get_logs</code></td>
<td>Path to the Next.js dev server log file. Read the returned <code>logFilePath</code> to see stdout from the dev server.</td>
<td>none</td>
</tr>
<tr>
<td><code>get_page_metadata</code></td>
<td>Route segments for the current URL: layouts, pages, loading boundaries, etc.</td>
<td>none</td>
</tr>
<tr>
<td><code>get_project_metadata</code></td>
<td>Project root path and dev server URL.</td>
<td>none</td>
</tr>
<tr>
<td><code>get_routes</code></td>
<td>All app router routes as an array of route patterns.</td>
<td>none</td>
</tr>
<tr>
<td><code>get_server_action_by_id</code></td>
<td>Inspect a server action by its ID. The ID comes from the <code>next-action</code> header on <code>POST /...</code> requests.</td>
<td><code>{"actionId": "..."}</code></td>
</tr>
</table>

## Examples

```bash
ORIGIN=http://localhost:3000

# Build and runtime errors
bash "$(agent-browser skills path nextjs)/templates/next-mcp.sh" \
  "$ORIGIN" get_errors

# Current page's route segments
bash "$(agent-browser skills path nextjs)/templates/next-mcp.sh" \
  "$ORIGIN" get_page_metadata

# All routes
bash "$(agent-browser skills path nextjs)/templates/next-mcp.sh" \
  "$ORIGIN" get_routes

# Inspect a server action by ID (the ID comes from the next-action header)
bash "$(agent-browser skills path nextjs)/templates/next-mcp.sh" \
  "$ORIGIN" get_server_action_by_id '{"actionId":"abc123def"}'
```

## Errors vs console output

<table>
<tr><th>Command</th><th>Source</th><th>Requires dev server</th></tr>
<tr>
<td><code>get_errors</code></td>
<td>Build errors + <code>console.error</code> reported by Next</td>
<td>Yes</td>
</tr>
<tr>
<td><code>get_logs</code></td>
<td>Next.js dev server stdout</td>
<td>Yes</td>
</tr>
<tr>
<td><code>agent-browser console</code></td>
<td>All browser-side console output</td>
<td>No</td>
</tr>
</table>

For dev-server diagnostics, prefer `get_logs` and `get_errors`. Use
`agent-browser console` when you need general browser console output or are
running a production build.
