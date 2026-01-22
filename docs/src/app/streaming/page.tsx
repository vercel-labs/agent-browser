import { CodeBlock } from "@/components/code-block";

export default function Streaming() {
  return (
    <div className="max-w-2xl mx-auto px-4 sm:px-6 py-8 sm:py-12">
      <div className="prose">
        <h1>Streaming</h1>
        <p>
          Stream the browser viewport via WebSocket for live preview or &quot;pair browsing&quot;
          where a human can watch and interact alongside an AI agent.
        </p>

        <h2>Enable streaming</h2>
        <p>
          Set the <code>AGENT_BROWSER_STREAM_PORT</code> environment variable to start
          a WebSocket server:
        </p>
        <CodeBlock code={`AGENT_BROWSER_STREAM_PORT=9223 agent-browser open example.com`} />

        <p>
          The server streams viewport frames and accepts input events (mouse, keyboard, touch).
        </p>

        <h2>WebSocket protocol</h2>
        <p>Connect to <code>ws://localhost:9223</code> to receive frames and send input.</p>

        <h3>Frame messages</h3>
        <p>The server sends frame messages with base64-encoded images:</p>
        <CodeBlock code={`{
  "type": "frame",
  "data": "<base64-encoded-jpeg>",
  "metadata": {
    "deviceWidth": 1280,
    "deviceHeight": 720,
    "pageScaleFactor": 1,
    "offsetTop": 0,
    "scrollOffsetX": 0,
    "scrollOffsetY": 0
  }
}`} />

        <h3>Status messages</h3>
        <p>Connection and screencast status:</p>
        <CodeBlock code={`{
  "type": "status",
  "connected": true,
  "screencasting": true,
  "viewportWidth": 1280,
  "viewportHeight": 720
}`} />

        <h2>Input injection</h2>
        <p>Send input events to control the browser remotely.</p>

        <h3>Mouse events</h3>
        <CodeBlock code={`// Click
{
  "type": "input_mouse",
  "eventType": "mousePressed",
  "x": 100,
  "y": 200,
  "button": "left",
  "clickCount": 1
}

// Release
{
  "type": "input_mouse",
  "eventType": "mouseReleased",
  "x": 100,
  "y": 200,
  "button": "left"
}

// Move
{
  "type": "input_mouse",
  "eventType": "mouseMoved",
  "x": 150,
  "y": 250
}

// Scroll
{
  "type": "input_mouse",
  "eventType": "mouseWheel",
  "x": 100,
  "y": 200,
  "deltaX": 0,
  "deltaY": 100
}`} />

        <h3>Keyboard events</h3>
        <CodeBlock code={`// Key down
{
  "type": "input_keyboard",
  "eventType": "keyDown",
  "key": "Enter",
  "code": "Enter"
}

// Key up
{
  "type": "input_keyboard",
  "eventType": "keyUp",
  "key": "Enter",
  "code": "Enter"
}

// Type character
{
  "type": "input_keyboard",
  "eventType": "char",
  "text": "a"
}

// With modifiers (1=Alt, 2=Ctrl, 4=Meta, 8=Shift)
{
  "type": "input_keyboard",
  "eventType": "keyDown",
  "key": "c",
  "code": "KeyC",
  "modifiers": 2
}`} />

        <h3>Touch events</h3>
        <CodeBlock code={`// Touch start
{
  "type": "input_touch",
  "eventType": "touchStart",
  "touchPoints": [{ "x": 100, "y": 200 }]
}

// Touch move
{
  "type": "input_touch",
  "eventType": "touchMove",
  "touchPoints": [{ "x": 150, "y": 250 }]
}

// Touch end
{
  "type": "input_touch",
  "eventType": "touchEnd",
  "touchPoints": []
}

// Multi-touch (pinch zoom)
{
  "type": "input_touch",
  "eventType": "touchStart",
  "touchPoints": [
    { "x": 100, "y": 200, "id": 0 },
    { "x": 200, "y": 200, "id": 1 }
  ]
}`} />

        <h2>Programmatic API</h2>
        <p>For advanced use, control streaming directly via the TypeScript API:</p>
        <CodeBlock code={`import { BrowserManager } from 'agent-browser';

const browser = new BrowserManager();
await browser.launch({ headless: true });
await browser.navigate('https://example.com');

// Start screencast with callback
await browser.startScreencast((frame) => {
  console.log('Frame:', frame.metadata.deviceWidth, 'x', frame.metadata.deviceHeight);
  // frame.data is base64-encoded image
}, {
  format: 'jpeg',  // or 'png'
  quality: 80,     // 0-100, jpeg only
  maxWidth: 1280,
  maxHeight: 720,
  everyNthFrame: 1
});

// Inject mouse event
await browser.injectMouseEvent({
  type: 'mousePressed',
  x: 100,
  y: 200,
  button: 'left',
  clickCount: 1
});

// Inject keyboard event
await browser.injectKeyboardEvent({
  type: 'keyDown',
  key: 'Enter',
  code: 'Enter'
});

// Inject touch event
await browser.injectTouchEvent({
  type: 'touchStart',
  touchPoints: [{ x: 100, y: 200 }]
});

// Check if screencasting
console.log('Active:', browser.isScreencasting());

// Stop screencast
await browser.stopScreencast();`} />

        <h2>Use cases</h2>
        <ul>
          <li><strong>Pair browsing</strong> - Human watches and assists AI agent in real-time</li>
          <li><strong>Remote preview</strong> - View browser output in a separate UI</li>
          <li><strong>Recording</strong> - Capture frames for video generation</li>
          <li><strong>Mobile testing</strong> - Inject touch events for mobile emulation</li>
          <li><strong>Accessibility testing</strong> - Manual interaction during automated tests</li>
        </ul>
      </div>
    </div>
  );
}
