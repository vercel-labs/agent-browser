import { CodeBlock } from "@/components/code-block";

export default function iOS() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>iOS Simulator</h1>
        <p>
          Control real Mobile Safari in the iOS Simulator for authentic mobile
          web testing. Uses Appium with XCUITest for native automation.
        </p>

        <h2>Requirements</h2>
        <ul>
          <li>macOS with Xcode installed</li>
          <li>iOS Simulator runtimes (download via Xcode)</li>
          <li>Appium with XCUITest driver</li>
        </ul>

        <h2>Setup</h2>
        <CodeBlock
          code={`# Install Appium globally
npm install -g appium

# Install the XCUITest driver for iOS
appium driver install xcuitest`}
        />

        <h2>List available devices</h2>
        <p>See all iOS simulators available on your system:</p>
        <CodeBlock
          code={`agent-browser device list

# Output:
# Available iOS Simulators:
#
#   ○ iPhone 16 Pro (iOS 18.0)
#     F21EEC0D-7618-419F-811B-33AF27A8B2FD
#   ○ iPhone 16 Pro Max (iOS 18.0)
#     50402807-C9B8-4D37-9F13-2E00E782C744
#   ○ iPad Pro 13-inch (M4) (iOS 18.0)
#     3A6C6436-B909-4593-866D-91D1062BB070
#   ...`}
        />

        <h2>Basic usage</h2>
        <p>
          Use the <code>-p ios</code> flag to enable iOS mode. The workflow is
          identical to desktop:
        </p>
        <CodeBlock
          code={`# Launch Safari on iPhone 16 Pro
agent-browser -p ios --device "iPhone 16 Pro" open https://example.com

# Get snapshot with refs (same as desktop)
agent-browser -p ios snapshot -i

# Interact using refs
agent-browser -p ios tap @e1
agent-browser -p ios fill @e2 "text"

# Take screenshot
agent-browser -p ios screenshot mobile.png

# Close session (shuts down simulator)
agent-browser -p ios close`}
        />

        <h2>Mobile-specific commands</h2>
        <CodeBlock
          code={`# Swipe gestures
agent-browser -p ios swipe up
agent-browser -p ios swipe down
agent-browser -p ios swipe left
agent-browser -p ios swipe right

# Swipe with distance (pixels)
agent-browser -p ios swipe up 500

# Tap (alias for click, semantically clearer for touch)
agent-browser -p ios tap @e1`}
        />

        <h2>Environment variables</h2>
        <p>Configure iOS mode via environment variables:</p>
        <CodeBlock
          code={`export AGENT_BROWSER_PROVIDER=ios
export AGENT_BROWSER_IOS_DEVICE="iPhone 16 Pro"

# Now all commands use iOS
agent-browser open https://example.com
agent-browser snapshot -i
agent-browser tap @e1`}
        />

        <table>
          <thead>
            <tr>
              <th>Variable</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>
                <code>AGENT_BROWSER_PROVIDER</code>
              </td>
              <td>
                Set to <code>ios</code> to enable iOS mode
              </td>
            </tr>
            <tr>
              <td>
                <code>AGENT_BROWSER_IOS_DEVICE</code>
              </td>
              <td>Device name (e.g., &quot;iPhone 16 Pro&quot;)</td>
            </tr>
            <tr>
              <td>
                <code>AGENT_BROWSER_IOS_UDID</code>
              </td>
              <td>Device UDID (alternative to device name)</td>
            </tr>
          </tbody>
        </table>

        <h2>Supported devices</h2>
        <p>
          All iOS Simulators available in Xcode are supported, including:
        </p>
        <ul>
          <li>All iPhone models (iPhone 15, 16, 17, SE, etc.)</li>
          <li>All iPad models (iPad Pro, iPad Air, iPad mini, etc.)</li>
          <li>Multiple iOS versions (17.x, 18.x, etc.)</li>
        </ul>
        <p>
          <strong>Real devices</strong> are also supported via USB connection
          (see below).
        </p>

        <h2>Real device support</h2>
        <p>
          Appium can control Safari on real iOS devices connected via USB. This
          requires additional one-time setup.
        </p>

        <h3>1. Get your device UDID</h3>
        <CodeBlock
          code={`# List connected devices
xcrun xctrace list devices

# Or via system profiler
system_profiler SPUSBDataType | grep -A 5 "iPhone\\|iPad"`}
        />

        <h3>2. Sign WebDriverAgent (one-time)</h3>
        <p>
          WebDriverAgent needs to be signed with your Apple Developer
          certificate to run on real devices.
        </p>
        <CodeBlock
          code={`# Open the WebDriverAgent Xcode project
cd ~/.appium/node_modules/appium-xcuitest-driver/node_modules/appium-webdriveragent
open WebDriverAgent.xcodeproj`}
        />
        <p>In Xcode:</p>
        <ol>
          <li>
            Select the <code>WebDriverAgentRunner</code> target
          </li>
          <li>Go to Signing &amp; Capabilities</li>
          <li>
            Select your Team (requires Apple Developer account, free tier works)
          </li>
          <li>Let Xcode manage signing automatically</li>
        </ol>

        <h3>3. Use with agent-browser</h3>
        <CodeBlock
          code={`# Connect device via USB, then use the UDID
agent-browser -p ios --device "<DEVICE_UDID>" open https://example.com

# Or use the device name if unique
agent-browser -p ios --device "John's iPhone" open https://example.com`}
        />

        <h3>Real device notes</h3>
        <ul>
          <li>
            First run installs WebDriverAgent to the device (may require Trust
            prompt on device)
          </li>
          <li>Device must be unlocked and connected via USB</li>
          <li>Slightly slower initial connection than simulator</li>
          <li>Tests against real Safari performance and behavior</li>
          <li>
            On first install, go to Settings &rarr; General &rarr; VPN &amp;
            Device Management to trust the developer certificate
          </li>
        </ul>

        <h2>Performance notes</h2>
        <ul>
          <li>
            <strong>First launch:</strong> Takes 30-60 seconds to boot the
            simulator and start Appium
          </li>
          <li>
            <strong>Subsequent commands:</strong> Fast (simulator stays running)
          </li>
          <li>
            <strong>Close command:</strong> Shuts down simulator and Appium
            server
          </li>
        </ul>

        <h2>Differences from desktop</h2>
        <table>
          <thead>
            <tr>
              <th>Feature</th>
              <th>Desktop</th>
              <th>iOS</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Browser</td>
              <td>Chromium/Firefox/WebKit</td>
              <td>Safari only</td>
            </tr>
            <tr>
              <td>Tabs</td>
              <td>Supported</td>
              <td>Single tab only</td>
            </tr>
            <tr>
              <td>PDF export</td>
              <td>Supported</td>
              <td>Not supported</td>
            </tr>
            <tr>
              <td>Screencast</td>
              <td>Supported</td>
              <td>Not supported</td>
            </tr>
            <tr>
              <td>Swipe gestures</td>
              <td>Not native</td>
              <td>Native support</td>
            </tr>
          </tbody>
        </table>

        <h2>Troubleshooting</h2>
        <h3>Appium not found</h3>
        <CodeBlock
          code={`# Make sure Appium is installed globally
npm install -g appium
appium driver install xcuitest

# Verify installation
appium --version`}
        />

        <h3>No simulators available</h3>
        <p>
          Open Xcode and download iOS Simulator runtimes from{" "}
          <strong>Settings &rarr; Platforms</strong>.
        </p>

        <h3>Simulator won&apos;t boot</h3>
        <p>
          Try booting the simulator manually from Xcode or the Simulator app to
          ensure it works, then retry with agent-browser.
        </p>
      </div>
    </div>
  );
}
