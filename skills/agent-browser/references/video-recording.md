# Video Recording

Capture browser automation sessions as video for debugging, documentation, or verification.

## Basic Recording

```bash
# Start recording
npx agent-browser record start ./demo.webm

# Perform actions
npx agent-browser open https://example.com
npx agent-browser snapshot -i
npx agent-browser click @e1
npx agent-browser fill @e2 "test input"

# Stop and save
npx agent-browser record stop
```

## Recording Commands

```bash
# Start recording to file
npx agent-browser record start ./output.webm

# Stop current recording
npx agent-browser record stop

# Restart with new file (stops current + starts new)
npx agent-browser record restart ./take2.webm
```

## Use Cases

### Debugging Failed Automation

```bash
#!/bin/bash
# Record automation for debugging

npx agent-browser record start ./debug-$(date +%Y%m%d-%H%M%S).webm

# Run your automation
npx agent-browser open https://app.example.com
npx agent-browser snapshot -i
npx agent-browser click @e1 || {
    echo "Click failed - check recording"
    npx agent-browser record stop
    exit 1
}

npx agent-browser record stop
```

### Documentation Generation

```bash
#!/bin/bash
# Record workflow for documentation

npx agent-browser record start ./docs/how-to-login.webm

npx agent-browser open https://app.example.com/login
npx agent-browser wait 1000  # Pause for visibility

npx agent-browser snapshot -i
npx agent-browser fill @e1 "demo@example.com"
npx agent-browser wait 500

npx agent-browser fill @e2 "password"
npx agent-browser wait 500

npx agent-browser click @e3
npx agent-browser wait --load networkidle
npx agent-browser wait 1000  # Show result

npx agent-browser record stop
```

### CI/CD Test Evidence

```bash
#!/bin/bash
# Record E2E test runs for CI artifacts

TEST_NAME="${1:-e2e-test}"
RECORDING_DIR="./test-recordings"
mkdir -p "$RECORDING_DIR"

npx agent-browser record start "$RECORDING_DIR/$TEST_NAME-$(date +%s).webm"

# Run test
if run_e2e_test; then
    echo "Test passed"
else
    echo "Test failed - recording saved"
fi

npx agent-browser record stop
```

## Best Practices

### 1. Add Pauses for Clarity

```bash
# Slow down for human viewing
npx agent-browser click @e1
npx agent-browser wait 500  # Let viewer see result
```

### 2. Use Descriptive Filenames

```bash
# Include context in filename
npx agent-browser record start ./recordings/login-flow-2024-01-15.webm
npx agent-browser record start ./recordings/checkout-test-run-42.webm
```

### 3. Handle Recording in Error Cases

```bash
#!/bin/bash
set -e

cleanup() {
    npx agent-browser record stop 2>/dev/null || true
    npx agent-browser close 2>/dev/null || true
}
trap cleanup EXIT

npx agent-browser record start ./automation.webm
# ... automation steps ...
```

### 4. Combine with Screenshots

```bash
# Record video AND capture key frames
npx agent-browser record start ./flow.webm

npx agent-browser open https://example.com
npx agent-browser screenshot ./screenshots/step1-homepage.png

npx agent-browser click @e1
npx agent-browser screenshot ./screenshots/step2-after-click.png

npx agent-browser record stop
```

## Output Format

- Default format: WebM (VP8/VP9 codec)
- Compatible with all modern browsers and video players
- Compressed but high quality

## Limitations

- Recording adds slight overhead to automation
- Large recordings can consume significant disk space
- Some headless environments may have codec limitations
