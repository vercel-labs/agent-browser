import { CodeBlock } from "@/components/code-block";

export default function Sessions() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Sessions</h1>
        <p>Run multiple isolated browser instances:</p>
        <CodeBlock code={`# Different sessions
agent-browser --session agent1 open site-a.com
agent-browser --session agent2 open site-b.com

# Or via environment variable
AGENT_BROWSER_SESSION=agent1 agent-browser click "#btn"

# List active sessions
agent-browser session list
# Output:
# Active sessions:
# -> default
#    agent1

# Show current session
agent-browser session`} />

        <h2>Session isolation</h2>
        <p>Each session has its own:</p>
        <ul>
          <li>Browser instance</li>
          <li>Cookies and storage</li>
          <li>Navigation history</li>
          <li>Authentication state</li>
        </ul>

        <h2>Session persistence</h2>
        <p>
          Automatically save and restore cookies and localStorage across browser restarts using <code>--session-name</code>:
        </p>
        <CodeBlock code={`# Auto-save/load state for "twitter" session
agent-browser --session-name twitter open twitter.com

# Login once, then state persists automatically
agent-browser --session-name twitter click "#login"

# Or via environment variable
export AGENT_BROWSER_SESSION_NAME=twitter
agent-browser open twitter.com`} />
        <p>
          State files are stored in <code>~/.agent-browser/sessions/</code> and automatically loaded on daemon start.
        </p>

        <h3>Session name rules</h3>
        <p>
          Session names must contain only alphanumeric characters, hyphens, and underscores:
        </p>
        <CodeBlock code={`# Valid session names
agent-browser --session-name my-project open example.com
agent-browser --session-name test_session_v2 open example.com

# Invalid (will be rejected)
agent-browser --session-name "../bad" open example.com    # path traversal
agent-browser --session-name "my session" open example.com # spaces
agent-browser --session-name "foo/bar" open example.com    # slashes`} />

        <h2>State encryption</h2>
        <p>
          Encrypt saved state files (cookies, localStorage) using AES-256-GCM:
        </p>
        <CodeBlock code={`# Generate a 256-bit key (64 hex characters)
openssl rand -hex 32

# Set the encryption key
export AGENT_BROWSER_ENCRYPTION_KEY=<your-64-char-hex-key>

# State files are now encrypted automatically
agent-browser --session-name secure-session open example.com

# List states shows encryption status
agent-browser state list
# Output:
#   secure-session-default.json (2.1KB, 2026-01-14) [encrypted]`} />

        <h2>State auto-expiration</h2>
        <p>
          Automatically delete old state files to prevent accumulation:
        </p>
        <CodeBlock code={`# Set expiration (default: 30 days)
export AGENT_BROWSER_STATE_EXPIRE_DAYS=7

# Manually clean old states
agent-browser state clean --older-than 7
# Output:
# Deleted 3 file(s):
#   - old-session-default.json
#   - test-agent1.json
#   - demo-default.json`} />

        <h2>State management commands</h2>
        <CodeBlock code={`# List all saved states
agent-browser state list

# Show state summary (cookies, origins, domains)
agent-browser state show my-session-default.json

# Rename a state file
agent-browser state rename old-name new-name

# Clear states for a specific session name
agent-browser state clear my-session

# Clear all saved states
agent-browser state clear --all

# Manual save/load (for custom paths)
agent-browser state save ./backup.json
agent-browser state load ./backup.json`} />

        <h2>Authenticated sessions</h2>
        <p>
          Use <code>--headers</code> to set HTTP headers for a specific origin:
        </p>
        <CodeBlock code={`# Headers scoped to api.example.com only
agent-browser open api.example.com --headers '{"Authorization": "Bearer <token>"}'

# Requests to api.example.com include the auth header
agent-browser snapshot -i --json
agent-browser click @e2

# Navigate to another domain - headers NOT sent
agent-browser open other-site.com`} />
        <p>Useful for:</p>
        <ul>
          <li><strong>Skipping login flows</strong> - Authenticate via headers</li>
          <li><strong>Switching users</strong> - Different auth tokens per session</li>
          <li><strong>API testing</strong> - Access protected endpoints</li>
          <li><strong>Security</strong> - Headers scoped to origin, not leaked</li>
        </ul>

        <h2>Multiple origins</h2>
        <CodeBlock code={`agent-browser open api.example.com --headers '{"Authorization": "Bearer token1"}'
agent-browser open api.acme.com --headers '{"Authorization": "Bearer token2"}'`} />

        <h2>Global headers</h2>
        <p>For headers on all domains:</p>
        <CodeBlock code={`agent-browser set headers '{"X-Custom-Header": "value"}'`} />

        <h2>Environment variables</h2>
        <table>
          <thead>
            <tr>
              <th>Variable</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>AGENT_BROWSER_SESSION</code></td>
              <td>Browser session ID (default: &quot;default&quot;)</td>
            </tr>
            <tr>
              <td><code>AGENT_BROWSER_SESSION_NAME</code></td>
              <td>Auto-save/load state persistence name</td>
            </tr>
            <tr>
              <td><code>AGENT_BROWSER_ENCRYPTION_KEY</code></td>
              <td>64-char hex key for AES-256-GCM encryption</td>
            </tr>
            <tr>
              <td><code>AGENT_BROWSER_STATE_EXPIRE_DAYS</code></td>
              <td>Auto-delete states older than N days (default: 30)</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  );
}
