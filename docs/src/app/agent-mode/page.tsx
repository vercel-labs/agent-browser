import { CodeBlock } from "@/components/code-block";

export default function AgentMode() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Agent Mode</h1>
        <p>
          agent-browser works with any AI coding agent. Use <code>--json</code> for machine-readable output.
        </p>

        <h2>Compatible agents</h2>
        <ul>
          <li>Claude Code</li>
          <li>Cursor</li>
          <li>GitHub Copilot</li>
          <li>OpenAI Codex</li>
          <li>Google Gemini</li>
          <li>opencode</li>
          <li>Any agent that can run shell commands</li>
        </ul>

        <h2>JSON output</h2>
        <CodeBlock code={`agent-browser snapshot --json
# {"success":true,"data":{"snapshot":"...","refs":{...}}}

agent-browser get text @e1 --json
agent-browser is visible @e2 --json`} />

        <h2>Optimal workflow</h2>
        <CodeBlock code={`# 1. Navigate and get snapshot
agent-browser open example.com
agent-browser snapshot -i --json   # AI parses tree and refs

# 2. AI identifies target refs from snapshot
# 3. Execute actions using refs
agent-browser click @e2
agent-browser fill @e3 "input text"

# 4. Get new snapshot if page changed
agent-browser snapshot -i --json`} />

        <h2>Integration</h2>

        <h3>Just ask</h3>
        <p>The simplest approach:</p>
        <CodeBlock lang="text" code="Use agent-browser to test the login flow. Run agent-browser --help to see available commands." />
        <p>The <code>--help</code> output is comprehensive.</p>

        <h3>AGENTS.md / CLAUDE.md</h3>
        <p>For consistent results, add to your instructions file:</p>
        <CodeBlock lang="markdown" code={`## Browser Automation

Use \`agent-browser\` for web automation. Run \`agent-browser --help\` for all commands.

Core workflow:
1. \`agent-browser open <url>\` - Navigate to page
2. \`agent-browser snapshot -i\` - Get interactive elements with refs (@e1, @e2)
3. \`agent-browser click @e1\` / \`fill @e2 "text"\` - Interact using refs
4. Re-snapshot after page changes`} />

        <h3>Claude Code skill</h3>
        <p>For richer context:</p>
        <CodeBlock code="cp -r node_modules/agent-browser/skills/agent-browser .claude/skills/" />
        <p>Or download:</p>
        <CodeBlock code={`mkdir -p .claude/skills/agent-browser
curl -o .claude/skills/agent-browser/SKILL.md \\
  https://raw.githubusercontent.com/vercel-labs/agent-browser/main/skills/agent-browser/SKILL.md`} />
      </div>
    </div>
  );
}
