use std::collections::HashMap;

use base64::Engine;
use serde_json::{json, Value};

use super::actions::session;
use super::query;
use super::target::{resolve_target, Target};
use crate::native::actions::{pdf_save_path, DaemonState};
use crate::native::screenshot::{
    annotation_overlay_js, annotation_removal_js, filter_annotations, parse_rect,
    project_annotations, save_screenshot, screenshot_format, screenshot_options, RawAnnotation,
    Rect,
};
use crate::native::snapshot::SnapshotOptions;

const NOT_YET: &str = "not yet supported on Firefox";

fn reject_unsupported_screenshot_options(cmd: &Value) -> Result<(), String> {
    if cmd
        .get("ifChanged")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!(
            "Conditional screenshots (`ifChanged`) are {NOT_YET}"
        ));
    }
    Ok(())
}

/// The BiDi parameters for a viewport or full-page screenshot of `context`.
fn screenshot_params(cmd: &Value, context: &str) -> Result<Value, String> {
    reject_unsupported_screenshot_options(cmd)?;
    let format = screenshot_format(cmd);
    let mime = match format.as_str() {
        "png" => "image/png",
        "jpeg" => "image/jpeg",
        other => return Err(format!("Unsupported screenshot format on Firefox: {other}")),
    };
    let full_page = cmd
        .get("fullPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut params = json!({
        "context": context,
        "origin": if full_page { "document" } else { "viewport" },
        "format": { "type": mime },
    });
    if format == "jpeg" {
        let quality = cmd.get("quality").and_then(Value::as_i64).unwrap_or(80);
        params["format"]["quality"] = json!(quality as f64 / 100.0);
    }
    Ok(params)
}

/// The BiDi `element` clip for `shared_id`.
fn element_clip(shared_id: &str) -> Value {
    json!({"type": "element", "element": {"sharedId": shared_id}})
}

/// The BiDi parameters for a PDF of `context`, following Chrome's `handle_pdf` options.
fn pdf_params(cmd: &Value, context: &str) -> Value {
    let landscape = cmd
        .get("landscape")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let prefer_css_page_size = cmd
        .get("preferCSSPageSize")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    json!({
        "context": context,
        "background": cmd.get("printBackground").and_then(Value::as_bool).unwrap_or(true),
        "orientation": if landscape { "landscape" } else { "portrait" },
        "shrinkToFit": !prefer_css_page_size,
    })
}

async fn evaluate(
    ff: &super::session::FirefoxSession,
    context: &str,
    expression: &str,
) -> Result<Value, String> {
    let r = ff
        .bidi
        .send(
            "script.evaluate",
            json!({
                "expression": expression,
                "target": {"context": context},
                "awaitPromise": true,
                "resultOwnership": "none",
            }),
        )
        .await?;
    if r["type"] == "exception" {
        return Err(format!(
            "Evaluation error: {}",
            r["exceptionDetails"]["text"]
                .as_str()
                .unwrap_or("script exception")
        ));
    }
    Ok(r["result"].clone())
}

async fn target_rect(state: &DaemonState, target: &Target) -> Option<Rect> {
    let ff = state.firefox.as_ref()?;
    let value = query::element_rect(&ff.bidi, target).await.ok().flatten()?;
    parse_rect(&value)
}

const FRAME_RECTS_JS: &str = r#"() => Array.from(document.querySelectorAll('iframe,frame')).map(f => {
  const r = f.getBoundingClientRect();
  return [f.contentWindow, r.left + f.clientLeft, r.top + f.clientTop];
})"#;

/// The context id that contains `child` in a `browsingContext.getTree` context list.
fn parent_context(contexts: &Value, child: &str) -> Option<String> {
    for c in contexts.as_array()? {
        let kids = &c["children"];
        if kids
            .as_array()
            .is_some_and(|k| k.iter().any(|k| k["context"] == child))
        {
            return c["context"].as_str().map(String::from);
        }
        if let Some(found) = parent_context(kids, child) {
            return Some(found);
        }
    }
    None
}

/// Offset of the document in `from` inside the viewport of its ancestor `to`, summed over the iframes between them.
async fn frame_offset(
    ff: &super::session::FirefoxSession,
    tree: &Value,
    from: &str,
    to: &str,
) -> Option<(f64, f64)> {
    let (mut current, mut dx, mut dy) = (from.to_string(), 0.0, 0.0);
    while current != to {
        let parent = parent_context(&tree["contexts"], &current)?;
        let rows = ff
            .bidi
            .call_function(&parent, FRAME_RECTS_JS, None, vec![])
            .await
            .ok()?;
        let row = rows.as_array()?.iter().find(|r| r[0] == current.as_str())?;
        dx += row[1].as_f64()?;
        dy += row[2].as_f64()?;
        current = parent;
    }
    Some((dx, dy))
}

/// Boxes for every resolved ref, read through each node's handle and expressed in `capture` viewport coordinates.
async fn collect_annotations(
    state: &DaemonState,
    capture: &str,
) -> Result<(Vec<RawAnnotation>, Vec<RawAnnotation>), String> {
    let Some(ff) = state.firefox.as_ref() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let tree = ff
        .bidi
        .send("browsingContext.getTree", json!({"root": ff.top_context}))
        .await?;
    let mut targets_by_id = HashMap::with_capacity(ff.refs.len());
    for (target, id) in &ff.refs {
        targets_by_id.entry(*id).or_insert(target);
    }
    let mut annotations = Vec::new();
    let mut overlay_annotations = Vec::new();
    for (ref_id, entry) in state.ref_map.entries_sorted() {
        let Some(id) = entry.backend_node_id else {
            continue;
        };
        let Some(&(context, shared_id)) = targets_by_id.get(&id) else {
            continue;
        };
        let target = Target {
            context: context.clone(),
            shared_id: shared_id.clone(),
        };
        let Some(mut rect) = target_rect(state, &target).await else {
            continue;
        };
        if context != capture {
            let Some((dx, dy)) = frame_offset(ff, &tree, context, capture).await else {
                continue;
            };
            rect.x += dx;
            rect.y += dy;
        }
        if rect.width <= 0.0 || rect.height <= 0.0 {
            continue;
        }
        let number = ref_id
            .strip_prefix('e')
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        let annotation = RawAnnotation {
            ref_id,
            number,
            role: entry.role,
            name: (!entry.name.is_empty()).then_some(entry.name),
            rect,
        };
        if context == capture {
            overlay_annotations.push(annotation.clone());
        }
        annotations.push(annotation);
    }
    Ok((annotations, overlay_annotations))
}

/// Viewport, full-page or element screenshot, with annotations drawn before capture.
/// Must return what Chrome's `handle_screenshot` returns for the same flags.
pub async fn screenshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    screenshot_params(cmd, "validate")?;
    let options = screenshot_options(cmd);
    let annotate = options.annotate;
    let full_page = options.full_page;

    if annotate {
        state.ref_map.begin_snapshot();
        let ff = state.firefox.as_mut().ok_or("Browser not launched")?;
        let snapshot_options = SnapshotOptions {
            interactive: true,
            ..SnapshotOptions::default()
        };
        Box::pin(super::snapshot::take_snapshot(
            ff,
            &snapshot_options,
            &mut state.ref_map,
        ))
        .await?;
    }

    let target = match options.selector.as_deref() {
        Some(selector) => Some(resolve_target(state, selector).await?),
        None => None,
    };
    let clip_rect = match (&target, annotate) {
        (Some(target), true) => target_rect(state, target).await,
        _ => None,
    };
    let capture_context = match (&target, full_page) {
        (Some(target), false) => target.context.clone(),
        _ => session(state)?.context.clone(),
    };
    let (annotations, overlay_annotations) = if annotate {
        let (annotations, overlay_annotations) =
            collect_annotations(state, &capture_context).await?;
        (
            filter_annotations(annotations, clip_rect.as_ref()),
            filter_annotations(overlay_annotations, clip_rect.as_ref()),
        )
    } else {
        (Vec::new(), Vec::new())
    };

    let data = {
        let ff = session(state)?;
        let context = capture_context.clone();
        let overlay = if annotate && !overlay_annotations.is_empty() {
            evaluate(ff, &context, &annotation_overlay_js(&overlay_annotations)).await?;
            true
        } else {
            false
        };
        let mut params = screenshot_params(cmd, &context)?;
        if !full_page {
            if let Some(target) = &target {
                params["clip"] = element_clip(&target.shared_id);
            }
        }
        let result = ff
            .bidi
            .send("browsingContext.captureScreenshot", params)
            .await;
        if overlay {
            let _ = evaluate(ff, &context, &annotation_removal_js()).await;
        }
        let result = result?;
        let data = result
            .get("data")
            .and_then(Value::as_str)
            .ok_or("No screenshot data returned")?
            .to_string();
        data
    };

    let mut response = json!({ "path": save_screenshot(&data, &options)? });
    if !annotations.is_empty() {
        let scroll = if full_page {
            let ff = session(state)?;
            let value = ff
                .eval("({x: window.scrollX || 0, y: window.scrollY || 0})")
                .await?;
            Some((
                value.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                value.get("y").and_then(Value::as_f64).unwrap_or(0.0),
            ))
        } else {
            None
        };
        let projected = project_annotations(&annotations, clip_rect.as_ref(), scroll);
        response["annotations"] = serde_json::to_value(&projected)
            .map_err(|e| format!("Failed to serialize annotations: {}", e))?;
    }
    Ok(response)
}

/// PDF of the active tab (`browsingContext.print`). Must return what Chrome's `handle_pdf` returns.
pub async fn pdf(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let params = pdf_params(cmd, &ff.context);
    let result = ff.bidi.send("browsingContext.print", params).await?;
    let data = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or("No PDF data returned")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("Failed to decode PDF: {}", e))?;
    let save_path = pdf_save_path(cmd);
    std::fs::write(&save_path, &bytes).map_err(|e| format!("Failed to save PDF: {}", e))?;
    Ok(json!({ "path": save_path }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn full_page_jpeg_carries_quality_as_a_fraction() {
        let params = screenshot_params(
            &json!({"fullPage": true, "format": "jpeg", "quality": 55}),
            "c",
        )
        .unwrap();
        assert_eq!(params["origin"], "document");
        assert_eq!(
            params["format"],
            json!({"type": "image/jpeg", "quality": 0.55})
        );
        let default = screenshot_params(&json!({"format": "jpeg"}), "c").unwrap();
        assert_eq!(default["format"]["quality"], 0.8);
    }

    #[test]
    fn type_aliases_format_and_png_ignores_quality() {
        let params = screenshot_params(&json!({"type": "jpeg", "quality": 30}), "c").unwrap();
        assert_eq!(
            params["format"],
            json!({"type": "image/jpeg", "quality": 0.3})
        );
        let png = screenshot_params(&json!({"format": "png", "quality": 30}), "c").unwrap();
        assert_eq!(png["format"], json!({"type": "image/png"}));
        let e = screenshot_params(&json!({"format": "webp"}), "c").unwrap_err();
        assert!(e.contains("Unsupported screenshot format"), "{e}");
    }

    #[test]
    fn conditional_screenshots_are_rejected() {
        let conditional = screenshot_params(&json!({"ifChanged": true}), "c").unwrap_err();
        assert!(
            conditional.contains("not yet supported on Firefox"),
            "{conditional}"
        );
    }

    #[test]
    fn pdf_maps_chrome_options_to_bidi_print() {
        let defaults = pdf_params(&json!({}), "ctx-1");
        assert_eq!(
            defaults,
            json!({
                "context": "ctx-1",
                "background": true,
                "orientation": "portrait",
                "shrinkToFit": true,
            })
        );
        let custom = pdf_params(
            &json!({"printBackground": false, "landscape": true, "preferCSSPageSize": true}),
            "ctx-2",
        );
        assert_eq!(custom["background"], false);
        assert_eq!(custom["orientation"], "landscape");
        assert_eq!(custom["shrinkToFit"], false);
    }

    #[test]
    fn parent_context_finds_the_direct_parent_at_any_depth() {
        let tree = json!([{"context": "top", "children": [
            {"context": "a", "children": [{"context": "b", "children": []}]},
            {"context": "c", "children": []}
        ]}]);
        assert_eq!(parent_context(&tree, "b").as_deref(), Some("a"));
        assert_eq!(parent_context(&tree, "c").as_deref(), Some("top"));
        assert_eq!(parent_context(&tree, "top"), None);
    }
}
