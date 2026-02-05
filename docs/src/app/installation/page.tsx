import { CodeBlock } from "@/components/code-block";

export default function Installation() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Installation</h1>

        <h2>npm (recommended)</h2>
        <CodeBlock code={`npm install -g agent-browser
agent-browser install  # Download Chromium`} />

        <h2>From source</h2>
        <CodeBlock code={`git clone https://github.com/vercel-labs/agent-browser
cd agent-browser
pnpm install
pnpm build
pnpm build:native
./bin/agent-browser install
pnpm link --global`} />

        <h2>Linux dependencies</h2>
        <p>On Linux, install system dependencies:</p>
        <CodeBlock code={`agent-browser install --with-deps
# or manually: npx playwright install-deps chromium`} />

        <h2>Custom browser</h2>
        <p>
          Use a custom browser executable instead of bundled Chromium:
        </p>
        <ul>
          <li><strong>Serverless</strong> - Use <code>@sparticuz/chromium</code> (~50MB vs ~684MB)</li>
          <li><strong>System browser</strong> - Use existing Chrome installation</li>
          <li><strong>Custom builds</strong> - Use modified browser builds</li>
        </ul>

        <CodeBlock code={`# Via flag
agent-browser --executable-path /path/to/chromium open example.com

# Via environment variable
AGENT_BROWSER_EXECUTABLE_PATH=/path/to/chromium agent-browser open example.com`} />

        <h3>Serverless example</h3>
        <CodeBlock lang="typescript" code={`import chromium from '@sparticuz/chromium';
import { BrowserManager } from 'agent-browser';

export async function handler() {
  const browser = new BrowserManager();
  await browser.launch({
    executablePath: await chromium.executablePath(),
    headless: true,
  });
  // ... use browser
}`} />

        <h2>AI agent setup</h2>
        <p>agent-browser works with any AI agent out of the box. For richer context:</p>

        <h3>AGENTS.md / CLAUDE.md</h3>
        <p>Add to your instructions file:</p>
        <CodeBlock lang="markdown" code={`## Browser Automation

Use \`agent-browser\` for web automation. Run \`agent-browser --help\` for all commands.

Core workflow:
1. \`agent-browser open <url>\` - Navigate to page
2. \`agent-browser snapshot -i\` - Get interactive elements with refs (@e1, @e2)
3. \`agent-browser click @e1\` / \`fill @e2 "text"\` - Interact using refs
4. Re-snapshot after page changes`} />

        <h3>Claude Code skill</h3>
        <CodeBlock code="cp -r node_modules/agent-browser/skills/agent-browser .claude/skills/" />
        <p>Or download:</p>
        <CodeBlock code={`mkdir -p .claude/skills/agent-browser
curl -o .claude/skills/agent-browser/SKILL.md \\
  https://raw.githubusercontent.com/vercel-labs/agent-browser/main/skills/agent-browser/SKILL.md`} />
      </div>
    </div>
  );
}
