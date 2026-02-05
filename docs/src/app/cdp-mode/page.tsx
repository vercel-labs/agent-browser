import { CodeBlock } from "@/components/code-block";

export default function CDPMode() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>CDP Mode</h1>
        <p>Connect to an existing browser via Chrome DevTools Protocol:</p>
        <CodeBlock code={`# Start Chrome with: google-chrome --remote-debugging-port=9222

# Connect once, then run commands without --cdp
agent-browser connect 9222
agent-browser snapshot
agent-browser tab
agent-browser close

# Or pass --cdp on each command
agent-browser --cdp 9222 snapshot`} />

        <h2>Remote WebSocket URLs</h2>
        <p>Connect to remote browser services via WebSocket URL:</p>
        <CodeBlock code={`# Connect to remote browser service
agent-browser --cdp "wss://browser-service.com/cdp?token=..." snapshot

# Works with any CDP-compatible service
agent-browser --cdp "ws://localhost:9222/devtools/browser/abc123" open example.com`} />
        <p>The <code>--cdp</code> flag accepts either:</p>
        <ul>
          <li>A port number (e.g., <code>9222</code>) for local connections via <code>http://localhost:&#123;port&#125;</code></li>
          <li>A full WebSocket URL (e.g., <code>wss://...</code> or <code>ws://...</code>) for remote browser services</li>
        </ul>

        <h2>Use cases</h2>
        <p>This enables control of:</p>
        <ul>
          <li>Electron apps</li>
          <li>Chrome/Chromium with remote debugging</li>
          <li>WebView2 applications</li>
          <li>Remote browser services (via WebSocket URL)</li>
          <li>Any browser exposing a CDP endpoint</li>
        </ul>

        <h2>Global options</h2>
        <table>
          <thead>
            <tr>
              <th>Option</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>--session &lt;name&gt;</code></td>
              <td>Use isolated session</td>
            </tr>
            <tr>
              <td><code>--profile &lt;path&gt;</code></td>
              <td>Persistent browser profile directory</td>
            </tr>
            <tr>
              <td><code>-p &lt;provider&gt;</code></td>
              <td>Cloud browser provider (<code>browserbase</code>, <code>browseruse</code>)</td>
            </tr>
            <tr>
              <td><code>--headers &lt;json&gt;</code></td>
              <td>HTTP headers scoped to origin</td>
            </tr>
            <tr>
              <td><code>--executable-path</code></td>
              <td>Custom browser executable</td>
            </tr>
            <tr>
              <td><code>--args &lt;args&gt;</code></td>
              <td>Browser launch args (comma-separated)</td>
            </tr>
            <tr>
              <td><code>--user-agent &lt;ua&gt;</code></td>
              <td>Custom User-Agent string</td>
            </tr>
            <tr>
              <td><code>--proxy &lt;url&gt;</code></td>
              <td>Proxy server URL</td>
            </tr>
            <tr>
              <td><code>--proxy-bypass &lt;hosts&gt;</code></td>
              <td>Hosts to bypass proxy</td>
            </tr>
            <tr>
              <td><code>--json</code></td>
              <td>JSON output for scripts</td>
            </tr>
            <tr>
              <td><code>--full, -f</code></td>
              <td>Full page screenshot</td>
            </tr>
            <tr>
              <td><code>--name, -n</code></td>
              <td>Locator name filter</td>
            </tr>
            <tr>
              <td><code>--exact</code></td>
              <td>Exact text match</td>
            </tr>
            <tr>
              <td><code>--headed</code></td>
              <td>Show browser window</td>
            </tr>
            <tr>
              <td><code>--cdp &lt;port|url&gt;</code></td>
              <td>CDP connection (port or WebSocket URL)</td>
            </tr>
            <tr>
              <td><code>--debug</code></td>
              <td>Debug output</td>
            </tr>
          </tbody>
        </table>

        <h2>Cloud providers</h2>
        <p>Use cloud browser infrastructure when local browsers aren&apos;t available:</p>
        <CodeBlock code={`# Browserbase
export BROWSERBASE_API_KEY="your-api-key"
export BROWSERBASE_PROJECT_ID="your-project-id"
agent-browser -p browserbase open https://example.com

# Browser Use
export BROWSER_USE_API_KEY="your-api-key"
agent-browser -p browseruse open https://example.com

# Or via environment variable
export AGENT_BROWSER_PROVIDER=browserbase
agent-browser open https://example.com`} />
        <p>The <code>-p</code> flag takes precedence over <code>AGENT_BROWSER_PROVIDER</code>.</p>
      </div>
    </div>
  );
}
