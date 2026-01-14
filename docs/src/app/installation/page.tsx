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
      </div>
    </div>
  );
}
