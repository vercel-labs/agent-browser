//! WebGPU probe: spawn a scratch daemon session with the WebGPU preset
//! enabled, render through a real WebGPU pass, and assert on actual pixels
//! twice: an in-page canvas readback and a decoded screenshot. WebGPU
//! failures are silent black (a screenshot request still returns 200), so
//! only pixel values prove anything. Opt-in via `agent-browser doctor
//! --webgpu` because it launches a second Chrome.
//!
//! Two subtleties this probe encodes:
//! - `navigator.gpu` only exists in secure contexts, and the daemon's
//!   `about:blank` is not one; the probe navigates to a temp `file://` page
//!   (file URLs are potentially trustworthy) so it works offline.
//! - The canvas readback must happen in the same task as the queue submit.
//!   After any `await`, the frame is presented and the current texture
//!   expires, so `drawImage` observes transparent black.

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
  try {
    if (!window.isSecureContext) return { ok: false, stage: 'context', detail: 'page is not a secure context' };
    if (!navigator.gpu) return { ok: false, stage: 'api', detail: 'navigator.gpu is undefined' };
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) return { ok: false, stage: 'adapter', detail: 'requestAdapter() returned null' };
    const info = adapter.info || {};
    const desc = [info.vendor, info.architecture, info.description].filter(Boolean).join(' ') || 'unknown adapter';
    const device = await adapter.requestDevice();
    const canvas = document.getElementById('c');
    const ctx = canvas.getContext('webgpu');
    if (!ctx) return { ok: false, stage: 'canvas', detail: 'getContext(webgpu) returned null', adapter: desc };
    ctx.configure({ device, format: navigator.gpu.getPreferredCanvasFormat(), alphaMode: 'opaque' });
    const draw = () => {
      const enc = device.createCommandEncoder();
      const pass = enc.beginRenderPass({ colorAttachments: [{
        view: ctx.getCurrentTexture().createView(),
        clearValue: { r: 1, g: 0, b: 0, a: 1 }, loadOp: 'clear', storeOp: 'store',
      }] });
      pass.end();
      device.queue.submit([enc.finish()]);
    };
    draw();
    // Same-task readback: no await between submit and drawImage, or the
    // frame is presented and the texture expires to transparent black.
    const c2 = document.createElement('canvas');
    c2.width = 64; c2.height = 64;
    const g = c2.getContext('2d');
    g.drawImage(canvas, 0, 0);
    const p = g.getImageData(32, 32, 1, 1).data;
    const ok = p[0] > 200 && p[1] < 64 && p[2] < 64;
    // Keep presenting like a real app so the screenshot sees fresh frames.
    const loop = () => { draw(); requestAnimationFrame(loop); };
    requestAnimationFrame(loop);
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
        engine: None,
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

    let open_cmd = json!({
        "id": new_id(),
        "action": "navigate",
        "url": format!("file://{}", page_path.display()),
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
            checks.push(probe_fail(
                "webgpu.screenshot",
                format!(
                    "Screenshot did not capture WebGPU output: expected red, got rgb({},{},{})",
                    r, g, b
                ),
            ));
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
