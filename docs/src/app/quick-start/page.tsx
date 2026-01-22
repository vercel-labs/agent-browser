import { CodeBlock } from "@/components/code-block";

export default function QuickStart() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Quick Start</h1>

        <h2>Basic workflow</h2>
        <CodeBlock code={`agent-browser open example.com
agent-browser snapshot                    # Get accessibility tree with refs
agent-browser click @e2                   # Click by ref from snapshot
agent-browser fill @e3 "test@example.com" # Fill by ref
agent-browser get text @e1                # Get text by ref
agent-browser screenshot                  # Base64 png to stdout
agent-browser screenshot page.png         # Save to file
agent-browser close`} />

        <h2>Traditional selectors</h2>
        <p>CSS selectors and semantic locators also supported:</p>
        <CodeBlock code={`agent-browser click "#submit"
agent-browser fill "#email" "test@example.com"
agent-browser find role button click --name "Submit"`} />

        <h2>AI workflow</h2>
        <p>Optimal workflow for AI agents:</p>
        <CodeBlock code={`# 1. Navigate and get snapshot
agent-browser open example.com
agent-browser snapshot -i --json   # AI parses tree and refs

# 2. AI identifies target refs from snapshot
# 3. Execute actions using refs
agent-browser click @e2
agent-browser fill @e3 "input text"

# 4. Get new snapshot if page changed
agent-browser snapshot -i --json`} />

        <h2>Headed mode</h2>
        <p>Show browser window for debugging:</p>
        <CodeBlock code="agent-browser open example.com --headed" />

        <h2>JSON output</h2>
        <p>Use <code>--json</code> for machine-readable output:</p>
        <CodeBlock code={`agent-browser snapshot --json
agent-browser get text @e1 --json
agent-browser is visible @e2 --json`} />
      </div>
    </div>
  );
}
