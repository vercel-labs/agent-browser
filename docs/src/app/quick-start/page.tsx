import { CodeBlock } from "@/components/code-block";

export default function QuickStart() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Quick Start</h1>

        <h2>Core workflow</h2>
        <p>Every browser automation follows this pattern:</p>
        <CodeBlock code={`# 1. Navigate
agent-browser open example.com

# 2. Snapshot to get element refs
agent-browser snapshot -i
# Output:
# @e1 [heading] "Example Domain"
# @e2 [link] "More information..."

# 3. Interact using refs
agent-browser click @e2

# 4. Re-snapshot after page changes
agent-browser snapshot -i`} />

        <h2>Common commands</h2>
        <CodeBlock code={`agent-browser open example.com
agent-browser snapshot -i                # Get interactive elements with refs
agent-browser click @e2                  # Click by ref
agent-browser fill @e3 "test@example.com" # Fill input by ref
agent-browser get text @e1               # Get text content
agent-browser screenshot                 # Save to temp directory
agent-browser screenshot page.png        # Save to specific path
agent-browser close`} />

        <h2>Traditional selectors</h2>
        <p>CSS selectors and semantic locators also supported:</p>
        <CodeBlock code={`agent-browser click "#submit"
agent-browser fill "#email" "test@example.com"
agent-browser find role button click --name "Submit"`} />

        <h2>Headed mode</h2>
        <p>Show browser window for debugging:</p>
        <CodeBlock code="agent-browser open example.com --headed" />

        <h2>Wait for content</h2>
        <CodeBlock code={`agent-browser wait @e1                   # Wait for element
agent-browser wait --load networkidle    # Wait for network idle
agent-browser wait --url "**/dashboard"  # Wait for URL pattern
agent-browser wait 2000                  # Wait milliseconds`} />

        <h2>JSON output</h2>
        <p>For programmatic parsing in scripts:</p>
        <CodeBlock code={`agent-browser snapshot --json
agent-browser get text @e1 --json`} />
        <p>
          Note: The default text output is more compact and preferred for AI agents.
        </p>
      </div>
    </div>
  );
}
