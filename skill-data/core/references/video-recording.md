# Video Recording

Capture browser automation as video for debugging, documentation, or verification.

**Related**: [commands.md](commands.md) for full command reference, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Requirements](#requirements)
- [Basic Recording](#basic-recording)
- [Recording Commands](#recording-commands)
- [Frame Rate](#frame-rate)
- [Recording Receipts](#recording-receipts)
- [Use Cases](#use-cases)
- [Best Practices](#best-practices)
- [Output Format](#output-format)
- [Limitations](#limitations)

## Requirements

Recording pipes frames into `ffmpeg`, which must be on `PATH` with the `libvpx` and `libx264` encoders. Install it with `brew install ffmpeg` (macOS) or `sudo apt install ffmpeg` (Debian/Ubuntu); `agent-browser doctor` reports it under "Recording". Nothing else in agent-browser needs ffmpeg.

Supported formats are `.webm` (VP8 via libvpx) and `.mp4` (H.264 via libx264). Other extensions are handed to ffmpeg as-is with H.264 video. A path with no extension is rejected before recording starts.

## Basic Recording

`record start` records the current active page as-is. Without a URL it attaches to the tab you already have open (no navigation, no new tab, page state and hydration intact). With a URL it navigates the active tab there first.

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

# Start recording to file (30 output fps)
agent-browser record start ./output.webm

# Start recording at a specific rate (1-60)
agent-browser record start ./output.webm --fps 60

# Stop current recording
agent-browser record stop

# Restart with new file (stops current + starts new)
agent-browser record restart ./take2.webm --fps 60

# Navigate the active tab, then record
agent-browser record start ./output.webm https://example.com/checkout

# Record in a separate tab: open it first, then start recording
agent-browser tab new https://example.com
agent-browser record start ./output.webm
```

## Frame Rate

`--fps` controls output playback (30 by default, 1 to 60 allowed), not the rate of new pictures. Chrome supplies repaint-driven screencast images, which may be sparse. Requesting 60 fps does not prove smoothness or that intermediate states were captured.

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

The last image is held through gaps. If the output ticker falls behind, it fills up to five seconds of missed slots plus the current slot and skips the remaining deficit. A late first image can be repeated over earlier output slots. Playback duration and output FPS are not evidence of continuous capture. 60 fps roughly doubles the bitrate of 30 fps.

## Recording Receipts

Keep `recordingId` from `record start`. Start success does not prove the first frame has arrived. `record stop --json` returns a receipt in `data` on both success and failure:

- `capture.startedAt`/`endedAt`: ISO-8601 UTC capture-loop times. `durationMs`: monotonic capture wall time, excluding teardown and encoder completion.
- `capture.firstFrameAt`/`lastFrameAt` and `firstFrameAfterMs`/`lastFrameAfterMs`: frame receipt times and offsets from capture start. `timestampSource` is `local-receive`, not browser presentation time. Values are `null` until frames arrive.
- `capturedFrames`: decoded screencast events consumed by the loop, including those later discarded, not pixel-unique images. `capture.averageFps` is that count divided by wall duration; `maxFrameGapMs` includes leading and trailing gaps.
- `frames`/`output.frames`: images written to ffmpeg. `output.encodedFrames`: ffmpeg's reported count. `output.fps`: requested output rate. `output.durationMs`: encoded frames divided by output rate after encoder success, not capture wall duration.
- `output.heldFrames`, `droppedFrames`, `skippedFrames`: repeated images, decoded images discarded by the recorder, and skipped output slots. Frames lost before the capture loop are not measured.
- `success`/`error` and `output.encoderSucceeded`: terminal outcome and encoder exit result. `file.exists`/`sizeBytes` are separate evidence; an existing file is not proof of successful encoding. Pending or unavailable evidence is `null`.

After a stop timeout, inspect `session info --json` → `data.runtime.recording.current`/`last` and match `recordingId`. Exact transport retries replay the original stop response without stopping a newer take; a new stop command targets the current take. `record restart` returns `previousRecording`, including failed receipts, separately from the new recording. Receipts are in daemon memory and do not survive its exit. Read the timing/gap warning and inspect the video before claiming smoothness.

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
agent-browser wait --url "**/dashboard"
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

- Format follows the extension: `.webm` (VP8 via libvpx) or `.mp4` (H.264 via libx264); other extensions get H.264 in that container
- Default output frame rate: 30 fps (`--fps` accepts 1 to 60); capture rate is reported separately
- Compatible with all modern browsers and video players
- Compressed but high quality

## Limitations

- Recording adds slight overhead to automation, and higher frame rates add more
- Large recordings can consume significant disk space; 60 fps roughly doubles the bitrate of 30 fps
- Capture depends on repaint frequency and recorder throughput; frame event counts do not prove unique pixels or smoothness
- Some headless environments may have codec limitations; an ffmpeg built without libvpx or libx264 cannot write the matching format
