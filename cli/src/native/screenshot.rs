use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

use std::collections::HashMap;

use super::cdp::client::CdpClient;
use super::cdp::types::*;
use super::element::RefMap;

const ANNOTATION_OVERLAY_ID: &str = "__agent_browser_annotations__";
const ANNOTATE_OBJECT_GROUP: &str = "agent-browser-annotate";

#[derive(Debug, Clone)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone)]
struct RawAnnotation {
    ref_id: String,
    number: u64,
    role: String,
    name: Option<String>,
    rect: Rect,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnnotationBox {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone)]
pub struct ScreenshotAnnotation {
    pub ref_id: String,
    pub number: u64,
    pub role: String,
    pub name: Option<String>,
    pub box_: AnnotationBox,
}

#[derive(Debug, Clone)]
pub struct ScreenshotResult {
    pub base64: String,
    pub annotations: Vec<ScreenshotAnnotation>,
}

#[derive(Debug, Clone)]
pub struct ScreenshotOptions {
    pub selector: Option<String>,
    pub path: Option<String>,
    pub full_page: bool,
    pub format: String,
    pub quality: Option<i32>,
    pub annotate: bool,
    pub output_dir: Option<String>,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            selector: None,
            path: None,
            full_page: false,
            format: "png".to_string(),
            quality: None,
            annotate: false,
            output_dir: None,
        }
    }
}

impl Serialize for ScreenshotAnnotation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("ScreenshotAnnotation", 5)?;
        state.serialize_field("ref", &self.ref_id)?;
        state.serialize_field("number", &self.number)?;
        state.serialize_field("role", &self.role)?;
        if let Some(name) = &self.name {
            state.serialize_field("name", name)?;
        }
        state.serialize_field("box", &self.box_)?;
        state.end()
    }
}

/// Captures screenshot bytes and optional annotations without writing a file.
pub async fn take_screenshot(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    options: &ScreenshotOptions,
    iframe_sessions: &HashMap<String, String>,
    viewport_refs: Option<Vec<super::snapshot::ViewportRef>>,
) -> Result<ScreenshotResult, String> {
    let target_rect = if options.annotate {
        match options.selector.as_deref() {
            Some(selector) => {
                get_rect_for_selector(client, session_id, ref_map, selector, iframe_sessions)
                    .await?
            }
            None => None,
        }
    } else {
        None
    };

    let raw_annotations = match viewport_refs {
        Some(refs) => refs
            .into_iter()
            .map(|r| RawAnnotation {
                number: r
                    .ref_id
                    .strip_prefix('e')
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0),
                ref_id: r.ref_id,
                role: r.role,
                name: (!r.name.is_empty()).then_some(r.name),
                rect: Rect {
                    x: r.x,
                    y: r.y,
                    width: r.width,
                    height: r.height,
                },
            })
            .collect(),
        None if options.annotate => collect_annotations(client, session_id, ref_map).await?,
        None => Vec::new(),
    };

    let overlay_items = filter_annotations(raw_annotations, target_rect.as_ref());
    let overlay_injected = if options.annotate && !overlay_items.is_empty() {
        inject_annotation_overlay(client, session_id, &overlay_items).await?;
        true
    } else {
        false
    };

    let base64 =
        capture_screenshot_base64(client, session_id, ref_map, options, iframe_sessions).await;

    if overlay_injected {
        let _ = remove_annotation_overlay(client, session_id).await;
    }

    let base64 = base64?;
    let annotations = if options.annotate {
        let scroll = if options.full_page {
            Some(get_scroll_offsets(client, session_id).await?)
        } else {
            None
        };
        project_annotations(&overlay_items, target_rect.as_ref(), scroll)
    } else {
        Vec::new()
    };

    Ok(ScreenshotResult {
        base64,
        annotations,
    })
}

async fn capture_screenshot_base64(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    options: &ScreenshotOptions,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let mut params = CaptureScreenshotParams {
        format: Some(options.format.clone()),
        quality: if matches!(options.format.as_str(), "jpeg" | "webp") {
            options.quality.or(Some(80))
        } else {
            None
        },
        clip: None,
        from_surface: Some(true),
        capture_beyond_viewport: if options.full_page { Some(true) } else { None },
        optimize_for_speed: None,
    };

    if options.full_page {
        let metrics: Value = client
            .send_command_no_params("Page.getLayoutMetrics", Some(session_id))
            .await?;

        let content_size = metrics
            .get("contentSize")
            .or_else(|| metrics.get("cssContentSize"));
        if let Some(size) = content_size {
            let width = size.get("width").and_then(|v| v.as_f64()).unwrap_or(1280.0);
            let height = size.get("height").and_then(|v| v.as_f64()).unwrap_or(720.0);

            params.clip = Some(Viewport {
                x: 0.0,
                y: 0.0,
                width,
                height,
                scale: 1.0,
            });
            if options.format == "png" && width * height > FAST_PNG_MIN_PIXELS {
                params.optimize_for_speed = Some(true);
            }
        }
    } else if let Some(ref selector) = options.selector {
        if let Some(rect) =
            get_rect_for_selector(client, session_id, ref_map, selector, iframe_sessions).await?
        {
            params.clip = Some(Viewport {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                scale: 1.0,
            });
        }
    }

    let result: CaptureScreenshotResult = client
        .send_command_typed("Page.captureScreenshot", &params, Some(session_id))
        .await?;

    Ok(result.data)
}

async fn collect_annotations(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
) -> Result<Vec<RawAnnotation>, String> {
    let entries = ref_map.entries_sorted();
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    // Collect entries that have backend_node_ids for batch resolution.
    let with_backend_ids: Vec<(String, super::element::RefEntry, i64)> = entries
        .iter()
        .filter_map(|(ref_id, entry)| {
            entry
                .backend_node_id
                .map(|bid| (ref_id.clone(), entry.clone(), bid))
        })
        .collect();

    if with_backend_ids.is_empty() {
        return Ok(Vec::new());
    }

    let bounds = layout_bounds(client, session_id, with_backend_ids.len()).await;
    let mut annotations = Vec::new();
    let mut with_backend_ids = with_backend_ids;
    with_backend_ids.retain(|(ref_id, entry, bid)| {
        match bounds.get(bid).filter(|_| entry.frame_id.is_none()) {
            Some(rect) => {
                if rect.width > 0.0 && rect.height > 0.0 {
                    annotations.push(raw_annotation(ref_id, entry, rect.clone()));
                }
                false
            }
            None => true,
        }
    });
    if with_backend_ids.is_empty() {
        return Ok(annotations);
    }

    // Batch-resolve all backend_node_ids to object IDs using concurrent CDP calls.
    let resolve_futures: Vec<_> = with_backend_ids
        .iter()
        .map(|(_, _, backend_node_id)| {
            client.send_command(
                "DOM.resolveNode",
                Some(serde_json::json!({
                    "backendNodeId": backend_node_id,
                    "objectGroup": ANNOTATE_OBJECT_GROUP
                })),
                Some(session_id),
            )
        })
        .collect();

    let resolve_results = futures_util::future::join_all(resolve_futures).await;

    // Collect resolved object IDs paired with their ref info.
    let mut resolved: Vec<(String, super::element::RefEntry, String)> = Vec::new();
    for (i, result) in resolve_results.into_iter().enumerate() {
        if let Ok(val) = result {
            if let Some(oid) = val
                .get("object")
                .and_then(|o| o.get("objectId"))
                .and_then(|v| v.as_str())
            {
                let (ref_id, entry, _) = &with_backend_ids[i];
                resolved.push((ref_id.clone(), entry.clone(), oid.to_string()));
            }
        }
    }

    // Batch-get bounding rects for all resolved elements using concurrent CDP calls.
    let rect_futures: Vec<_> = resolved
        .iter()
        .map(|(_, _, object_id)| get_rect_for_object(client, session_id, object_id))
        .collect();

    let rect_results = futures_util::future::join_all(rect_futures).await;
    let _ = client
        .send_command(
            "Runtime.releaseObjectGroup",
            Some(serde_json::json!({ "objectGroup": ANNOTATE_OBJECT_GROUP })),
            Some(session_id),
        )
        .await;

    for (i, rect_result) in rect_results.into_iter().enumerate() {
        let rect = match rect_result {
            Ok(Some(r)) if r.width > 0.0 && r.height > 0.0 => r,
            _ => continue,
        };

        let (ref_id, entry, _) = &resolved[i];
        annotations.push(raw_annotation(ref_id, entry, rect));
    }

    Ok(annotations)
}

fn raw_annotation(ref_id: &str, entry: &super::element::RefEntry, rect: Rect) -> RawAnnotation {
    RawAnnotation {
        ref_id: ref_id.to_string(),
        number: ref_id
            .strip_prefix('e')
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0),
        role: entry.role.clone(),
        name: (!entry.name.is_empty()).then_some(entry.name.clone()),
        rect,
    }
}

/// Reads every main-document layout box in one `DOMSnapshot` call, keyed by backend node id, when that is cheaper
/// than per-ref calls. Only boxes that match `getBoundingClientRect` exactly are returned; nodes under a transform,
/// inside SVG or skipped content, or with more than one layout object are left to the per-ref path.
async fn layout_bounds(client: &CdpClient, session_id: &str, refs: usize) -> HashMap<i64, Rect> {
    if refs < LAYOUT_BOUNDS_MIN_REFS {
        return HashMap::new();
    }
    let elements = client
        .evaluate_value(
            "document.getElementsByTagName('*').length",
            Some(session_id),
        )
        .await
        .ok()
        .and_then(|v| v.as_f64());
    if elements.is_none_or(|n| (refs as f64) * 4.0 < n) {
        return HashMap::new();
    }
    let Ok(snap) = client
        .send_command(
            "DOMSnapshot.captureSnapshot",
            Some(serde_json::json!({
                "computedStyles": ["transform", "translate", "rotate", "scale", "offset-path", "content-visibility"]
            })),
            Some(session_id),
        )
        .await
    else {
        return HashMap::new();
    };
    let strings = snap["strings"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let text = |v: &Value| {
        v.as_u64()
            .and_then(|i| strings.get(i as usize))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    let doc = &snap["documents"][0];
    let ints = |v: &Value| -> Vec<i64> {
        v.as_array()
            .map(|a| a.iter().map(|x| x.as_i64().unwrap_or(-1)).collect())
            .unwrap_or_default()
    };
    let parents = ints(&doc["nodes"]["parentIndex"]);
    let backend = ints(&doc["nodes"]["backendNodeId"]);
    let names = doc["nodes"]["nodeName"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let layout_nodes = ints(&doc["layout"]["nodeIndex"]);
    let bounds = doc["layout"]["bounds"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let styles = doc["layout"]["styles"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut layout_of: Vec<Option<usize>> = vec![None; backend.len()];
    let mut unsafe_node: Vec<bool> = names
        .iter()
        .map(|n| text(n).eq_ignore_ascii_case("svg"))
        .collect();
    unsafe_node.resize(backend.len(), true);
    for (li, &ni) in layout_nodes.iter().enumerate() {
        let Some(ni) = usize::try_from(ni).ok().filter(|&i| i < backend.len()) else {
            continue;
        };
        let style: Vec<&str> = styles
            .get(li)
            .and_then(Value::as_array)
            .map(|s| s.iter().map(text).collect())
            .unwrap_or_default();
        let transformed = !style.is_empty()
            && (style.len() != 6
                || style[..5].iter().any(|v| *v != "none")
                || !matches!(style[5], "visible" | ""));
        if layout_of[ni].is_some() || transformed {
            unsafe_node[ni] = true;
        }
        layout_of[ni] = Some(li);
    }
    for i in 0..backend.len() {
        match parents.get(i) {
            Some(&p) if p < 0 => {}
            Some(&p) if (p as usize) < i => unsafe_node[i] |= unsafe_node[p as usize],
            _ => unsafe_node[i] = true,
        }
    }

    let (Some(scroll_x), Some(scroll_y)) =
        (doc["scrollOffsetX"].as_f64(), doc["scrollOffsetY"].as_f64())
    else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for (i, li) in layout_of.into_iter().enumerate() {
        let (Some(b), false) = (li.and_then(|li| bounds.get(li)), unsafe_node[i]) else {
            continue;
        };
        if let (Some(x), Some(y), Some(width), Some(height)) =
            (b[0].as_f64(), b[1].as_f64(), b[2].as_f64(), b[3].as_f64())
        {
            map.insert(
                backend[i],
                Rect {
                    x: x - scroll_x,
                    y: y - scroll_y,
                    width,
                    height,
                },
            );
        }
    }
    map
}

async fn get_rect_for_selector(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Option<Rect>, String> {
    let (object_id, effective_session_id) = super::element::resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector,
        iframe_sessions,
    )
    .await?;
    get_rect_for_object(client, &effective_session_id, &object_id).await
}

async fn get_rect_for_object(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
) -> Result<Option<Rect>, String> {
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    const rect = this.getBoundingClientRect();
                    return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
                }"#
                .to_string(),
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    Ok(result.result.value.as_ref().and_then(parse_rect))
}

fn parse_rect(value: &Value) -> Option<Rect> {
    Some(Rect {
        x: value.get("x")?.as_f64()?,
        y: value.get("y")?.as_f64()?,
        width: value.get("width")?.as_f64()?,
        height: value.get("height")?.as_f64()?,
    })
}

fn filter_annotations(
    annotations: Vec<RawAnnotation>,
    target_rect: Option<&Rect>,
) -> Vec<RawAnnotation> {
    let mut items = annotations
        .into_iter()
        .filter(|annotation| match target_rect {
            Some(target) => overlaps(&annotation.rect, target),
            None => true,
        })
        .collect::<Vec<_>>();

    items.sort_by_key(|annotation| annotation.number);
    items
}

/// Pages with fewer refs than this read boxes with per-ref calls, which are cheaper than a full layout snapshot.
const LAYOUT_BOUNDS_MIN_REFS: usize = 256;

/// Full-page PNGs larger than this many CSS pixels use Chrome's faster lossless encoder; smaller ones keep the compact default.
const FAST_PNG_MIN_PIXELS: f64 = 16_000_000.0;

fn overlaps(left: &Rect, right: &Rect) -> bool {
    let left_x2 = left.x + left.width;
    let left_y2 = left.y + left.height;
    let right_x2 = right.x + right.width;
    let right_y2 = right.y + right.height;

    left.x < right_x2 && left_x2 > right.x && left.y < right_y2 && left_y2 > right.y
}

async fn inject_annotation_overlay(
    client: &CdpClient,
    session_id: &str,
    annotations: &[RawAnnotation],
) -> Result<(), String> {
    let overlay_data = annotations
        .iter()
        .map(|annotation| {
            serde_json::json!({
                "number": annotation.number,
                "x": round(annotation.rect.x),
                "y": round(annotation.rect.y),
                "width": round(annotation.rect.width),
                "height": round(annotation.rect.height),
            })
        })
        .collect::<Vec<_>>();

    let expression = format!(
        r#"(() => {{
            var items = {items};
            var id = {overlay_id};
            var existing = document.getElementById(id);
            if (existing) existing.remove();
            var sx = window.scrollX || 0;
            var sy = window.scrollY || 0;
            var c = document.createElement('div');
            c.id = id;
            c.style.cssText = 'position:absolute;top:0;left:0;width:0;height:0;pointer-events:none;z-index:2147483647;';
            for (var i = 0; i < items.length; i++) {{
                var it = items[i];
                var dx = it.x + sx;
                var dy = it.y + sy;
                var b = document.createElement('div');
                b.style.cssText = 'position:absolute;left:' + dx + 'px;top:' + dy + 'px;width:' + it.width + 'px;height:' + it.height + 'px;border:2px solid rgba(255,0,0,0.8);box-sizing:border-box;pointer-events:none;';
                var l = document.createElement('div');
                l.textContent = String(it.number);
                var labelTop = dy < 14 ? '2px' : '-14px';
                l.style.cssText = 'position:absolute;top:' + labelTop + ';left:-2px;background:rgba(255,0,0,0.9);color:#fff;font:bold 11px/14px monospace;padding:0 4px;border-radius:2px;white-space:nowrap;';
                b.appendChild(l);
                c.appendChild(b);
            }}
            document.documentElement.appendChild(c);
            return true;
        }})()"#,
        items = serde_json::to_string(&overlay_data).unwrap_or_else(|_| "[]".to_string()),
        overlay_id =
            serde_json::to_string(ANNOTATION_OVERLAY_ID).unwrap_or_else(|_| "\"\"".to_string()),
    );

    let _: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    Ok(())
}

async fn remove_annotation_overlay(client: &CdpClient, session_id: &str) -> Result<(), String> {
    let expression = format!(
        r#"(() => {{
            var el = document.getElementById({overlay_id});
            if (el) el.remove();
            return true;
        }})()"#,
        overlay_id =
            serde_json::to_string(ANNOTATION_OVERLAY_ID).unwrap_or_else(|_| "\"\"".to_string()),
    );

    let _: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    Ok(())
}

async fn get_scroll_offsets(client: &CdpClient, session_id: &str) -> Result<(f64, f64), String> {
    let value = client
        .evaluate_value(
            "({x: window.scrollX || 0, y: window.scrollY || 0})",
            Some(session_id),
        )
        .await?;
    let x = value.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = value.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    Ok((x, y))
}

fn project_annotations(
    annotations: &[RawAnnotation],
    target_rect: Option<&Rect>,
    scroll: Option<(f64, f64)>,
) -> Vec<ScreenshotAnnotation> {
    annotations
        .iter()
        .map(|annotation| {
            let rect = if let Some(target) = target_rect {
                Rect {
                    x: annotation.rect.x - target.x,
                    y: annotation.rect.y - target.y,
                    width: annotation.rect.width,
                    height: annotation.rect.height,
                }
            } else if let Some((scroll_x, scroll_y)) = scroll {
                Rect {
                    x: annotation.rect.x + scroll_x,
                    y: annotation.rect.y + scroll_y,
                    width: annotation.rect.width,
                    height: annotation.rect.height,
                }
            } else {
                annotation.rect.clone()
            };

            ScreenshotAnnotation {
                ref_id: annotation.ref_id.clone(),
                number: annotation.number,
                role: annotation.role.clone(),
                name: annotation.name.clone(),
                box_: AnnotationBox {
                    x: round(rect.x),
                    y: round(rect.y),
                    width: round(rect.width),
                    height: round(rect.height),
                },
            }
        })
        .collect()
}

/// Writes a captured image only after the caller decides it is needed.
pub fn save_screenshot(base64_data: &str, options: &ScreenshotOptions) -> Result<String, String> {
    let ext = match options.format.as_str() {
        "jpeg" => "jpg",
        "webp" => "webp",
        _ => "png",
    };
    let explicit_path = options.path.as_deref();
    let output_dir = options.output_dir.as_deref();
    let save_path = match explicit_path {
        Some(path) => path.to_string(),
        None => {
            let dir = match output_dir {
                Some(d) => PathBuf::from(d),
                None => get_screenshot_dir(),
            };
            let _ = std::fs::create_dir_all(&dir);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let name = format!("screenshot-{}.{}", timestamp, ext);
            dir.join(name).to_string_lossy().to_string()
        }
    };

    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, base64_data)
        .map_err(|e| format!("Failed to decode screenshot: {}", e))?;

    std::fs::write(&save_path, &bytes)
        .map_err(|e| format!("Failed to save screenshot to {}: {}", save_path, e))?;

    Ok(save_path)
}

fn round(value: f64) -> i64 {
    value.round() as i64
}

fn get_screenshot_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".agent-browser").join("tmp").join("screenshots")
    } else {
        std::env::temp_dir()
            .join("agent-browser")
            .join("screenshots")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_screenshot_names_webp_files() {
        let dir = std::env::temp_dir().join(format!("ab-webp-name-{}", std::process::id()));
        let options = ScreenshotOptions {
            format: "webp".to_string(),
            output_dir: Some(dir.to_string_lossy().to_string()),
            ..ScreenshotOptions::default()
        };
        let path = save_screenshot("UklGRg==", &options).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(path.ends_with(".webp"), "got {path}");
    }

    #[test]
    fn filters_annotations_to_target_overlap() {
        let annotations = vec![
            RawAnnotation {
                ref_id: "e1".to_string(),
                number: 1,
                role: "button".to_string(),
                name: Some("Inside".to_string()),
                rect: Rect {
                    x: 10.0,
                    y: 10.0,
                    width: 50.0,
                    height: 20.0,
                },
            },
            RawAnnotation {
                ref_id: "e2".to_string(),
                number: 2,
                role: "button".to_string(),
                name: Some("Outside".to_string()),
                rect: Rect {
                    x: 200.0,
                    y: 200.0,
                    width: 40.0,
                    height: 20.0,
                },
            },
        ];

        let target = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };

        let filtered = filter_annotations(annotations, Some(&target));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].ref_id, "e1");
    }

    #[test]
    fn projects_selector_annotations_relative_to_target() {
        let annotations = vec![RawAnnotation {
            ref_id: "e1".to_string(),
            number: 1,
            role: "button".to_string(),
            name: Some("Inside".to_string()),
            rect: Rect {
                x: 25.0,
                y: 35.0,
                width: 40.0,
                height: 20.0,
            },
        }];

        let target = Rect {
            x: 10.0,
            y: 15.0,
            width: 100.0,
            height: 100.0,
        };

        let projected = project_annotations(&annotations, Some(&target), None);
        assert_eq!(projected[0].box_.x, 15);
        assert_eq!(projected[0].box_.y, 20);
    }

    #[test]
    fn projects_full_page_annotations_to_document_space() {
        let annotations = vec![RawAnnotation {
            ref_id: "e1".to_string(),
            number: 1,
            role: "button".to_string(),
            name: Some("Bottom".to_string()),
            rect: Rect {
                x: 5.0,
                y: 12.0,
                width: 40.0,
                height: 20.0,
            },
        }];

        let projected = project_annotations(&annotations, None, Some((10.0, 1000.0)));
        assert_eq!(projected[0].box_.x, 15);
        assert_eq!(projected[0].box_.y, 1012);
    }
}
