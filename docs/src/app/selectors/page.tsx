import { CodeBlock } from "@/components/code-block";

export default function Selectors() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Selectors</h1>

        <h2>Refs (recommended)</h2>
        <p>
          Refs provide deterministic element selection from snapshots. Best for AI agents.
        </p>
        <CodeBlock code={`# 1. Get snapshot with refs
agent-browser snapshot
# Output:
# - heading "Example Domain" [ref=e1] [level=1]
# - button "Submit" [ref=e2]
# - textbox "Email" [ref=e3]
# - link "Learn more" [ref=e4]

# 2. Use refs to interact
agent-browser click @e2                   # Click the button
agent-browser fill @e3 "test@example.com" # Fill the textbox
agent-browser get text @e1                # Get heading text
agent-browser hover @e4                   # Hover the link`} />

        <h3>Why refs?</h3>
        <ul>
          <li><strong>Deterministic</strong> - Ref points to exact element from snapshot</li>
          <li><strong>Fast</strong> - No DOM re-query needed</li>
          <li><strong>AI-friendly</strong> - LLMs can reliably parse and use refs</li>
        </ul>

        <h2>CSS selectors</h2>
        <CodeBlock code={`agent-browser click "#id"
agent-browser click ".class"
agent-browser click "div > button"
agent-browser click "[data-testid='submit']"`} />

        <h2>Text & XPath</h2>
        <CodeBlock code={`agent-browser click "text=Submit"
agent-browser click "xpath=//button[@type='submit']"`} />

        <h2>Semantic locators</h2>
        <p>Find elements by role, label, or other semantic properties:</p>
        <CodeBlock code={`agent-browser find role button click --name "Submit"
agent-browser find label "Email" fill "test@test.com"
agent-browser find placeholder "Search..." fill "query"
agent-browser find testid "submit-btn" click`} />
      </div>
    </div>
  );
}
