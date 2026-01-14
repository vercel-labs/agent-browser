import { CodeBlock } from "@/components/code-block";

export default function Commands() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Commands</h1>

        <h2>Core</h2>
        <CodeBlock code={`agent-browser open <url>              # Navigate (aliases: goto, navigate)
agent-browser click <sel>             # Click element
agent-browser dblclick <sel>          # Double-click
agent-browser fill <sel> <text>       # Clear and fill
agent-browser type <sel> <text>       # Type into element
agent-browser press <key>             # Press key (Enter, Tab, Control+a)
agent-browser hover <sel>             # Hover element
agent-browser select <sel> <val>      # Select dropdown option
agent-browser check <sel>             # Check checkbox
agent-browser uncheck <sel>           # Uncheck checkbox
agent-browser scroll <dir> [px]       # Scroll (up/down/left/right)
agent-browser screenshot [path]       # Screenshot (--full for full page)
agent-browser snapshot                # Accessibility tree with refs
agent-browser eval <js>               # Run JavaScript
agent-browser close                   # Close browser`} />

        <h2>Get info</h2>
        <CodeBlock code={`agent-browser get text <sel>          # Get text content
agent-browser get html <sel>          # Get innerHTML
agent-browser get value <sel>         # Get input value
agent-browser get attr <sel> <attr>   # Get attribute
agent-browser get title               # Get page title
agent-browser get url                 # Get current URL
agent-browser get count <sel>         # Count matching elements
agent-browser get box <sel>           # Get bounding box`} />

        <h2>Check state</h2>
        <CodeBlock code={`agent-browser is visible <sel>        # Check if visible
agent-browser is enabled <sel>        # Check if enabled
agent-browser is checked <sel>        # Check if checked`} />

        <h2>Find elements</h2>
        <p>Semantic locators with actions (<code>click</code>, <code>fill</code>, <code>check</code>, <code>hover</code>, <code>text</code>):</p>
        <CodeBlock code={`agent-browser find role <role> <action> [value]
agent-browser find text <text> <action>
agent-browser find label <label> <action> [value]
agent-browser find placeholder <ph> <action> [value]
agent-browser find testid <id> <action> [value]
agent-browser find first <sel> <action> [value]
agent-browser find nth <n> <sel> <action> [value]`} />
        <p>Examples:</p>
        <CodeBlock code={`agent-browser find role button click --name "Submit"
agent-browser find label "Email" fill "test@test.com"
agent-browser find first ".item" click`} />

        <h2>Wait</h2>
        <CodeBlock code={`agent-browser wait <selector>         # Wait for element
agent-browser wait <ms>               # Wait for time
agent-browser wait --text "Welcome"   # Wait for text
agent-browser wait --url "**/dash"    # Wait for URL pattern
agent-browser wait --load networkidle # Wait for load state
agent-browser wait --fn "condition"   # Wait for JS condition`} />

        <h2>Mouse</h2>
        <CodeBlock code={`agent-browser mouse move <x> <y>      # Move mouse
agent-browser mouse down [button]     # Press button
agent-browser mouse up [button]       # Release button
agent-browser mouse wheel <dy> [dx]   # Scroll wheel`} />

        <h2>Settings</h2>
        <CodeBlock code={`agent-browser set viewport <w> <h>    # Set viewport size
agent-browser set device <name>       # Emulate device ("iPhone 14")
agent-browser set geo <lat> <lng>     # Set geolocation
agent-browser set offline [on|off]    # Toggle offline mode
agent-browser set headers <json>      # Extra HTTP headers
agent-browser set credentials <u> <p> # HTTP basic auth
agent-browser set media [dark|light]  # Emulate color scheme`} />

        <h2>Cookies & storage</h2>
        <CodeBlock code={`agent-browser cookies                 # Get all cookies
agent-browser cookies set <name> <val> # Set cookie
agent-browser cookies clear           # Clear cookies

agent-browser storage local           # Get all localStorage
agent-browser storage local <key>     # Get specific key
agent-browser storage local set <k> <v>  # Set value
agent-browser storage local clear     # Clear all

agent-browser storage session         # Same for sessionStorage`} />

        <h2>Network</h2>
        <CodeBlock code={`agent-browser network route <url>              # Intercept requests
agent-browser network route <url> --abort      # Block requests
agent-browser network route <url> --body <json>  # Mock response
agent-browser network unroute [url]            # Remove routes
agent-browser network requests                 # View tracked requests`} />

        <h2>Tabs & frames</h2>
        <CodeBlock code={`agent-browser tab                     # List tabs
agent-browser tab new [url]           # New tab
agent-browser tab <n>                 # Switch to tab
agent-browser tab close [n]           # Close tab
agent-browser frame <sel>             # Switch to iframe
agent-browser frame main              # Back to main frame`} />

        <h2>Debug</h2>
        <CodeBlock code={`agent-browser trace start [path]      # Start trace
agent-browser trace stop [path]       # Stop and save trace
agent-browser console                 # View console messages
agent-browser errors                  # View page errors
agent-browser highlight <sel>         # Highlight element
agent-browser state save <path>       # Save auth state
agent-browser state load <path>       # Load auth state`} />

        <h2>Navigation</h2>
        <CodeBlock code={`agent-browser back                    # Go back
agent-browser forward                 # Go forward
agent-browser reload                  # Reload page`} />
      </div>
    </div>
  );
}
