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
      </div>
    </div>
  );
}
