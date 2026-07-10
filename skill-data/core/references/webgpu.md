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
- Linux only: `--enable-features=Vulkan --use-angle=vulkan --use-vulkan=swiftshader --use-webgpu-adapter=swiftshader --disable-vulkan-surface` — routes WebGPU through SwiftShader's software Vulkan with software compositing, so it works with no GPU and no display (containers, CI)

macOS uses the hardware Metal backend; Windows uses D3D. Nothing extra to install on either.

## Verify the pipeline

```bash
agent-browser doctor --webgpu
```

This launches a scratch session with the preset, requests an adapter, renders a red clear through a real WebGPU render pass, and pixel-checks both an in-page readback and a decoded screenshot. If both pass, WebGPU screenshots will work. Failures include the failing stage (api / adapter / pixels / screenshot) and the adapter that was used.

## Linux / containers / CI

The SwiftShader Vulkan path needs the system Vulkan loader and Mesa ICD. Without them `requestAdapter()` returns null (or fails with "A valid external Instance reference no longer exists"):

```bash
apt-get install -y libvulkan1 mesa-vulkan-drivers
```

Minimal Docker recipe (Debian/Ubuntu base):

```dockerfile
FROM node:22-bookworm-slim
RUN apt-get update && apt-get install -y \
    ca-certificates libvulkan1 mesa-vulkan-drivers \
    && rm -rf /var/lib/apt/lists/*
RUN npm install -g agent-browser \
    && agent-browser install   # downloads Chrome for Testing
# sanity check at build time (optional):
# RUN agent-browser doctor --webgpu --offline
```

No real GPU, `/dev/dri`, or Xvfb is required — the preset composites in software under `--headless=new`.

To prefer a real GPU on a Linux machine that has working hardware Vulkan, override the adapter (user `--args` win over the preset):

```bash
agent-browser --webgpu --args "--use-webgpu-adapter=default" open ...
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

If you `eval` your own WebGPU readback: `drawImage(webgpuCanvas, ...)` must run **in the same task** as the `queue.submit()`. After any `await`, the frame is presented and the current texture expires — you'll read transparent black even though rendering worked. (Screenshots don't have this problem; they capture presented frames.)

## Performance expectations

SwiftShader is a CPU rasterizer. Simple scenes render fine; heavy three.js scenes are single-digit FPS. For screenshots that's usually irrelevant; for smooth video capture of complex scenes, use hardware (macOS/Windows, or Linux with `--use-webgpu-adapter=default` and real Vulkan drivers).
