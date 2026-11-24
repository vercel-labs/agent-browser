use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

use std::collections::HashMap;

use super::cdp::client::CdpClient;
use super::cdp::types::*;
use super::element::RefMap;

const ANNOTATION_OVERLAY_ID: &str = "__agent_browser_annotations__";

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
    in_iframe: bool,
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

    let raw_annotations = if options.annotate {
        collect_annotations(client, session_id, ref_map, iframe_sessions).await?
    } else {
        Vec::new()
    };

    let overlay_items = filter_annotations(raw_annotations, target_rect.as_ref());
    let dom_overlay_items: Vec<&RawAnnotation> = overlay_items
        .iter()
        .filter(|item| !item.in_iframe)
        .collect();
    let has_iframe_annotations = overlay_items.iter().any(|item| item.in_iframe);

    let overlay_injected = if options.annotate && !dom_overlay_items.is_empty() {
        inject_annotation_overlay(client, session_id, &dom_overlay_items).await?;
        true
    } else {
        false
    };

    let base64 =
        capture_screenshot_base64(client, session_id, ref_map, options, iframe_sessions).await;

    if overlay_injected {
        let _ = remove_annotation_overlay(client, session_id).await;
    }

    let mut base64 = base64?;
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

    if options.annotate && has_iframe_annotations {
        let iframe_boxes: Vec<&ScreenshotAnnotation> = overlay_items
            .iter()
            .zip(annotations.iter())
            .filter(|(raw, _)| raw.in_iframe)
            .map(|(_, projected)| projected)
            .collect();
        if !iframe_boxes.is_empty() {
            let dpr = get_device_scale_factor(client, session_id).await;
            base64 = compose_annotations_on_image(
                &base64,
                &iframe_boxes,
                dpr,
                &options.format,
                options.quality,
            )?;
        }
    }

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
        quality: if options.format == "jpeg" {
            options.quality.or(Some(80))
        } else {
            None
        },
        clip: None,
        from_surface: Some(true),
        capture_beyond_viewport: capture_beyond_viewport(options),
    };

    if options.full_page {
        let metrics: Value = client
            .send_command_no_params("Page.getLayoutMetrics", Some(session_id))
            .await?;

        if let Some((width, height)) = full_page_clip_size(&metrics) {
            params.clip = Some(Viewport {
                x: 0.0,
                y: 0.0,
                width,
                height,
                scale: 1.0,
            });
        }
    } else if let Some(ref selector) = options.selector {
        if let Some(rect) =
            get_document_rect_for_selector(client, session_id, ref_map, selector, iframe_sessions)
                .await?
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

fn capture_beyond_viewport(options: &ScreenshotOptions) -> Option<bool> {
    (options.full_page || options.selector.is_some()).then_some(true)
}

/// Size of the full-page clip in CSS pixels, which is the unit
/// `Page.captureScreenshot` expects. The deprecated `contentSize` is in
/// physical pixels, so it is only a fallback.
fn full_page_clip_size(metrics: &Value) -> Option<(f64, f64)> {
    let size = metrics
        .get("cssContentSize")
        .or_else(|| metrics.get("contentSize"))?;
    let width = size.get("width").and_then(|v| v.as_f64()).unwrap_or(1280.0);
    let height = size.get("height").and_then(|v| v.as_f64()).unwrap_or(720.0);
    Some((width, height))
}

async fn collect_annotations(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Vec<RawAnnotation>, String> {
    let entries = ref_map.entries_sorted();
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let mut main_entries: Vec<(String, super::element::RefEntry, i64)> = Vec::new();
    let mut iframe_entries: Vec<(String, super::element::RefEntry, i64, String)> = Vec::new();
    for (ref_id, entry) in entries {
        let Some(bid) = entry.backend_node_id else {
            continue;
        };
        if let Some(frame_id) = entry.frame_id.as_deref() {
            let effective_session = iframe_sessions
                .get(frame_id)
                .map(String::as_str)
                .unwrap_or(session_id)
                .to_string();
            iframe_entries.push((ref_id, entry, bid, effective_session));
        } else {
            main_entries.push((ref_id, entry, bid));
        }
    }

    let mut annotations = Vec::new();

    if !main_entries.is_empty() {
        let resolve_futures: Vec<_> = main_entries
            .iter()
            .map(|(_, _, backend_node_id)| {
                client.send_command(
                    "DOM.resolveNode",
                    Some(serde_json::json!({
                        "backendNodeId": backend_node_id,
                        "objectGroup": "agent-browser-annotate"
                    })),
                    Some(session_id),
                )
            })
            .collect();

        let resolve_results = futures_util::future::join_all(resolve_futures).await;

        let mut resolved: Vec<(String, super::element::RefEntry, String)> = Vec::new();
        for (i, result) in resolve_results.into_iter().enumerate() {
            if let Ok(val) = result {
                if let Some(oid) = val
                    .get("object")
                    .and_then(|o| o.get("objectId"))
                    .and_then(|v| v.as_str())
                {
                    let (ref_id, entry, _) = &main_entries[i];
                    resolved.push((ref_id.clone(), entry.clone(), oid.to_string()));
                }
            }
        }

        if !resolved.is_empty() {
            let rect_futures: Vec<_> = resolved
                .iter()
                .map(|(_, _, object_id)| get_rect_for_object(client, session_id, object_id))
                .collect();

            let rect_results = futures_util::future::join_all(rect_futures).await;

            for (i, rect_result) in rect_results.into_iter().enumerate() {
                let rect = match rect_result {
                    Ok(Some(r)) if r.width > 0.0 && r.height > 0.0 => r,
                    _ => continue,
                };
                let (ref_id, entry, _) = &resolved[i];
                annotations.push(make_raw_annotation(ref_id, entry, rect, false));
            }
        }
    }

    if !iframe_entries.is_empty() {
        let mut session_transforms: HashMap<String, Affine2D> = HashMap::new();
        for (_, _, _, effective_session) in &iframe_entries {
            if effective_session != session_id
                && !session_transforms.contains_key(effective_session)
            {
                let transform = session_viewport_transform(
                    client,
                    session_id,
                    effective_session,
                    iframe_sessions,
                )
                .await
                .unwrap_or_else(|_| Affine2D::identity());
                session_transforms.insert(effective_session.clone(), transform);
            }
        }

        let box_futures: Vec<_> = iframe_entries
            .iter()
            .map(|(_, _, backend_node_id, effective_session)| {
                get_box_model_rect(client, effective_session, *backend_node_id)
            })
            .collect();

        let box_results = futures_util::future::join_all(box_futures).await;

        for (i, rect_result) in box_results.into_iter().enumerate() {
            let Ok(Some(mut rect)) = rect_result else {
                continue;
            };
            if rect.width <= 0.0 || rect.height <= 0.0 {
                continue;
            }
            let (ref_id, entry, _, effective_session) = &iframe_entries[i];
            if let Some(transform) = session_transforms.get(effective_session) {
                rect = transform.map_rect(&rect);
            }
            annotations.push(make_raw_annotation(ref_id, entry, rect, true));
        }
    }

    Ok(annotations)
}

fn make_raw_annotation(
    ref_id: &str,
    entry: &super::element::RefEntry,
    rect: Rect,
    in_iframe: bool,
) -> RawAnnotation {
    let number = ref_id
        .strip_prefix('e')
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);

    RawAnnotation {
        ref_id: ref_id.to_string(),
        number,
        role: entry.role.clone(),
        name: (!entry.name.is_empty()).then_some(entry.name.clone()),
        rect,
        in_iframe,
    }
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
    let mut rect =
        match get_box_model_rect_for_object(client, &effective_session_id, &object_id).await {
            Ok(Some(r)) => Some(r),
            _ => get_rect_for_object(client, &effective_session_id, &object_id).await?,
        };
    if effective_session_id != session_id {
        if let Some(r) = rect.take() {
            let transform = session_viewport_transform(
                client,
                session_id,
                &effective_session_id,
                iframe_sessions,
            )
            .await
            .unwrap_or_else(|_| Affine2D::identity());
            rect = Some(transform.map_rect(&r));
        }
    }
    Ok(rect)
}

async fn get_document_rect_for_selector(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Option<Rect>, String> {
    let Some(rect) =
        get_rect_for_selector(client, session_id, ref_map, selector, iframe_sessions).await?
    else {
        return Ok(None);
    };
    let scroll = get_scroll_offsets(client, session_id).await?;
    Ok(Some(offset_rect(rect, scroll)))
}

async fn get_box_model_rect(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
) -> Result<Option<Rect>, String> {
    let value: Value = client
        .send_command(
            "DOM.getBoxModel",
            Some(serde_json::json!({ "backendNodeId": backend_node_id })),
            Some(session_id),
        )
        .await?;
    Ok(parse_box_model_border(&value))
}

async fn get_box_model_rect_for_object(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
) -> Result<Option<Rect>, String> {
    let value: Value = client
        .send_command(
            "DOM.getBoxModel",
            Some(serde_json::json!({ "objectId": object_id })),
            Some(session_id),
        )
        .await?;
    Ok(parse_box_model_border(&value))
}

#[derive(Debug, Clone, Copy)]
struct Affine2D {
    ox: f64,
    oy: f64,
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
}

impl Affine2D {
    fn identity() -> Self {
        Self {
            ox: 0.0,
            oy: 0.0,
            ax: 1.0,
            ay: 0.0,
            bx: 0.0,
            by: 1.0,
        }
    }

    fn translation(dx: f64, dy: f64) -> Self {
        Self {
            ox: dx,
            oy: dy,
            ..Self::identity()
        }
    }

    fn from_content_quad(quad: &[f64; 8], local_w: f64, local_h: f64) -> Self {
        if local_w <= 0.0 || local_h <= 0.0 {
            return Self::translation(quad[0], quad[1]);
        }
        Self {
            ox: quad[0],
            oy: quad[1],
            ax: (quad[2] - quad[0]) / local_w,
            ay: (quad[3] - quad[1]) / local_w,
            bx: (quad[6] - quad[0]) / local_h,
            by: (quad[7] - quad[1]) / local_h,
        }
    }

    fn then(self, outer: Self) -> Self {
        Self {
            ox: outer.ox + outer.ax * self.ox + outer.bx * self.oy,
            oy: outer.oy + outer.ay * self.ox + outer.by * self.oy,
            ax: outer.ax * self.ax + outer.bx * self.ay,
            ay: outer.ay * self.ax + outer.by * self.ay,
            bx: outer.ax * self.bx + outer.bx * self.by,
            by: outer.ay * self.bx + outer.by * self.by,
        }
    }

    fn map_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.ox + self.ax * x + self.bx * y,
            self.oy + self.ay * x + self.by * y,
        )
    }

    fn map_rect(&self, rect: &Rect) -> Rect {
        let (x0, y0) = self.map_point(rect.x, rect.y);
        let (x1, y1) = self.map_point(rect.x + rect.width, rect.y);
        let (x2, y2) = self.map_point(rect.x + rect.width, rect.y + rect.height);
        let (x3, y3) = self.map_point(rect.x, rect.y + rect.height);
        quad_bounding_rect(&[x0, y0, x1, y1, x2, y2, x3, y3])
    }
}

/// Affine map from an OOPIF session's local viewport to the top-level page
/// viewport, preserving translation, CSS scale, and rotation across nested
/// OOPIF boundaries.
async fn session_viewport_transform(
    client: &CdpClient,
    page_session: &str,
    target_session: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Affine2D, String> {
    let mut current = target_session.to_string();
    let mut transform = Affine2D::identity();
    for _ in 0..=iframe_sessions.len() {
        if current == page_session {
            return Ok(transform);
        }
        let frame = iframe_sessions
            .iter()
            .find(|(_, session)| **session == current)
            .map(|(frame, _)| frame)
            .ok_or("Cannot locate OOPIF frame")?;
        let (local_w, local_h) = session_layout_viewport_size(client, &current).await;
        let mut owner = None;
        for candidate in
            std::iter::once(page_session).chain(iframe_sessions.values().map(String::as_str))
        {
            if candidate == current {
                continue;
            }
            let Ok(node) = client
                .send_command(
                    "DOM.getFrameOwner",
                    Some(serde_json::json!({ "frameId": frame })),
                    Some(candidate),
                )
                .await
            else {
                continue;
            };
            let Some(backend_id) = node["backendNodeId"].as_i64() else {
                continue;
            };
            let model: Value = client
                .send_command(
                    "DOM.getBoxModel",
                    Some(serde_json::json!({ "backendNodeId": backend_id })),
                    Some(candidate),
                )
                .await?;
            let quad = parse_quad(model.pointer("/model/content"))
                .ok_or("Missing OOPIF host content quad")?;
            let step = Affine2D::from_content_quad(&quad, local_w, local_h);
            transform = transform.then(step);
            owner = Some(candidate.to_string());
            break;
        }
        current = owner.ok_or("Cannot locate OOPIF owner session")?;
    }
    Err("Cyclic OOPIF frame ownership".to_string())
}

async fn session_layout_viewport_size(client: &CdpClient, session_id: &str) -> (f64, f64) {
    let Ok(metrics): Result<Value, _> = client
        .send_command_no_params("Page.getLayoutMetrics", Some(session_id))
        .await
    else {
        return (0.0, 0.0);
    };
    let viewport = metrics
        .get("cssLayoutViewport")
        .or_else(|| metrics.get("layoutViewport"))
        .or_else(|| metrics.get("cssVisualViewport"));
    let w = viewport
        .and_then(|v| v.get("clientWidth"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let h = viewport
        .and_then(|v| v.get("clientHeight"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    (w, h)
}

fn parse_quad(value: Option<&Value>) -> Option<[f64; 8]> {
    let quad = value?.as_array()?;
    if quad.len() != 8 {
        return None;
    }
    let mut coords = [0.0; 8];
    for (slot, number) in coords.iter_mut().zip(quad) {
        *slot = number.as_f64()?;
    }
    Some(coords)
}

fn parse_box_model_border(value: &Value) -> Option<Rect> {
    let coords = parse_quad(value.pointer("/model/border"))?;
    Some(quad_bounding_rect(&coords))
}

fn quad_bounding_rect(quad: &[f64; 8]) -> Rect {
    let min_x = quad[0].min(quad[2]).min(quad[4]).min(quad[6]);
    let max_x = quad[0].max(quad[2]).max(quad[4]).max(quad[6]);
    let min_y = quad[1].min(quad[3]).min(quad[5]).min(quad[7]);
    let max_y = quad[1].max(quad[3]).max(quad[5]).max(quad[7]);
    Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

fn offset_rect(mut rect: Rect, (scroll_x, scroll_y): (f64, f64)) -> Rect {
    rect.x += scroll_x;
    rect.y += scroll_y;
    rect
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
    annotations: &[&RawAnnotation],
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
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: "({x: window.scrollX || 0, y: window.scrollY || 0})".to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    let value = result.result.value.unwrap_or(Value::Null);
    let x = value.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = value.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    Ok((x, y))
}

async fn get_device_scale_factor(client: &CdpClient, session_id: &str) -> f64 {
    let result: Result<EvaluateResult, _> = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: "window.devicePixelRatio || 1".to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await;
    result
        .ok()
        .and_then(|r| r.result.value)
        .and_then(|v| v.as_f64())
        .filter(|&d| d.is_finite() && d > 0.0)
        .unwrap_or(1.0)
}

/// 5x7 pixel glyphs for digits 0-9 used to draw annotation badges onto captured
/// images without touching any page DOM.
const DIGIT_GLYPHS: [[u8; 7]; 10] = [
    [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E], // 0
    [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E], // 1
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F], // 2
    [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E], // 3
    [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02], // 4
    [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E], // 5
    [0x0E, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x0E], // 6
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08], // 7
    [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E], // 8
    [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C], // 9
];

fn blend_pixel(img: &mut image::RgbaImage, x: i64, y: i64, color: [u8; 4]) {
    if x < 0 || y < 0 {
        return;
    }
    let (ux, uy) = (x as u32, y as u32);
    if ux >= img.width() || uy >= img.height() {
        return;
    }
    let dst = img.get_pixel_mut(ux, uy);
    let alpha = color[3] as u32;
    let inv = 255 - alpha;
    dst[0] = ((color[0] as u32 * alpha + dst[0] as u32 * inv + 127) / 255) as u8;
    dst[1] = ((color[1] as u32 * alpha + dst[1] as u32 * inv + 127) / 255) as u8;
    dst[2] = ((color[2] as u32 * alpha + dst[2] as u32 * inv + 127) / 255) as u8;
    dst[3] = 255;
}

fn fill_rect(img: &mut image::RgbaImage, x: i64, y: i64, w: i64, h: i64, color: [u8; 4]) {
    if w <= 0 || h <= 0 {
        return;
    }
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(img.width() as i64);
    let y1 = (y + h).min(img.height() as i64);
    for py in y0..y1 {
        for px in x0..x1 {
            blend_pixel(img, px, py, color);
        }
    }
}

fn draw_annotation_box(img: &mut image::RgbaImage, ann: &ScreenshotAnnotation, dpr: f64) {
    let x = (ann.box_.x as f64 * dpr).round() as i64;
    let y = (ann.box_.y as f64 * dpr).round() as i64;
    let w = (ann.box_.width as f64 * dpr).round() as i64;
    let h = (ann.box_.height as f64 * dpr).round() as i64;
    if w <= 0 || h <= 0 {
        return;
    }

    let border = (2.0 * dpr).round().max(1.0) as i64;
    let border_color = [255, 0, 0, 204];
    fill_rect(img, x, y, w, border.min(h), border_color);
    fill_rect(
        img,
        x,
        (y + h - border).max(y),
        w,
        border.min(h),
        border_color,
    );
    fill_rect(
        img,
        x,
        y + border,
        border.min(w),
        (h - 2 * border).max(0),
        border_color,
    );
    fill_rect(
        img,
        (x + w - border).max(x),
        y + border,
        border.min(w),
        (h - 2 * border).max(0),
        border_color,
    );

    let text = ann.number.to_string();
    let glyph_scale = dpr.round().max(1.0) as i64;
    let pad_x = (4.0 * dpr).round().max(2.0) as i64;
    let pad_y = (3.0 * dpr).round().max(2.0) as i64;
    let char_w = 5 * glyph_scale;
    let char_gap = glyph_scale;
    let text_w = (text.len() as i64) * char_w + ((text.len().saturating_sub(1)) as i64) * char_gap;
    let badge_w = text_w + 2 * pad_x;
    let badge_h = 7 * glyph_scale + 2 * pad_y;
    let badge_x = x;
    let badge_y = if y < badge_h {
        y + (2.0 * dpr).round() as i64
    } else {
        y - badge_h
    };

    fill_rect(img, badge_x, badge_y, badge_w, badge_h, [255, 0, 0, 230]);

    let mut cursor_x = badge_x + pad_x;
    let cursor_y = badge_y + pad_y;
    for ch in text.bytes() {
        if let Some(digit) = (ch as char).to_digit(10) {
            let glyph = DIGIT_GLYPHS[digit as usize];
            for (row, &bits) in glyph.iter().enumerate() {
                for col in 0..5 {
                    if (bits & (1 << (4 - col))) != 0 {
                        fill_rect(
                            img,
                            cursor_x + col * glyph_scale,
                            cursor_y + (row as i64) * glyph_scale,
                            glyph_scale,
                            glyph_scale,
                            [255, 255, 255, 255],
                        );
                    }
                }
            }
        }
        cursor_x += char_w + char_gap;
    }
}

fn compose_annotations_on_image(
    base64_data: &str,
    annotations: &[&ScreenshotAnnotation],
    dpr: f64,
    format: &str,
    quality: Option<i32>,
) -> Result<String, String> {
    use base64::Engine;
    use std::io::Cursor;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .map_err(|e| format!("Failed to decode screenshot for annotation overlay: {}", e))?;
    let mut img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to parse screenshot for annotation overlay: {}", e))?
        .to_rgba8();

    for ann in annotations {
        draw_annotation_box(&mut img, ann, dpr);
    }

    let mut out = Vec::new();
    if format == "jpeg" {
        let q = quality.unwrap_or(80).clamp(1, 100) as u8;
        let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
        encoder
            .encode_image(&rgb)
            .map_err(|e| format!("Failed to encode annotated JPEG screenshot: {}", e))?;
    } else {
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|e| format!("Failed to encode annotated PNG screenshot: {}", e))?;
    }

    Ok(base64::engine::general_purpose::STANDARD.encode(out))
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
    let ext = if options.format == "jpeg" {
        "jpg"
    } else {
        "png"
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
    if let Some(home) = std::env::home_dir() {
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
    fn selector_screenshots_capture_beyond_the_initial_viewport() {
        let options = ScreenshotOptions {
            selector: Some("#below-fold".to_string()),
            ..ScreenshotOptions::default()
        };

        assert_eq!(capture_beyond_viewport(&options), Some(true));
    }

    #[test]
    fn viewport_screenshots_keep_the_default_capture_extent() {
        assert_eq!(capture_beyond_viewport(&ScreenshotOptions::default()), None);
    }

    #[test]
    fn selector_clip_uses_document_coordinates_after_scroll() {
        let rect = Rect {
            x: 10.0,
            y: 197.0,
            width: 200.0,
            height: 200.0,
        };

        let document_rect = offset_rect(rect, (0.0, 503.0));

        assert_eq!(document_rect.x, 10.0);
        assert_eq!(document_rect.y, 700.0);
        assert_eq!(document_rect.width, 200.0);
        assert_eq!(document_rect.height, 200.0);
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
                in_iframe: false,
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
                in_iframe: false,
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
            in_iframe: false,
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
            in_iframe: false,
        }];

        let projected = project_annotations(&annotations, None, Some((10.0, 1000.0)));
        assert_eq!(projected[0].box_.x, 15);
        assert_eq!(projected[0].box_.y, 1012);
    }

    #[test]
    fn full_page_clip_uses_css_pixels_on_high_dpi_displays() {
        // Page.getLayoutMetrics for a 1280x2500 CSS px page at device scale factor 2.
        let metrics = serde_json::json!({
            "contentSize": { "x": 0, "y": 0, "width": 2560, "height": 5000 },
            "cssContentSize": { "x": 0, "y": 0, "width": 1280, "height": 2500 },
        });

        assert_eq!(full_page_clip_size(&metrics), Some((1280.0, 2500.0)));
    }

    #[test]
    fn full_page_clip_falls_back_to_content_size() {
        let metrics = serde_json::json!({
            "contentSize": { "x": 0, "y": 0, "width": 1280, "height": 2500 },
        });

        assert_eq!(full_page_clip_size(&metrics), Some((1280.0, 2500.0)));
        assert_eq!(full_page_clip_size(&serde_json::json!({})), None);
    }

    #[test]
    fn parses_box_model_border_quad_including_transformed_corners() {
        let axis_aligned = serde_json::json!({
            "model": {
                "border": [143.0, 263.0, 243.0, 263.0, 243.0, 293.0, 143.0, 293.0]
            }
        });
        let rect = parse_box_model_border(&axis_aligned).unwrap();
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (143.0, 263.0, 100.0, 30.0)
        );

        // Rotated/skewed quad: bounding box spans min..max across all 4 corners.
        let transformed = serde_json::json!({
            "model": {
                "border": [100.0, 50.0, 150.0, 40.0, 160.0, 90.0, 110.0, 100.0]
            }
        });
        let rect = parse_box_model_border(&transformed).unwrap();
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (100.0, 40.0, 60.0, 60.0)
        );

        assert!(
            parse_box_model_border(&serde_json::json!({ "model": { "border": [1.0, 2.0] } }))
                .is_none()
        );
        assert!(parse_box_model_border(&serde_json::json!({})).is_none());
    }

    #[test]
    fn affine_transform_projects_scaled_and_nested_oopif_rects() {
        // 200x80 OOPIF rendered at (520, 410) with scale(2) -> 400x160 content quad.
        let quad = [520.0, 410.0, 920.0, 410.0, 920.0, 570.0, 520.0, 570.0];
        let t = Affine2D::from_content_quad(&quad, 200.0, 80.0);
        let mapped = t.map_rect(&Rect {
            x: 15.0,
            y: 10.0,
            width: 50.0,
            height: 20.0,
        });
        assert_eq!(
            (mapped.x, mapped.y, mapped.width, mapped.height),
            (550.0, 430.0, 100.0, 40.0)
        );

        // Composed with an outer translation of (10, -20).
        let nested = t.then(Affine2D::translation(10.0, -20.0));
        let mapped_nested = nested.map_rect(&Rect {
            x: 15.0,
            y: 10.0,
            width: 50.0,
            height: 20.0,
        });
        assert_eq!(
            (
                mapped_nested.x,
                mapped_nested.y,
                mapped_nested.width,
                mapped_nested.height
            ),
            (560.0, 410.0, 100.0, 40.0)
        );
    }

    #[test]
    fn composes_iframe_annotations_onto_png_at_expected_pixels() {
        use base64::Engine;
        use std::io::Cursor;

        let blank = image::RgbaImage::from_pixel(200, 120, image::Rgba([255, 255, 255, 255]));
        let mut raw = Vec::new();
        image::DynamicImage::ImageRgba8(blank)
            .write_to(&mut Cursor::new(&mut raw), image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);

        let ann = ScreenshotAnnotation {
            ref_id: "e12".to_string(),
            number: 12,
            role: "button".to_string(),
            name: Some("OopifScaledBtn".to_string()),
            box_: AnnotationBox {
                x: 40,
                y: 30,
                width: 60,
                height: 24,
            },
        };

        let out_b64 = compose_annotations_on_image(&b64, &[&ann], 1.0, "png", None).unwrap();
        let out_bytes = base64::engine::general_purpose::STANDARD
            .decode(out_b64)
            .unwrap();
        let img = image::load_from_memory(&out_bytes).unwrap().to_rgba8();

        // Top-left and bottom-right border pixels are red-blended.
        let tl = img.get_pixel(40, 30);
        let br = img.get_pixel(99, 53);
        assert!(tl[0] > 200 && tl[1] < 80 && tl[2] < 80, "tl={tl:?}");
        assert!(br[0] > 200 && br[1] < 80 && br[2] < 80, "br={br:?}");
        // Interior stays white.
        let center = img.get_pixel(70, 42);
        assert_eq!(*center, image::Rgba([255, 255, 255, 255]));
    }
}
