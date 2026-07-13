//! WebGPU probe: spawn a scratch daemon session with the WebGPU preset
//! enabled, render through a real WebGPU pass, and assert on actual pixels
//! twice: an offscreen buffer readback and a decoded screenshot. WebGPU
//! failures are silent black (a screenshot request still returns 200), so
//! only pixel values prove anything. Opt-in via `agent-browser doctor
//! --webgpu` because it launches a second Chrome.
//!
//! Subtleties this probe encodes (each verified on real machines):
//! - `navigator.gpu` only exists in secure contexts, and the daemon's
//!   `about:blank` is not one; the probe navigates to a temp `file://` page
//!   (file URLs are potentially trustworthy) so it works offline.
//! - The render proof is `copyTextureToBuffer` + `mapAsync` on an offscreen
//!   texture, never a canvas snapshot: canvas readback is presentation-timing
//!   dependent and reads transparent black on Windows even when rendering
//!   works.
//! - Every await in the probe script races a timeout. Runtime.evaluate with
//!   awaitPromise never returns if a WebGPU promise stalls, which would hang
//!   doctor indefinitely.
//! - The screenshot check exercises the compositor path separately: headless
//!   Chrome cannot capture WebGPU canvas presentation on Windows and Linux
//!   because of an upstream limitation, so render can pass while the
//!   screenshot fails. The failure message points at `--headed` on a real or
//!   virtual display, which is the verified workaround.

use std::env;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use serde_json::{json, Value};

use super::helpers::new_id;
use super::{Check, Status};
use crate::connection::{cleanup_stale_files, ensure_daemon, send_command, DaemonOptions};

const CATEGORY: &str = "WebGPU probe";

/// Minimal page with a viewport-filling canvas. The probe script draws into
/// it and leaves a rAF render loop running so the compositor has fresh
/// frames when the screenshot is captured.
const PROBE_HTML: &str = "<!doctype html><html><head><style>html,body{margin:0;padding:0}#c{display:block;width:100vw;height:100vh}</style></head><body><canvas id='c' width='64' height='64'></canvas></body></html>";

/// Async IIFE evaluated in the page. Returns `{ ok, stage, detail, adapter }`.
/// Uses only single quotes so it embeds cleanly in the JSON envelope.
const PROBE_JS: &str = r#"(async () => {
  const withTimeout = (p, ms, label) => Promise.race([
    Promise.resolve(p),
    new Promise((_, rej) => setTimeout(() => rej(new Error(label + ' timed out after ' + ms + 'ms')), ms)),
  ]);
  try {
    if (!window.isSecureContext) return { ok: false, stage: 'context', detail: 'page is not a secure context' };
    if (!navigator.gpu) return { ok: false, stage: 'api', detail: 'navigator.gpu is undefined' };
    // A cold Chrome can return null while the GPU process is still starting
    // (observed on Windows when eval runs right after launch); retry briefly.
    let adapter = null;
    for (let i = 0; i < 5 && !adapter; i++) {
      if (i > 0) await new Promise(r => setTimeout(r, 1000));
      adapter = await withTimeout(navigator.gpu.requestAdapter(), 10000, 'requestAdapter');
    }
    if (!adapter) return { ok: false, stage: 'adapter', detail: 'requestAdapter() returned null (5 attempts)' };
    const info = adapter.info || {};
    const desc = [info.vendor, info.architecture, info.description].filter(Boolean).join(' ') || 'unknown adapter';
    const device = await withTimeout(adapter.requestDevice(), 10000, 'requestDevice');
    // Deterministic render proof: offscreen texture -> buffer readback.
    const tex = device.createTexture({ size: [64, 64], format: 'rgba8unorm', usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC });
    const buf = device.createBuffer({ size: 256 * 64, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const enc = device.createCommandEncoder();
    const pass = enc.beginRenderPass({ colorAttachments: [{
      view: tex.createView(),
      clearValue: { r: 1, g: 0, b: 0, a: 1 }, loadOp: 'clear', storeOp: 'store',
    }] });
    pass.end();
    enc.copyTextureToBuffer({ texture: tex }, { buffer: buf, bytesPerRow: 256 }, [64, 64]);
    device.queue.submit([enc.finish()]);
    await withTimeout(buf.mapAsync(GPUMapMode.READ), 10000, 'mapAsync');
    const p = new Uint8Array(buf.getMappedRange(256 * 32 + 32 * 4, 4)).slice();
    buf.unmap();
    const ok = p[0] > 200 && p[1] < 64 && p[2] < 64;
    // Drive the visible canvas with a rAF render loop so the follow-up
    // screenshot check exercises the presentation/compositor path.
    const canvas = document.getElementById('c');
    const ctx = canvas && canvas.getContext('webgpu');
    if (ctx) {
      ctx.configure({ device, format: navigator.gpu.getPreferredCanvasFormat(), alphaMode: 'opaque' });
      const draw = () => {
        const e = device.createCommandEncoder();
        const r = e.beginRenderPass({ colorAttachments: [{
          view: ctx.getCurrentTexture().createView(),
          clearValue: { r: 1, g: 0, b: 0, a: 1 }, loadOp: 'clear', storeOp: 'store',
        }] });
        r.end();
        device.queue.submit([e.finish()]);
      };
      draw();
      const loop = () => { draw(); requestAnimationFrame(loop); };
      requestAnimationFrame(loop);
    }
    return { ok, stage: 'pixels', detail: 'rgba(' + [p[0], p[1], p[2], p[3]].join(',') + ')', adapter: desc };
  } catch (e) {
    return { ok: false, stage: 'exception', detail: String((e && e.message) || e) };
  }
})()"#;

pub(super) fn check(checks: &mut Vec<Check>) {
    if env::var("AGENT_BROWSER_PROVIDER").is_ok() {
        checks.push(Check::new(
            "webgpu.skipped.provider",
            CATEGORY,
            Status::Info,
            "Skipped (AGENT_BROWSER_PROVIDER is set; would consume cloud quota)",
        ));
        return;
    }
    if env::var("AGENT_BROWSER_CDP").is_ok() {
        checks.push(Check::new(
            "webgpu.skipped.cdp",
            CATEGORY,
            Status::Info,
            "Skipped (AGENT_BROWSER_CDP is set; would attach to a real browser)",
        ));
        return;
    }

    let stamp = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let session = format!("doctor-webgpu-{}", stamp);
    let page_path = env::temp_dir().join(format!("agent-browser-doctor-webgpu-{}.html", stamp));
    let shot_path = env::temp_dir().join(format!("agent-browser-doctor-webgpu-{}.png", stamp));

    if let Err(e) = std::fs::write(&page_path, PROBE_HTML) {
        checks.push(Check::new(
            "webgpu.setup",
            CATEGORY,
            Status::Fail,
            format!("Could not write probe page: {}", e),
        ));
        return;
    }

    // Armed after `ensure_daemon` succeeds; Drop closes the scratch session,
    // cleans sidecar files, and removes temp files on every path out of
    // this function.
    let mut guard = ProbeGuard {
        session: None,
        files: vec![page_path.clone(), shot_path.clone()],
    };

    let opts = DaemonOptions {
        headed: false,
        debug: false,
        executable_path: None,
        extensions: &[],
        init_scripts: &[],
        enable: &[],
        args: None,
        user_agent: None,
        proxy: None,
        proxy_bypass: None,
        proxy_username: None,
        proxy_password: None,
        ignore_https_errors: false,
        allow_file_access: false,
        hide_scrollbars: true,
        webgpu: true,
        profile: None,
        state: None,
        provider: None,
        device: None,
        session_name: None,
        restore_save: None,
        restore_check_url: None,
        restore_check_text: None,
        restore_check_fn: None,
        download_path: None,
        allowed_domains: None,
        action_policy: None,
        confirm_actions: None,
        // Pinned: the probe is Chrome-specific, and an inherited
        // AGENT_BROWSER_ENGINE=lightpanda would otherwise test the wrong
        // browser and report a false WebGPU failure.
        engine: Some("chrome"),
        auto_connect: false,
        idle_timeout: None,
        default_timeout: None,
        cdp: None,
        no_auto_dialog: false,
        plugins: None,
    };

    let started = Instant::now();
    if let Err(e) = ensure_daemon(&session, &opts) {
        checks.push(
            Check::new(
                "webgpu.daemon",
                CATEGORY,
                Status::Fail,
                format!("Could not start daemon: {}", e),
            )
            .with_fix("check Chrome install and re-run with --debug"),
        );
        return;
    }
    guard.session = Some(session.clone());
    let launch_cmd = json!({
        "id": new_id(),
        "action": "launch",
        "headless": true,
        "webgpu": true,
        "engine": "chrome",
    });
    if let Err(e) = send_json(launch_cmd, &session) {
        checks.push(
            Check::new(
                "webgpu.launch",
                CATEGORY,
                Status::Fail,
                format!("Browser launch failed: {}", e),
            )
            .with_fix("agent-browser install   # or check --debug output"),
        );
        return;
    }

    let page_url = match url::Url::from_file_path(&page_path) {
        Ok(u) => u.to_string(),
        Err(()) => {
            checks.push(Check::new(
                "webgpu.setup",
                CATEGORY,
                Status::Fail,
                format!("Could not build file URL for {}", page_path.display()),
            ));
            return;
        }
    };
    let open_cmd = json!({
        "id": new_id(),
        "action": "navigate",
        "url": page_url,
    });
    if let Err(e) = send_json(open_cmd, &session) {
        checks.push(
            Check::new(
                "webgpu.navigate",
                CATEGORY,
                Status::Fail,
                format!("Navigation to probe page failed: {}", e),
            )
            .with_fix("re-run with --debug for full launch logs"),
        );
        return;
    }
    let eval_cmd = json!({
        "id": new_id(),
        "action": "evaluate",
        "script": PROBE_JS,
    });
    let result = match send_command(eval_cmd, &session) {
        Ok(resp) if resp.success => resp
            .data
            .as_ref()
            .and_then(|d| d.get("result"))
            .cloned()
            .unwrap_or(Value::Null),
        Ok(resp) => {
            checks.push(probe_fail(
                "webgpu.eval",
                format!(
                    "Probe script failed: {}",
                    resp.error.unwrap_or_else(|| "unknown error".to_string())
                ),
            ));
            return;
        }
        Err(e) => {
            checks.push(probe_fail(
                "webgpu.eval",
                format!("Probe script failed: {}", e),
            ));
            return;
        }
    };

    let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let stage = result.get("stage").and_then(|v| v.as_str()).unwrap_or("?");
    let detail = result.get("detail").and_then(|v| v.as_str()).unwrap_or("");
    let adapter = result
        .get("adapter")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown adapter");

    if ok {
        checks.push(Check::new(
            "webgpu.render",
            CATEGORY,
            Status::Pass,
            format!(
                "WebGPU rendered and read back pixels in {:.2}s ({})",
                started.elapsed().as_secs_f64(),
                adapter
            ),
        ));
    } else {
        let mut message = format!("WebGPU probe failed at '{}': {}", stage, detail);
        if stage != "adapter" && stage != "api" && stage != "context" {
            message.push_str(&format!(" (adapter: {})", adapter));
        }
        checks.push(probe_fail("webgpu.render", message));
        return;
    }

    // Second assertion: the compositor path. Screenshots and video capture
    // read presented frames, which can be black even when in-page readback
    // works, so decode a real screenshot and check the center pixel.
    let shot_cmd = json!({
        "id": new_id(),
        "action": "screenshot",
        "path": shot_path.display().to_string(),
    });
    if let Err(e) = send_json(shot_cmd, &session) {
        checks.push(probe_fail(
            "webgpu.screenshot",
            format!("Screenshot of WebGPU page failed: {}", e),
        ));
        return;
    }

    match center_pixel(&shot_path) {
        Ok((r, g, b)) if r > 200 && g < 64 && b < 64 => {
            checks.push(Check::new(
                "webgpu.screenshot",
                CATEGORY,
                Status::Pass,
                format!("Screenshot captured WebGPU output (rgb({},{},{}))", r, g, b),
            ));
        }
        Ok((r, g, b)) => {
            // Rendering passed but the presented canvas is not in the
            // capture: headless Chrome cannot composite WebGPU canvas
            // presentation on Windows/Linux (upstream limitation).
            checks.push(
                Check::new(
                    "webgpu.screenshot",
                    CATEGORY,
                    Status::Fail,
                    format!(
                        "WebGPU renders, but headless screenshots miss the canvas (expected red, got rgb({},{},{})); this is a known headless Chrome limitation on this platform",
                        r, g, b
                    ),
                )
                .with_fix(if cfg!(target_os = "linux") {
                    "add --headed (a virtual display starts automatically; needs Xvfb: apt-get install -y xvfb)"
                } else {
                    "run WebGPU sessions with --headed on a logged-in desktop for screenshots"
                }),
            );
        }
        Err(e) => {
            checks.push(probe_fail(
                "webgpu.screenshot",
                format!("Could not decode screenshot: {}", e),
            ));
        }
    }
}

fn center_pixel(path: &PathBuf) -> Result<(u8, u8, u8), String> {
    let img = image::open(path)
        .map_err(|e| format!("{}", e))?
        .into_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".to_string());
    }
    let p = img.get_pixel(w / 2, h / 2);
    Ok((p[0], p[1], p[2]))
}

fn probe_fail(id: &str, message: String) -> Check {
    let check = Check::new(id.to_string(), CATEGORY, Status::Fail, message);
    if cfg!(target_os = "linux") {
        check.with_fix(
            "apt-get install -y libvulkan1 mesa-vulkan-drivers   # WebGPU on Linux needs a Vulkan loader + Mesa ICD",
        )
    } else {
        check.with_fix("update Chrome to the latest stable (agent-browser install)")
    }
}

fn send_json(cmd: Value, session: &str) -> Result<(), String> {
    match send_command(cmd, session) {
        Ok(resp) => {
            if resp.success {
                Ok(())
            } else {
                Err(resp.error.unwrap_or_else(|| "unknown error".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// Best-effort cleanup when the probe panics or returns early.
struct ProbeGuard {
    session: Option<String>,
    files: Vec<PathBuf>,
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(ref session) = self.session {
            let close_cmd = json!({ "id": new_id(), "action": "close" });
            let _ = send_command(close_cmd, session);
            cleanup_stale_files(session);
        }
        for f in &self.files {
            let _ = std::fs::remove_file(f);
        }
    }
}
