import { CodeBlock } from "@/components/code-block";

export default function Snapshots() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Snapshots</h1>
        <p>
          The <code>snapshot</code> command returns the accessibility tree with refs for AI-friendly interaction.
        </p>

        <h2>Options</h2>
        <p>Filter output to reduce size:</p>
        <CodeBlock code={`agent-browser snapshot                    # Full accessibility tree
agent-browser snapshot -i                 # Interactive elements only
agent-browser snapshot -c                 # Compact (remove empty elements)
agent-browser snapshot -d 3               # Limit depth to 3 levels
agent-browser snapshot -s "#main"         # Scope to CSS selector
agent-browser snapshot -i -c -d 5         # Combine options`} />

        <table>
          <thead>
            <tr>
              <th>Option</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>-i, --interactive</code></td>
              <td>Only interactive elements (buttons, links, inputs)</td>
            </tr>
            <tr>
              <td><code>-c, --compact</code></td>
              <td>Remove empty structural elements</td>
            </tr>
            <tr>
              <td><code>-d, --depth</code></td>
              <td>Limit tree depth</td>
            </tr>
            <tr>
              <td><code>-s, --selector</code></td>
              <td>Scope to CSS selector</td>
            </tr>
          </tbody>
        </table>

        <h2>Output format</h2>
        <CodeBlock code={`agent-browser snapshot
# Output:
# - heading "Example Domain" [ref=e1] [level=1]
# - button "Submit" [ref=e2]
# - textbox "Email" [ref=e3]
# - link "Learn more" [ref=e4]`} />

        <h2>JSON output</h2>
        <p>Use <code>--json</code> for machine-readable output:</p>
        <CodeBlock code={`agent-browser snapshot --json
# {"success":true,"data":{"snapshot":"...","refs":{"e1":{"role":"heading","name":"Title"},...}}}`} />

        <h2>Best practices</h2>
        <ol>
          <li>Use <code>-i</code> to reduce output to actionable elements</li>
          <li>Use <code>--json</code> for structured parsing</li>
          <li>Re-snapshot after page changes to get updated refs</li>
          <li>Scope with <code>-s</code> for specific page sections</li>
        </ol>
      </div>
    </div>
  );
}
