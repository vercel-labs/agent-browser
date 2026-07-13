# WebGPU

Screenshots and video of WebGPU pages (three.js `WebGPURenderer`, Babylon.js, raw WebGPU) in headless Chrome. Without setup this is a silent failure: the page loads, the screenshot succeeds, and the canvas is black.

## Quick start

```bash
agent-browser --webgpu open https://my-webgpu-app.example.com
# wait for the app to render (see "Timing" below)
agent-browser screenshot app.png
```

`--webgpu` (or `AGENT_BROWSER_WEBGPU=1`, or `"webgpu": true` in agent-browser.json) applies a launch preset:

- everywhere: `--enable-unsafe-webgpu` (WebGPU is hidden in headless/blocklisted environments by default)
- Linux only: `--enable-features=Vulkan --use-angle=vulkan --use-vulkan=swiftshader --use-webgpu-adapter=swiftshader --disable-vulkan-surface` — routes WebGPU through SwiftShader's software Vulkan, so it works with no GPU (containers, CI)

macOS uses the hardware Metal backend; Windows uses D3D. Nothing extra to install on either.

## Platform matrix (verified)

| Platform | WebGPU rendering (headless) | Screenshots of WebGPU canvases |
|---|---|---|
| macOS | works | works headless |
| Windows | works (hardware D3D) | **headless captures black** — use `--headed` on a logged-in desktop |
| Linux | works (SwiftShader Vulkan) | headless capture not supported upstream — add `--headed` (virtual display starts automatically) |

The Windows/Linux screenshot gap is an upstream headless-Chrome limitation: WebGPU canvas *presentation* never reaches the headless compositor, even though rendering itself works (verified by pixel readback). It is not an agent-browser or flag problem — no known flag combination fixes it. Rendering, `eval`-based pixel readbacks, and compute all work headless everywhere.

On Linux, `--headed` is all you need even on displayless servers and containers: when no `DISPLAY` is set and Xvfb is installed (`apt-get install -y xvfb`), agent-browser starts a private virtual display for the browser and tears it down with it. Set `AGENT_BROWSER_NO_XVFB=1` to opt out.

```bash
agent-browser --webgpu --headed open https://my-webgpu-app.example.com
agent-browser screenshot app.png   # real WebGPU pixels, no display hardware
```

On Windows, the session must run headed in a logged-in desktop session (an ssh/Session-0 context is not enough — schedule the launch on the interactive desktop, e.g. `schtasks /IT`, then drive it from anywhere).

## Verify the pipeline

```bash
agent-browser doctor --webgpu
```

This launches a scratch session with the preset and pixel-checks two stages separately:

1. **render** — requests an adapter (with retries; a cold Chrome returns null while the GPU process starts), clears an offscreen texture to red through a real render pass, and reads the buffer back. Proves WebGPU works at all, and reports the adapter (e.g. `nvidia ampere`, `apple metal-3`, `google swiftshader`).
2. **screenshot** — decodes an actual screenshot of a presenting canvas. Proves the capture path. Expected to fail headless on Windows/Linux (see matrix); the failure message says so and points at `--headed`.

## Linux / containers / CI

The SwiftShader Vulkan path needs the system Vulkan loader and Mesa ICD. Without them `requestAdapter()` returns null (or fails with "A valid external Instance reference no longer exists"):

```bash
apt-get install -y libvulkan1 mesa-vulkan-drivers
```

Container recipe (Debian/Ubuntu base; xvfb needed only for the screenshot path). Verified with both Chrome for Testing and Debian's `chromium` package (set `AGENT_BROWSER_EXECUTABLE_PATH=/usr/bin/chromium` for the latter — useful on ARM64, where Chrome for Testing has no Linux builds):

```dockerfile
FROM node:22-bookworm-slim
RUN apt-get update && apt-get install -y \
    ca-certificates libvulkan1 mesa-vulkan-drivers xvfb xauth \
    && rm -rf /var/lib/apt/lists/*
RUN npm install -g agent-browser \
    && agent-browser install   # downloads Chrome for Testing
```

No real GPU or `/dev/dri` is required. To prefer a real GPU on a Linux machine that has working hardware Vulkan, override both the Vulkan driver and the adapter — the preset pins `--use-vulkan=swiftshader`, so overriding only the adapter still enumerates SwiftShader (user `--args` win over the preset):

```bash
agent-browser --webgpu --args "--use-vulkan=native,--use-webgpu-adapter=default" open ...
```

## Secure contexts

`navigator.gpu` only exists in secure contexts. `https://`, `http://localhost`, and `file://` qualify; a plain `http://` LAN address or `data:` URL does not — WebGPU will be `undefined` there no matter which flags are set.

## Timing: don't screenshot too early

WebGPU apps initialize asynchronously. A screenshot taken at `load` captures a blank canvas with no error anywhere. In particular:

- **three.js `WebGPURenderer`**: `renderer.init()` is async; the first frame lands only after it resolves. Also note three.js **silently falls back to WebGL2** when it can't get a WebGPU adapter — the page "works" but you're not testing WebGPU (and on old setups the WebGL fallback itself may be black).
- Wait for an app-specific signal before capturing: a canvas with content, a "ready" DOM marker, or simply a rendered-frame check:

```bash
agent-browser wait --fn "window.__appReady === true"
# or generically: give the render loop a frame or two
agent-browser eval "new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))"
agent-browser screenshot app.png
```

To check which backend a three.js app actually got:

```bash
agent-browser eval "document.querySelector('canvas').getContext('webgpu') ? 'webgpu' : 'webgl-fallback'"
```

## Reading pixels back inside the page

If you `eval` your own WebGPU readback, don't snapshot the canvas (`drawImage(webgpuCanvas, ...)`) — it depends on presentation timing and reads transparent black on Windows even when rendering works. Render to an offscreen texture and read it back deterministically:

```js
const tex = device.createTexture({ size: [w, h], format: 'rgba8unorm',
  usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC });
// ...render to tex, then:
encoder.copyTextureToBuffer({ texture: tex }, { buffer, bytesPerRow }, [w, h]);
device.queue.submit([encoder.finish()]);
await buffer.mapAsync(GPUMapMode.READ);
```

This works headless on every platform (it's how `doctor --webgpu` proves rendering).

## Performance expectations

SwiftShader is a CPU rasterizer. Simple scenes render fine; heavy three.js scenes are single-digit FPS. For screenshots that's usually irrelevant; for smooth video capture of complex scenes, use hardware (macOS/Windows, or Linux with `--use-vulkan=native,--use-webgpu-adapter=default` and real Vulkan drivers).
