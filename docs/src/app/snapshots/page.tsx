import { CodeBlock } from "@/components/code-block";

export default function Snapshots() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Snapshots</h1>
        <p>
          The <code>snapshot</code> command returns a compact accessibility tree with refs for element interaction.
        </p>

        <h2>Options</h2>
        <p>Filter output to reduce size:</p>
        <CodeBlock code={`agent-browser snapshot                    # Full accessibility tree
agent-browser snapshot -i                 # Interactive elements only (recommended)
agent-browser snapshot -i -C              # Include cursor-interactive elements
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
              <td><code>-C, --cursor</code></td>
              <td>Include cursor-interactive elements (cursor:pointer, onclick, tabindex)</td>
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

        <h2>Cursor-interactive elements</h2>
        <p>
          Many modern web apps use custom clickable elements (divs, spans) instead of standard buttons or links.
          The <code>-C</code> flag detects these by looking for:
        </p>
        <ul>
          <li><code>cursor: pointer</code> CSS style</li>
          <li><code>onclick</code> attribute or handler</li>
          <li><code>tabindex</code> attribute (keyboard focusable)</li>
        </ul>
        <CodeBlock code={`agent-browser snapshot -i -C
# Output includes:
# @e1 [button] "Submit"
# @e2 [link] "Learn more"
# Cursor-interactive elements:
# @e3 [clickable] "Menu Item" [cursor:pointer, onclick]
# @e4 [clickable] "Card" [cursor:pointer]`} />

        <h2>Output format</h2>
        <p>The default text output is compact and AI-friendly:</p>
        <CodeBlock code={`agent-browser snapshot -i
# Output:
# @e1 [heading] "Example Domain" [level=1]
# @e2 [button] "Submit"
# @e3 [input type="email"] placeholder="Email"
# @e4 [link] "Learn more"`} />

        <h2>Using refs</h2>
        <p>Refs from the snapshot map directly to commands:</p>
        <CodeBlock code={`agent-browser click @e2      # Click the Submit button
agent-browser fill @e3 "a@b.com"  # Fill the email input
agent-browser get text @e1        # Get heading text`} />

        <h2>Ref lifecycle</h2>
        <p>
          Refs are invalidated when the page changes. Always re-snapshot after navigation or DOM updates:
        </p>
        <CodeBlock code={`agent-browser click @e4      # Navigates to new page
agent-browser snapshot -i    # Get fresh refs
agent-browser click @e1      # Use new refs`} />

        <h2>Best practices</h2>
        <ol>
          <li>Use <code>-i</code> to reduce output to actionable elements</li>
          <li>Re-snapshot after page changes to get updated refs</li>
          <li>Scope with <code>-s</code> for specific page sections</li>
          <li>Use <code>-d</code> to limit depth on complex pages</li>
        </ol>

        <h2>JSON output</h2>
        <p>For programmatic parsing in scripts:</p>
        <CodeBlock code={`agent-browser snapshot --json
# {"success":true,"data":{"snapshot":"...","refs":{"e1":{"role":"heading","name":"Title"},...}}}`} />
        <p>
          Note: JSON uses more tokens than text output. The default text format is preferred for AI agents.
        </p>
      </div>
    </div>
  );
}
