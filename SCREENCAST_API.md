# Screencast & Input Injection API

Real-time browser streaming and remote input control for collaborative automation, pair programming, and monitoring.

## Overview

The screencast API provides:
- **Live video stream** of browser viewport
- **Input injection** for remote control (mouse, keyboard, touch)
- **Real-time monitoring** for AI agents and humans
- **Collaborative browsing** between agents
- **Session recording** for debugging

## Screencast Endpoints

### Start Screencast
```bash
POST /screencast/start
Content-Type: application/json

{
  "format": "jpeg",      # jpeg or png
  "quality": 80,         # 0-100
  "maxWidth": 1280,      # max width in pixels
  "maxHeight": 720,      # max height in pixels
  "everyNthFrame": 1     # skip frames (1 = every frame)
}
```

**Presets:**
```bash
# High quality (1920x1080 PNG)
POST /screencast/start?preset=hd

# Balanced (1280x720 JPEG, default)
POST /screencast/start?preset=balanced

# Low bandwidth (640x480 JPEG)
POST /screencast/start?preset=low

# Mobile (375x667 JPEG)
POST /screencast/start?preset=mobile
```

### Stop Screencast
```bash
GET /screencast/stop
```

### Get Screencast Status
```bash
GET /screencast/status
```

Response:
```json
{
  "screencasting": true,
  "connected": true,
  "clientCount": 2,
  "format": "jpeg",
  "quality": 80,
  "maxWidth": 1280,
  "maxHeight": 720
}
```

## Input Injection Endpoints

### Mouse Events
```bash
POST /input/mouse
Content-Type: application/json

{
  "type": "mousePressed",      # mousePressed, mouseReleased, mouseMoved, mouseWheel
  "x": 100,                     # X coordinate
  "y": 200,                     # Y coordinate
  "button": "left",             # left, right, middle, none
  "clickCount": 1,              # for multi-click
  "deltaX": 0,                  # for mouse wheel
  "deltaY": 10,                 # for mouse wheel
  "modifiers": 0                # Shift, Alt, Ctrl, Meta flags
}
```

**Examples:**

Click at coordinates:
```bash
curl -X POST http://localhost:8787/input/mouse \
  -H "Content-Type: application/json" \
  -d '{
    "type": "mousePressed",
    "x": 100,
    "y": 200,
    "button": "left"
  }'
```

Double-click:
```bash
# First click
curl -X POST http://localhost:8787/input/mouse \
  -d '{"type": "mousePressed", "x": 100, "y": 200}'

curl -X POST http://localhost:8787/input/mouse \
  -d '{"type": "mouseReleased", "x": 100, "y": 200}'

# Second click
curl -X POST http://localhost:8787/input/mouse \
  -d '{"type": "mousePressed", "x": 100, "y": 200}'

curl -X POST http://localhost:8787/input/mouse \
  -d '{"type": "mouseReleased", "x": 100, "y": 200}'
```

Drag operation:
```bash
# Press mouse
curl -X POST http://localhost:8787/input/mouse \
  -d '{"type": "mousePressed", "x": 100, "y": 100}'

# Move to target
curl -X POST http://localhost:8787/input/mouse \
  -d '{"type": "mouseMoved", "x": 200, "y": 200}'

# Release mouse
curl -X POST http://localhost:8787/input/mouse \
  -d '{"type": "mouseReleased", "x": 200, "y": 200}'
```

### Keyboard Events
```bash
POST /input/keyboard
Content-Type: application/json

{
  "type": "keyDown",     # keyDown, keyUp, char
  "key": "Enter",        # key identifier
  "code": "Enter",       # key code
  "text": "a",           # for char type
  "modifiers": 0         # Shift, Alt, Ctrl, Meta flags
}
```

**Examples:**

Type text:
```bash
# Type "Hello"
for char in H e l l o; do
  curl -X POST http://localhost:8787/input/keyboard \
    -d "{\"type\": \"char\", \"text\": \"$char\"}"
done
```

Press Enter:
```bash
curl -X POST http://localhost:8787/input/keyboard \
  -d '{"type": "keyDown", "key": "Enter", "code": "Enter"}'

curl -X POST http://localhost:8787/input/keyboard \
  -d '{"type": "keyUp", "key": "Enter", "code": "Enter"}'
```

Press Ctrl+A (select all):
```bash
# Modifiers: Shift=1, Ctrl=2, Alt=4, Meta=8
curl -X POST http://localhost:8787/input/keyboard \
  -d '{"type": "keyDown", "key": "a", "modifiers": 2}'

curl -X POST http://localhost:8787/input/keyboard \
  -d '{"type": "keyUp", "key": "a", "modifiers": 2}'
```

### Touch Events
```bash
POST /input/touch
Content-Type: application/json

{
  "type": "touchStart",        # touchStart, touchEnd, touchMove, touchCancel
  "touchPoints": [
    {"x": 100, "y": 200, "id": 1}
  ],
  "modifiers": 0
}
```

**Examples:**

Tap at coordinates:
```bash
curl -X POST http://localhost:8787/input/touch \
  -d '{
    "type": "touchStart",
    "touchPoints": [{"x": 100, "y": 200}]
  }'

curl -X POST http://localhost:8787/input/touch \
  -d '{
    "type": "touchEnd",
    "touchPoints": [{"x": 100, "y": 200}]
  }'
```

Multi-touch swipe:
```bash
curl -X POST http://localhost:8787/input/touch \
  -d '{
    "type": "touchStart",
    "touchPoints": [
      {"x": 100, "y": 200, "id": 1},
      {"x": 150, "y": 250, "id": 2}
    ]
  }'

curl -X POST http://localhost:8787/input/touch \
  -d '{
    "type": "touchMove",
    "touchPoints": [
      {"x": 150, "y": 250, "id": 1},
      {"x": 200, "y": 300, "id": 2}
    ]
  }'

curl -X POST http://localhost:8787/input/touch \
  -d '{
    "type": "touchEnd",
    "touchPoints": [
      {"x": 150, "y": 250, "id": 1},
      {"x": 200, "y": 300, "id": 2}
    ]
  }'
```

## WebSocket Streaming

Connect to WebSocket for real-time frame streaming:

```bash
wscat -c ws://localhost:8787/stream?session=default
```

### WebSocket Messages

**Frame Message** (from server):
```json
{
  "type": "frame",
  "data": "base64-encoded-image",
  "metadata": {
    "offsetTop": 0,
    "pageScaleFactor": 1,
    "deviceWidth": 1280,
    "deviceHeight": 720,
    "scrollOffsetX": 0,
    "scrollOffsetY": 0,
    "timestamp": 1705764000000
  }
}
```

**Status Message** (from server):
```json
{
  "type": "status",
  "connected": true,
  "screencasting": true,
  "viewportWidth": 1280,
  "viewportHeight": 720
}
```

**Input Message** (to server):
```json
{
  "type": "input_mouse",
  "eventType": "mousePressed",
  "x": 100,
  "y": 200,
  "button": "left"
}
```

**Error Message**:
```json
{
  "type": "error",
  "message": "Browser not launched"
}
```

## Use Cases

### 1. Collaborative Pair Programming

Agent 1 streams:
```bash
# Agent 1: Start streaming
POST /screencast/start?preset=hd&session=pair-session

# Agent 2: Connect via WebSocket
wscat -c ws://localhost:8787/stream?session=pair-session
```

Both agents can control:
```bash
# Agent 1 clicks
POST /input/mouse with session=pair-session

# Agent 2 types
POST /input/keyboard with session=pair-session
```

### 2. AI Agent Monitoring

Humans monitor AI agent automation:
```bash
# Human: Watch AI agent
wscat -c ws://localhost:8787/stream?session=ai-agent-123

# AI Agent: Automates while human watches
POST /browser/click?session=ai-agent-123
```

### 3. Remote Browser Control

Control browser from another location:
```bash
# Local: Start automation server
npm run worker:dev

# Remote: Control via HTTP
curl -X POST http://server:8787/input/mouse \
  -d '{"type": "mousePressed", "x": 100, "y": 200}'

# Local: Watch via WebSocket
wscat -c ws://localhost:8787/stream
```

### 4. Recording & Playback

Record session:
```bash
# Start recording
screencastStream.on('frame', (frame) => {
  saveFrame(frame); // Save frames
});

# Later: Playback
frames.forEach((frame, i) => {
  setTimeout(() => {
    sendToUI(frame);
  }, i * 33); // 30fps
});
```

### 5. Session Isolation

Each session gets its own screencast:
```bash
# Session 1
POST /screencast/start?session=user1

# Session 2 (separate)
POST /screencast/start?session=user2

# WebSocket for each
ws://localhost:8787/stream?session=user1
ws://localhost:8787/stream?session=user2
```

## Performance Tuning

### High Bandwidth (Local Network)
```bash
POST /screencast/start?preset=hd
# 1920x1080 PNG, 95% quality, every frame
```

### Limited Bandwidth (Internet)
```bash
POST /screencast/start?preset=low
# 640x480 JPEG, 60% quality, skip frames
```

### Mobile Device
```bash
POST /screencast/start?preset=mobile
# 375x667 JPEG, 75% quality
```

### Custom
```bash
curl -X POST http://localhost:8787/screencast/start \
  -d '{
    "format": "jpeg",
    "quality": 70,
    "maxWidth": 800,
    "maxHeight": 600,
    "everyNthFrame": 2
  }'
```

## Keyboard Modifiers

Bitwise flags for modifier keys:
- `0` - None
- `1` - Shift
- `2` - Ctrl/Cmd
- `4` - Alt
- `8` - Meta

Combined modifiers:
```bash
# Ctrl+Shift
modifiers: 3  # 2 | 1

# Alt+Shift
modifiers: 5  # 4 | 1

# Ctrl+Alt+Shift
modifiers: 7  # 2 | 4 | 1
```

## Frame Format

### JPEG
- Smaller file size
- Good for bandwidth-constrained
- Quality configurable 0-100
- Lower compression at higher quality

### PNG
- Lossless compression
- Larger file size
- Better for detail preservation
- Quality parameter ignored

## Best Practices

1. **Choose right preset**
   - Local: `hd`
   - Internet: `balanced` or `low`
   - Mobile: `mobile`

2. **Frame rate optimization**
   - `everyNthFrame: 1` - Real-time (full frames)
   - `everyNthFrame: 2` - 15 FPS (skip every other)
   - `everyNthFrame: 3` - 10 FPS (skip 2 of 3)

3. **Input timing**
   - Don't send inputs faster than frames arrive
   - Add 50-100ms delay between inputs
   - Wait for element visibility before clicking

4. **Session cleanup**
   - Stop screencast when done: `GET /screencast/stop`
   - Close WebSocket connections
   - Clean up temporary frames

5. **Error handling**
   - Reconnect on WebSocket disconnect
   - Retry input injection on failure
   - Log frame timestamps for debugging

## Examples

### Complete Collaborative Session

```python
import asyncio
import websockets
import json
import requests

async def monitor_and_control():
    # Connect to stream
    async with websockets.connect('ws://localhost:8787/stream?session=demo') as ws:
        # Listen for frames
        frame_count = 0

        async def listen():
            nonlocal frame_count
            async for message in ws:
                data = json.loads(message)
                if data['type'] == 'frame':
                    frame_count += 1
                    # Save frame or display
                    print(f"Frame {frame_count}: {data['metadata']}")

        # Send inputs while listening
        def send_input():
            # Click at (100, 200)
            requests.post(
                'http://localhost:8787/input/mouse',
                json={
                    'type': 'mousePressed',
                    'x': 100,
                    'y': 200,
                    'button': 'left'
                },
                params={'session': 'demo'}
            )

        # Monitor and control concurrently
        listen_task = asyncio.create_task(listen())

        await asyncio.sleep(1)
        send_input()

        await asyncio.sleep(2)
        await ws.close()

asyncio.run(monitor_and_control())
```

### JavaScript Client

```javascript
// Start screencast
await fetch('/screencast/start?preset=balanced', { method: 'POST' });

// Connect to WebSocket
const ws = new WebSocket('ws://localhost:8787/stream');

ws.onmessage = (event) => {
  const message = JSON.parse(event.data);

  if (message.type === 'frame') {
    // Display frame
    const img = new Image();
    img.src = `data:image/jpeg;base64,${message.data}`;
    document.body.appendChild(img);
  }
};

// Send mouse input
function click(x, y) {
  fetch('/input/mouse', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      type: 'mousePressed',
      x, y,
      button: 'left'
    })
  });
}

// Usage
click(100, 200);
```

## See Also

- [BROWSER_API.md](./BROWSER_API.md) - Full browser control API
- [SKILLS.md](./SKILLS.md) - Skills and plugins
- [stream-server.ts](./src/stream-server.ts) - WebSocket implementation
