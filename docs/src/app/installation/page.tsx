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

        <h2>Browser selection</h2>
        <p>
          agent-browser supports multiple browsers. By default, Chromium is used,
          but you can select Firefox or webkit for specific use cases or platforms:
        </p>

        <h3>Platform compatibility</h3>
        <table>
          <thead>
            <tr>
              <th>Platform</th>
              <th>Chromium</th>
              <th>Firefox</th>
              <th>webkit</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>x86_64 (Linux/Mac/Windows)</td>
              <td>✅</td>
              <td>✅</td>
              <td>✅</td>
            </tr>
            <tr>
              <td>ARM64 (Graviton, Cobalt, etc.)</td>
              <td>❓*</td>
              <td>✅</td>
              <td>✅</td>
            </tr>
          </tbody>
        </table>
        <p>
          <small>
            *Chromium may not be available on all ARM64 systems. Firefox is recommended
            for ARM64 deployments (AWS Graviton, Azure Cobalt, on-premises servers, etc.).
          </small>
        </p>

        <h3>Browser selection</h3>
        <p>
          Use the <code>browser</code> parameter when launching to select Firefox or webkit:
        </p>
        <CodeBlock lang="typescript" code={`import { BrowserManager } from 'agent-browser';

const browser = new BrowserManager();
await browser.launch({
  browser: 'firefox',  // or 'chromium', 'webkit'
  headless: true,
});`} />

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

        <h2>ARM64 troubleshooting</h2>
        <h3>Chromium not found on ARM64</h3>
        <p>
          On ARM64 systems (AWS Graviton, Azure Cobalt, etc.), agent-browser automatically selects Firefox as the default browser. If you encounter "Chromium not found" errors, ensure you're using a recent version where Firefox is the ARM64 default.
        </p>
        <p>
          Alternatively, explicitly specify Firefox when launching:
        </p>
        <CodeBlock lang="typescript" code={`import { BrowserManager } from 'agent-browser';

const browser = new BrowserManager();
await browser.launch({
  browser: 'firefox',
  headless: true,
});`} />
      </div>
    </div>
  );
}
