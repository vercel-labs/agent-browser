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

        <h2>Use cases</h2>
        <p>This enables control of:</p>
        <ul>
          <li>Electron apps</li>
          <li>Chrome/Chromium with remote debugging</li>
          <li>WebView2 applications</li>
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
              <td><code>--headers &lt;json&gt;</code></td>
              <td>HTTP headers scoped to origin</td>
            </tr>
            <tr>
              <td><code>--executable-path</code></td>
              <td>Custom browser executable</td>
            </tr>
            <tr>
              <td><code>--json</code></td>
              <td>JSON output for agents</td>
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
              <td><code>--cdp &lt;port&gt;</code></td>
              <td>CDP connection port</td>
            </tr>
            <tr>
              <td><code>--debug</code></td>
              <td>Debug output</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  );
}
