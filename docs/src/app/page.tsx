import { CodeBlock } from "@/components/code-block";

export default function Home() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>agent-browser</h1>
        <p>
          Headless browser automation CLI for AI agents. Fast Rust CLI with Node.js fallback.
        </p>

        <CodeBlock code="npm install -g agent-browser" />

        <h2>Features</h2>
        <ul>
          <li><strong>Universal</strong> - Works with any AI agent: Claude Code, Cursor, Codex, Copilot, Gemini, opencode, and more</li>
          <li><strong>AI-first</strong> - Snapshot returns accessibility tree with refs for deterministic element selection</li>
          <li><strong>Fast</strong> - Native Rust CLI for instant command parsing</li>
          <li><strong>Complete</strong> - 50+ commands for navigation, forms, screenshots, network, storage</li>
          <li><strong>Sessions</strong> - Multiple isolated browser instances with separate auth</li>
          <li><strong>Cross-platform</strong> - macOS, Linux, Windows with native binaries</li>
          <li><strong>Serverless</strong> - Custom executable path for lightweight Chromium builds</li>
        </ul>

        <h2>Example</h2>
        <CodeBlock code={`# Navigate and get snapshot
agent-browser open example.com
agent-browser snapshot -i

# Output:
# - heading "Example Domain" [ref=e1]
# - link "More information..." [ref=e2]

# Interact using refs
agent-browser click @e2
agent-browser screenshot page.png
agent-browser close`} />

        <h2>Why refs?</h2>
        <p>
          The <code>snapshot</code> command returns an accessibility tree where each element 
          has a unique ref like <code>@e1</code>, <code>@e2</code>. This provides:
        </p>
        <ul>
          <li><strong>Deterministic</strong> - Ref points to exact element from snapshot</li>
          <li><strong>Fast</strong> - No DOM re-query needed</li>
          <li><strong>AI-friendly</strong> - LLMs can reliably parse and use refs</li>
        </ul>

        <h2>Architecture</h2>
        <p>
          Client-daemon architecture for optimal performance:
        </p>
        <ol>
          <li><strong>Rust CLI</strong> - Parses commands, communicates with daemon</li>
          <li><strong>Node.js Daemon</strong> - Manages Playwright browser instance</li>
        </ol>
        <p>
          Daemon starts automatically and persists between commands.
        </p>

        <h2>Platforms</h2>
        <p>
          Native Rust binaries for macOS (ARM64, x64), Linux (ARM64, x64), and Windows (x64).
        </p>
      </div>
    </div>
  );
}
