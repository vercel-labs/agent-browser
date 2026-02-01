import { CodeBlock } from "@/components/code-block";

export default function Changelog() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Changelog</h1>

        <h2 id="v0.8.0">v0.8.0</h2>
        <p className="text-[#888] text-sm">January 2026</p>

        <h3>New Features</h3>
        <ul>
          <li>
            <strong>Kernel cloud browser provider</strong> - Connect to Kernel (kernel.sh) for remote browser infrastructure with stealth mode and persistent profiles
            <CodeBlock code={`# Via -p flag
agent-browser -p kernel open https://example.com

# Via environment variable
export AGENT_BROWSER_PROVIDER=kernel
export KERNEL_API_KEY=your-api-key
agent-browser open https://example.com

# With persistent profile
export KERNEL_PROFILE_NAME=my-profile
agent-browser open https://example.com`} />
          </li>
          <li>
            <strong>Ignore HTTPS certificate errors</strong> - New flag for working with self-signed certificates and development environments
            <CodeBlock code={`agent-browser --ignore-https-errors open https://localhost:3000`} />
          </li>
          <li>
            <strong>Enhanced cookie management</strong> - Extended <code>cookies set</code> command with additional flags for setting cookies before page load
            <CodeBlock code={`agent-browser cookies set session_id "abc123" --url https://app.example.com --httpOnly --secure
agent-browser cookies set token "xyz" --domain .example.com --path /api --expires 1735689600`} />
          </li>
        </ul>

        <h3>Bug Fixes</h3>
        <ul>
          <li>Fixed tab list command not recognizing new pages opened via clicks or <code>target=&quot;_blank&quot;</code> links</li>
          <li>Fixed <code>check</code> command hanging indefinitely</li>
          <li>Fixed <code>set device</code> not applying deviceScaleFactor - HiDPI screenshots now work correctly</li>
          <li>Fixed state load and profile persistence not working in v0.7.6</li>
          <li>Screenshots now save to temp directory when no path is provided</li>
        </ul>

        <h3>Security</h3>
        <ul>
          <li>Daemon and stream server now reject cross-origin connections</li>
        </ul>

        <hr className="my-8 border-[#333]" />

        <h2 id="v0.7.1">v0.7.1</h2>
        <p className="text-[#888] text-sm">January 2026</p>

        <h3>Bug Fixes</h3>
        <ul>
          <li>
            <strong>Fix native binary distribution</strong> - Native binaries for all platforms (Linux x64/arm64, macOS x64/arm64, Windows x64) are now included in the npm package. Previously, the release workflow published to npm before building binaries, causing &quot;No binary found&quot; errors on installation.
          </li>
        </ul>

        <hr className="my-8 border-[#333]" />

        <h2 id="v0.7.0">v0.7.0</h2>
        <p className="text-[#888] text-sm">January 2026</p>

        <h3>New Features</h3>
        <ul>
          <li>
            <strong>Cloud browser providers</strong> - Connect to Browserbase or Browser Use for remote browser infrastructure
            <CodeBlock code={`# Via -p flag (recommended)
agent-browser -p browserbase open https://example.com
agent-browser -p browseruse open https://example.com

# Via environment variable
export AGENT_BROWSER_PROVIDER=browserbase
agent-browser open https://example.com`} />
          </li>
          <li>
            <strong>Persistent browser profiles</strong> - Store cookies, localStorage, and login sessions across browser restarts
            <CodeBlock code={`agent-browser --profile ~/.myapp-profile open myapp.com
# Login persists across restarts`} />
          </li>
          <li>
            <strong>Remote CDP WebSocket URLs</strong> - Connect to remote browser services via WebSocket
            <CodeBlock code={`agent-browser --cdp "wss://browser-service.com/cdp?token=..." snapshot`} />
          </li>
          <li>
            <strong><code>download</code> command</strong> - Trigger downloads and wait for completion
            <CodeBlock code={`agent-browser download @e1 ./file.pdf
agent-browser wait --download ./output.zip --timeout 30000`} />
          </li>
          <li>
            <strong>Browser launch configuration</strong> - Fine-grained control over browser startup
            <CodeBlock code={`agent-browser --args "--disable-gpu,--no-sandbox" open example.com
agent-browser --user-agent "Custom UA" open example.com
agent-browser --proxy-bypass "localhost,*.internal" open example.com`} />
          </li>
          <li>
            <strong>Enhanced skills</strong> - Hierarchical structure with references and templates for Claude Code
          </li>
        </ul>

        <h3>Bug Fixes</h3>
        <ul>
          <li>Screenshot command now supports refs and has improved error messages</li>
          <li>WebSocket URLs work in <code>connect</code> command</li>
          <li>Fixed socket file location (uses <code>~/.agent-browser</code> instead of TMPDIR)</li>
          <li>Windows binary path fix (.exe extension)</li>
          <li>State load and path-based actions now show correct output messages</li>
        </ul>

        <h3>Documentation</h3>
        <ul>
          <li>Added Claude Code marketplace plugin installation instructions</li>
          <li>Updated skill documentation with references and templates</li>
          <li>Improved error documentation</li>
        </ul>

        <hr className="my-8 border-[#333]" />

        <h2 id="v0.6.0">v0.6.0</h2>
        <p className="text-[#888] text-sm">January 2026</p>

        <h3>New Features</h3>
        <ul>
          <li>
            <strong>Video recording</strong> - Record browser sessions to WebM using Playwright&apos;s native recording
            <CodeBlock code={`agent-browser record start ./demo.webm
agent-browser click @e1
agent-browser record stop`} />
          </li>
          <li>
            <strong><code>connect</code> command</strong> - Connect to a browser via CDP and persist the connection for subsequent commands
            <CodeBlock code={`agent-browser connect 9222
agent-browser snapshot  # No --cdp needed after connect`} />
          </li>
          <li>
            <strong><code>--proxy</code> flag</strong> - Configure browser proxy with optional authentication
            <CodeBlock code="agent-browser --proxy http://user:pass@proxy.com:8080 open example.com" />
          </li>
          <li>
            <strong><code>get styles</code> command</strong> - Extract computed styles from elements
            <CodeBlock code={`agent-browser get styles "button"`} />
          </li>
          <li>
            <strong>Claude marketplace plugin</strong> - Added <code>.claude-plugin/marketplace.json</code> for Claude Code integration
          </li>
          <li>
            <strong>Enhanced network output</strong> - <code>network requests</code> now shows method, URL, and resource type
          </li>
          <li>
            <strong><code>--version</code> flag</strong> - Display CLI version
          </li>
        </ul>

        <h3>Bug Fixes</h3>
        <ul>
          <li>Fix Windows daemon startup and port calculation</li>
          <li>Support <code>libasound2t64</code> on newer Ubuntu versions (24.04+)</li>
          <li>Prevent CDP timeout on empty URL tabs</li>
          <li>Output screenshot as base64 when no path provided</li>
          <li>Resolve refs in <code>get value</code> command</li>
          <li>Support URL parameter in <code>tab new</code> command</li>
          <li>Allow <code>about:</code>, <code>data:</code>, and <code>file:</code> URL schemes</li>
          <li>Detect stale unix socket by attempting connection</li>
          <li>Respect <code>AGENT_BROWSER_HEADED</code> environment variable</li>
          <li>Handle SIGPIPE to prevent panic when piping to <code>head</code>/<code>tail</code></li>
          <li>Fix null path validation in screenshot command</li>
        </ul>

        <h3>Protocol Alignment</h3>
        <p>These changes align the CLI with the daemon protocol for consistency:</p>
        <ul>
          <li><code>select</code> command now uses <code>values</code> field (supports multiple selections)</li>
          <li><code>frame main</code> uses <code>mainframe</code> action</li>
          <li><code>mouse wheel</code> uses <code>wheel</code> action</li>
          <li><code>set media</code> uses <code>emulatemedia</code> action</li>
          <li>Console output uses <code>messages</code> field</li>
        </ul>

        <h3>Documentation</h3>
        <ul>
          <li>Expanded SKILL.md with comprehensive command reference</li>
          <li>Updated README with new commands and options</li>
          <li>Updated CDP mode documentation with <code>connect</code> workflow</li>
        </ul>

      </div>
    </div>
  );
}
