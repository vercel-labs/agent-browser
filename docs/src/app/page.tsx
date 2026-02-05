import { CodeBlock } from "@/components/code-block";

export default function Home() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>agent-browser</h1>
        <p>
          Browser automation CLI designed for AI agents. Compact text output minimizes context usage. Fast Rust CLI with Node.js fallback.
        </p>

        <CodeBlock code="npm install -g agent-browser" />

        <h2>Features</h2>
        <ul>
          <li><strong>Agent-first</strong> - Compact text output uses fewer tokens than JSON, designed for AI context efficiency</li>
          <li><strong>Ref-based</strong> - Snapshot returns accessibility tree with refs for deterministic element selection</li>
          <li><strong>Fast</strong> - Native Rust CLI for instant command parsing</li>
          <li><strong>Complete</strong> - 50+ commands for navigation, forms, screenshots, network, storage</li>
          <li><strong>Sessions</strong> - Multiple isolated browser instances with separate auth</li>
          <li><strong>Cross-platform</strong> - macOS, Linux, Windows with native binaries</li>
        </ul>

        <h2>Works with</h2>
        <p>
          Claude Code, Cursor, GitHub Copilot, OpenAI Codex, Google Gemini, opencode, and any agent that can run shell commands.
        </p>

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
          The <code>snapshot</code> command returns a compact accessibility tree where each element 
          has a unique ref like <code>@e1</code>, <code>@e2</code>. This provides:
        </p>
        <ul>
          <li><strong>Context-efficient</strong> - Text output uses ~200-400 tokens vs ~3000-5000 for full DOM</li>
          <li><strong>Deterministic</strong> - Ref points to exact element from snapshot</li>
          <li><strong>Fast</strong> - No DOM re-query needed</li>
          <li><strong>AI-friendly</strong> - LLMs parse text output naturally</li>
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
