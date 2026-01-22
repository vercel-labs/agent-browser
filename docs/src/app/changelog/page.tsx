import { CodeBlock } from "@/components/code-block";

export default function Changelog() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Changelog</h1>

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
