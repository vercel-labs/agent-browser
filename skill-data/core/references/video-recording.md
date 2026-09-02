# Video Recording

Capture browser automation as video for debugging, documentation, or verification.

**Related**: [commands.md](commands.md) for full command reference, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Basic Recording](#basic-recording)
- [Recording Commands](#recording-commands)
- [Frame Rate](#frame-rate)
- [Use Cases](#use-cases)
- [Best Practices](#best-practices)
- [Output Format](#output-format)
- [Limitations](#limitations)

## Basic Recording

```bash
# Launch the browser, then start recording
agent-browser open https://example.com
agent-browser record start ./demo.webm

# Perform actions
agent-browser snapshot -i
agent-browser click @e1
agent-browser fill @e2 "test input"

# Stop and save
agent-browser record stop
```

## Recording Commands

```bash
# Launch a session first
agent-browser open

# Start recording to file (30 fps)
agent-browser record start ./output.webm

# Start recording at a specific rate (1-60)
agent-browser record start ./output.webm --fps 60

# Stop current recording
agent-browser record stop

# Restart with new file (stops current + starts new)
agent-browser record restart ./take2.webm --fps 60
```

## Frame Rate

Recording captures 30 fps by default, so scrolling, hover states, and CSS transitions read as motion instead of a slideshow. `--fps` takes any rate from 1 to 60.

| Rate | Use it for |
| --- | --- |
| 60 | Short, motion-heavy takes: drag interactions, animation, scroll polish work |
| 30 (default) | Flows, CI evidence, walkthroughs |
| 1-15 | Long sessions where the video is a timeline, not a motion study |

```bash
# Animation review
agent-browser record start ./transition.webm --fps 60
agent-browser click @e1
agent-browser wait 1500
agent-browser record stop

# Hour-long soak run
agent-browser record start ./soak.webm --fps 5
```

The saved file carries the requested rate, and its duration matches the wall clock time it recorded: a capture that overruns its frame budget holds the current frame rather than dropping it, so playback never runs fast. On a heavy page the number of distinct frames can still fall short of the requested rate. 60 fps roughly doubles the bitrate of 30 fps.

## Use Cases

### Debugging Failed Automation

```bash
#!/bin/bash
# Record automation for debugging

# Run your automation
agent-browser open https://app.example.com
agent-browser record start ./debug-$(date +%Y%m%d-%H%M%S).webm
agent-browser snapshot -i
agent-browser click @e1 || {
    echo "Click failed - check recording"
    agent-browser record stop
    exit 1
}

agent-browser record stop
```

### Documentation Generation

```bash
#!/bin/bash
# Record workflow for documentation

agent-browser open https://app.example.com/login
agent-browser record start ./docs/how-to-login.webm
agent-browser wait 1000  # Pause for visibility

agent-browser snapshot -i
agent-browser fill @e1 "demo@example.com"
agent-browser wait 500

agent-browser fill @e2 "password"
agent-browser wait 500

agent-browser click @e3
agent-browser wait --load networkidle
agent-browser wait 1000  # Show result

agent-browser record stop
```

### CI/CD Test Evidence

```bash
#!/bin/bash
# Record E2E test runs for CI artifacts

TEST_NAME="${1:-e2e-test}"
RECORDING_DIR="./test-recordings"
mkdir -p "$RECORDING_DIR"

agent-browser open
agent-browser record start "$RECORDING_DIR/$TEST_NAME-$(date +%s).webm"

# Run test
if run_e2e_test; then
    echo "Test passed"
else
    echo "Test failed - recording saved"
fi

agent-browser record stop
```

## Best Practices

### 1. Add Pauses for Clarity

```bash
# Slow down for human viewing
agent-browser click @e1
agent-browser wait 500  # Let viewer see result
```

### 2. Use Descriptive Filenames

```bash
# Include context in filename
agent-browser record start ./recordings/login-flow-2024-01-15.webm
agent-browser record start ./recordings/checkout-test-run-42.webm
```

### 3. Handle Recording in Error Cases

```bash
#!/bin/bash
set -e

cleanup() {
    agent-browser record stop 2>/dev/null || true
    agent-browser close 2>/dev/null || true
}
trap cleanup EXIT

agent-browser open
agent-browser record start ./automation.webm
# ... automation steps ...
```

### 4. Combine with Screenshots

```bash
# Record video AND capture key frames
agent-browser open https://example.com
agent-browser record start ./flow.webm
agent-browser screenshot ./screenshots/step1-homepage.png

agent-browser click @e1
agent-browser screenshot ./screenshots/step2-after-click.png

agent-browser record stop
```

## Output Format

- Default format: WebM (VP8/VP9 codec)
- Default frame rate: 30 fps (`--fps` accepts 1 to 60)
- Compatible with all modern browsers and video players
- Compressed but high quality

## Limitations

- Recording adds slight overhead to automation, and higher frame rates add more
- Large recordings can consume significant disk space; 60 fps roughly doubles the bitrate of 30 fps
- Distinct frames per second are bounded by how fast the browser produces screenshots, so a heavy page may repeat frames at 60 fps
- Some headless environments may have codec limitations
