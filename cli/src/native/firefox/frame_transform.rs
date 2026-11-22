use serde_json::{json, Value};

use super::bidi::SANDBOX;
use super::session::FirefoxSession;
use crate::native::element::BLOCKER_AT_JS;

/// One frame's mapping from its document's viewport to its parent's viewport: where the child's
/// viewport starts in the parent, and how many parent CSS pixels one child CSS pixel covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FrameTransform {
    origin: (f64, f64),
    scale: (f64, f64),
}

impl FrameTransform {
    /// Refuses a scale that cannot be inverted, so a collapsed or mirrored frame never maps a point.
    pub(super) fn new(origin: (f64, f64), scale: (f64, f64)) -> Result<Self, String> {
        if !finite(origin) || !finite(scale) || scale.0 <= 0.0 || scale.1 <= 0.0 {
            return Err("Firefox reported a frame that cannot be mapped".to_string());
        }
        Ok(Self { origin, scale })
    }

    /// The point's position in the parent's viewport.
    pub(super) fn to_parent(self, point: (f64, f64)) -> (f64, f64) {
        (
            self.origin.0 + point.0 * self.scale.0,
            self.origin.1 + point.1 * self.scale.1,
        )
    }

    /// The point's position back inside the frame, the inverse of `to_parent`.
    pub(super) fn to_child(self, point: (f64, f64)) -> (f64, f64) {
        (
            (point.0 - self.origin.0) / self.scale.0,
            (point.1 - self.origin.1) / self.scale.1,
        )
    }

    /// A box in the frame's viewport as a box in the parent's, position and size alike.
    pub(super) fn to_parent_rect(self, rect: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
        let (x, y) = self.to_parent((rect.0, rect.1));
        (x, y, rect.2 * self.scale.0, rect.3 * self.scale.1)
    }
}

/// One hop out of a frame: its transform, and the parent viewport that transform belongs to.
#[derive(Debug, Clone, Copy)]
pub(super) struct FrameHop {
    pub transform: FrameTransform,
    pub parent_viewport: (f64, f64),
}

fn finite(pair: (f64, f64)) -> bool {
    pair.0.is_finite() && pair.1.is_finite()
}

fn sane_viewport(viewport: (f64, f64)) -> bool {
    finite(viewport) && viewport.0 > 0.0 && viewport.1 > 0.0
}

pub(super) const TRANSFORM_JS: &str = r#"function frameTransform(frame, viewport) {
    const connected = frameConnected(frame);
    const refused = frameAxes(frame);
    if (refused) return { error: refused, connected: connected };
    if (!connected) return { error: 'the frame is no longer in the page', connected: false };
    if (!(frame.offsetWidth > 0) || !(frame.offsetHeight > 0)) return { error: 'the frame has no laid out box', connected: connected };
    const style = getComputedStyle(frame);
    const number = (value) => { const parsed = parseFloat(value); return isFinite(parsed) ? parsed : 0; };
    const borderLeft = number(style.borderLeftWidth) || frame.clientLeft;
    const borderTop = number(style.borderTopWidth) || frame.clientTop;
    const padLeft = number(style.paddingLeft);
    const padRight = number(style.paddingRight);
    const padTop = number(style.paddingTop);
    const padBottom = number(style.paddingBottom);
    const contentWidth = Math.max(0, frame.clientWidth - padLeft - padRight);
    const contentHeight = Math.max(0, frame.clientHeight - padTop - padBottom);
    if (!(contentWidth > 0) || !(contentHeight > 0)) return { error: 'the frame has no content box', connected: connected };
    if (!viewport || !(viewport.width > 0) || !(viewport.height > 0)) return { error: 'the frame document reported no viewport', connected: connected };
    const rect = frame.getBoundingClientRect();
    const borderScaleX = rect.width / frame.offsetWidth;
    const borderScaleY = rect.height / frame.offsetHeight;
    const scaleX = contentWidth * borderScaleX / viewport.width;
    const scaleY = contentHeight * borderScaleY / viewport.height;
    if (!(scaleX > 0) || !(scaleY > 0)) return { error: 'the frame cannot be mapped', connected: connected };
    return {
        originX: rect.x + (borderLeft + padLeft) * borderScaleX,
        originY: rect.y + (borderTop + padTop) * borderScaleY,
        scaleX: scaleX,
        scaleY: scaleY,
        width: contentWidth * borderScaleX,
        height: contentHeight * borderScaleY,
        connected: connected,
        viewport: { width: innerWidth, height: innerHeight },
    };
}

function frameAxes(frame) {
    let node = frame;
    while (node) {
        if (node.nodeType === 1) {
            const view = node.ownerDocument && node.ownerDocument.defaultView;
            if (view) {
                const style = view.getComputedStyle(node);
                if (style.perspective && style.perspective !== 'none') return 'the frame is drawn with a perspective transform';
                const matrix = frameMatrix(style.transform);
                if (!matrix) return 'the frame is drawn with a 3D transform';
                if (!(matrix[0] > 0) || !(matrix[3] > 0) || Math.abs(matrix[1]) > 1e-6 || Math.abs(matrix[2]) > 1e-6) {
                    return 'the frame is rotated, skewed or mirrored';
                }
                const rotation = frameRotation(style.rotate);
                if (rotation) return rotation;
                const scaled = frameScale(style.scale);
                if (scaled) return scaled;
            }
        }
        node = node.parentNode || node.host;
    }
    return null;
}

function frameRotation(rotate) {
    if (!rotate || rotate === 'none') return null;
    const turns = rotate.trim().split(/\s+/).map((part) => {
        const value = parseFloat(part);
        if (!isFinite(value)) return NaN;
        if (part.endsWith('turn')) return value * 360;
        if (part.endsWith('rad')) return value * 180 / Math.PI;
        if (part.endsWith('grad')) return value * 0.9;
        return value;
    });
    if (turns.some((turn) => !isFinite(turn))) return 'the frame is drawn with a rotation that cannot be read';
    return turns.every((turn) => Math.abs(turn % 360) < 1e-6) ? null : 'the frame is rotated, skewed or mirrored';
}

function frameScale(scale) {
    if (!scale || scale === 'none') return null;
    const axes = scale.trim().split(/\s+/).map(Number);
    if (!axes.every((axis) => isFinite(axis))) return 'the frame is drawn with a scale that cannot be read';
    return axes.every((axis) => axis > 0) ? null : 'the frame is rotated, skewed or mirrored';
}

function frameConnected(frame) {
    let node = frame;
    while (node) {
        if (node.isConnected === false) return false;
        node = node.parentNode || node.host;
    }
    return true;
}

function frameMatrix(transform) {
    if (!transform || transform === 'none') return [1, 0, 0, 1];
    const values = transform.slice(transform.indexOf('(') + 1, -1).split(',').map(Number);
    if (values.some((value) => !isFinite(value))) return null;
    if (transform.startsWith('matrix3d(')) {
        if (values.length !== 16) return null;
        const flat = [2, 3, 6, 7, 8, 9, 11, 12, 13, 14].every((index) => values[index] === 0);
        if (!flat || values[10] !== 1 || values[15] !== 1) return null;
        return [values[0], values[1], values[4], values[5]];
    }
    if (values.length !== 6) return null;
    return [values[0], values[1], values[2], values[3]];
}"#;

pub(super) const SETTLE_JS: &str = r#"async function frameSettle(measure) {
    let current = measure();
    for (let round = 0; round < 5; round++) {
        await new Promise((resolve) => {
            let done = false;
            const once = () => { if (!done) { done = true; resolve(); } };
            if (typeof requestAnimationFrame === 'function') {
                requestAnimationFrame(() => requestAnimationFrame(once));
            } else if (typeof setTimeout === 'function') {
                setTimeout(once, 32);
            } else {
                once();
            }
            if (typeof setTimeout === 'function') setTimeout(once, 100);
        });
        const next = measure();
        const steady = frameSteady(current, next);
        current = next;
        if (steady) return { value: current, stable: true };
    }
    return { value: current, stable: false };
}

function frameSteady(before, after) {
    if (!before || !after || before.error || after.error) return false;
    if (before.connected === false || after.connected === false) return false;
    const close = (one, other) => Math.abs((one || 0) - (other || 0)) <= 0.5;
    return close(before.stableX ?? before.x, after.stableX ?? after.x) &&
        close(before.stableY ?? before.y, after.stableY ?? after.y) &&
        close(before.width, after.width) && close(before.height, after.height);
}"#;

pub(super) const VIEWPORT_JS: &str =
    "function() { return { width: innerWidth, height: innerHeight }; }";

/// Maps one point out of a frame's viewport into its parent's, scrolling the frame into view and
/// waiting for the page to settle first. It runs in the parent's sandbox with `this` bound to the
/// browser's frame handle.
pub(super) fn hop_js(point: (f64, f64), viewport: (f64, f64)) -> String {
    format!(
        r#"async function() {{
    {TRANSFORM_JS}
    {SETTLE_JS}
    const point = {{ x: {point_x}, y: {point_y} }};
    const child = {{ width: {viewport_x}, height: {viewport_y} }};
    const measure = () => {{
        const frame = frameTransform(this, child);
        if (frame.error) return frame;
        return {{
            x: frame.originX + point.x * frame.scaleX,
            y: frame.originY + point.y * frame.scaleY,
            width: frame.width,
            height: frame.height,
            connected: frame.connected,
            viewport: frame.viewport,
        }};
    }};
    let mapped = measure();
    if (mapped.error) return mapped;
    let scrolled = false;
    if (mapped.x < 0 || mapped.y < 0 || mapped.x >= innerWidth || mapped.y >= innerHeight) {{
        this.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
        mapped = measure();
        if (mapped.error) return mapped;
        scrollBy({{ left: Math.min(0, mapped.x) + Math.max(0, mapped.x - innerWidth + 1),
                   top: Math.min(0, mapped.y) + Math.max(0, mapped.y - innerHeight + 1),
                   behavior: 'instant' }});
        scrolled = true;
        const settled = await frameSettle(measure);
        if (!settled.stable) return {{ error: 'the page kept moving while the frame was scrolled into view' }};
        mapped = settled.value;
        if (mapped.error) return mapped;
    }}
    return {{
        x: mapped.x,
        y: mapped.y,
        scrolled: scrolled,
        viewport: mapped.viewport,
        blocker: ({BLOCKER_AT_JS})(document, this, mapped.x, mapped.y),
    }};
}}"#,
        point_x = point.0,
        point_y = point.1,
        viewport_x = viewport.0,
        viewport_y = viewport.1,
    )
}

/// The error a caller reports when a frame's geometry is refused or never settles.
pub(super) fn hop_error(label: &str, error: &str) -> String {
    format!(
        "Element '{label}' cannot be clicked safely: {error}, so the click point could not be placed in its frame. Open the frame's own URL to interact with it."
    )
}

/// The frame tree the browser reports for the active tab.
pub(super) async fn tree(ff: &FirefoxSession) -> Result<Value, String> {
    ff.bidi
        .send(
            "browsingContext.getTree",
            json!({ "root": ff.contexts.active_tab() }),
        )
        .await
}

/// Every hop from a context tree root down to `context`, the outermost hop first.
pub(super) fn path(tree: &Value, context: &str) -> Option<Vec<(String, String)>> {
    let mut hops = Vec::new();
    let found = tree["contexts"]
        .as_array()
        .is_some_and(|roots| roots.iter().any(|root| descend(root, context, &mut hops)));
    found.then_some(hops)
}

fn descend(node: &Value, context: &str, hops: &mut Vec<(String, String)>) -> bool {
    let Some(parent) = node["context"].as_str() else {
        return false;
    };
    if parent == context {
        return true;
    }
    for child in node["children"].as_array().into_iter().flatten() {
        let Some(id) = child["context"].as_str() else {
            continue;
        };
        hops.push((parent.to_string(), id.to_string()));
        if descend(child, context, hops) {
            return true;
        }
        hops.pop();
    }
    false
}

/// The hops from `to` down to `from`, or None when `to` does not hold `from`.
pub(super) fn chain(tree: &Value, from: &str, to: &str) -> Option<Vec<(String, String)>> {
    let hops = path(tree, from)?;
    let start = hops.iter().position(|(parent, _)| parent == to)?;
    Some(hops[start..].to_vec())
}

/// The frame element that hosts `child` inside `parent`, as the browser reports it. Exactly one
/// element must match, so a page cannot substitute another frame for the one it was asked about.
pub(super) async fn verified_frame(
    ff: &FirefoxSession,
    parent: &str,
    child: &str,
) -> Result<Value, String> {
    let frames = super::query::locate_nodes(
        &ff.bidi,
        parent,
        json!({ "type": "context", "value": { "context": child } }),
        None,
    )
    .await?;
    let [frame] = frames.as_slice() else {
        return Err("Firefox could not verify the frame".to_string());
    };
    if frame["sharedId"].as_str().is_none() {
        return Err("Firefox could not verify the frame".to_string());
    }
    Ok(json!({ "sharedId": frame["sharedId"] }))
}

/// The viewport the document in `context` reports.
pub(super) async fn viewport(ff: &FirefoxSession, context: &str) -> Result<(f64, f64), String> {
    let reply = ff
        .bidi
        .call_function_in_sandbox(context, SANDBOX, VIEWPORT_JS, Vec::new())
        .await?;
    match (reply["width"].as_f64(), reply["height"].as_f64()) {
        (Some(width), Some(height)) if sane_viewport((width, height)) => Ok((width, height)),
        _ => Err(format!(
            "Firefox did not report the viewport of context {context}"
        )),
    }
}

/// Reads one frame's mapping from the browser's own handle inside the parent's sandbox. The child's
/// viewport is measured when the caller has not already read it, and the parent's viewport comes
/// back with the mapping so the next hop out needs no extra round trip.
pub(super) async fn of(
    ff: &FirefoxSession,
    parent: &str,
    child: &str,
    child_viewport: Option<(f64, f64)>,
) -> Result<FrameHop, String> {
    let child_viewport = match child_viewport {
        Some(viewport) if sane_viewport(viewport) => viewport,
        _ => viewport(ff, child).await?,
    };
    let frame = verified_frame(ff, parent, child).await?;
    let js = format!(
        "function() {{ {TRANSFORM_JS} return frameTransform(this, {{ width: {}, height: {} }}); }}",
        child_viewport.0, child_viewport.1
    );
    let reply = ff
        .bidi
        .call_function_on_in_sandbox(parent, SANDBOX, &js, frame)
        .await?;
    if let Some(error) = reply["error"].as_str() {
        return Err(error.to_string());
    }
    let transform = FrameTransform::new(
        read_pair(&reply, "originX", "originY")?,
        read_pair(&reply, "scaleX", "scaleY")?,
    )?;
    let parent_viewport = read_pair(&reply["viewport"], "width", "height")?;
    if !sane_viewport(parent_viewport) {
        return Err("Firefox did not report the frame's viewport".to_string());
    }
    Ok(FrameHop {
        transform,
        parent_viewport,
    })
}

/// Maps a point out of a frame's viewport into the top-level viewport the pointer state is kept in.
/// `hops` are the hops down to that frame, outermost first, so they are applied from the frame out.
pub(super) async fn to_root(
    ff: &FirefoxSession,
    hops: &[(String, String)],
    point: (f64, f64),
) -> Result<(f64, f64), String> {
    let mut viewport = None;
    let mut mapped = point;
    for (parent, child) in hops.iter().rev() {
        let hop = of(ff, parent, child, viewport).await?;
        mapped = hop.transform.to_parent(mapped);
        viewport = Some(hop.parent_viewport);
    }
    Ok(mapped)
}

/// Maps a point from the top-level viewport back into a frame's viewport, the inverse of `to_root`.
pub(super) async fn to_context(
    ff: &FirefoxSession,
    hops: &[(String, String)],
    point: (f64, f64),
) -> Result<(f64, f64), String> {
    let mut inward = Vec::new();
    for (parent, child) in hops {
        let hop = of(ff, parent, child, None).await?;
        inward.push(hop.transform);
    }
    let mut mapped = point;
    for transform in inward.iter() {
        mapped = transform.to_child(mapped);
    }
    Ok(mapped)
}

/// Maps a box out of a frame's viewport into an ancestor's, position and size alike. A frame the
/// browser cannot measure exactly, a rotated one for instance, is an error rather than a guess.
pub(super) async fn lift_rect(
    ff: &FirefoxSession,
    hops: &[(String, String)],
    rect: (f64, f64, f64, f64),
) -> Result<(f64, f64, f64, f64), String> {
    let mut viewport = None;
    let mut mapped = rect;
    for (parent, child) in hops.iter().rev() {
        let hop = of(ff, parent, child, viewport).await?;
        mapped = hop.transform.to_parent_rect(mapped);
        viewport = Some(hop.parent_viewport);
    }
    Ok(mapped)
}

fn read_pair(value: &Value, first: &str, second: &str) -> Result<(f64, f64), String> {
    match (value[first].as_f64(), value[second].as_f64()) {
        (Some(first), Some(second)) if finite((first, second)) => Ok((first, second)),
        _ => Err("Firefox returned a frame geometry it cannot describe".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::firefox::session::tests::fake_session_with;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn frame_tree() -> Value {
        json!({ "contexts": [{ "context": "top", "children": [
            { "context": "mid", "children": [{ "context": "deep" }] },
            { "context": "side" }
        ] }] })
    }

    fn geometry(origin: (f64, f64), scale: (f64, f64)) -> Value {
        json!({ "type": "success", "result": { "type": "object", "value": [
            ["originX", { "type": "number", "value": origin.0 }],
            ["originY", { "type": "number", "value": origin.1 }],
            ["scaleX", { "type": "number", "value": scale.0 }],
            ["scaleY", { "type": "number", "value": scale.1 }],
            ["viewport", { "type": "object", "value": [
                ["width", { "type": "number", "value": 800 }],
                ["height", { "type": "number", "value": 600 }],
            ] }],
        ] } })
    }

    fn viewport_reply(viewport: (f64, f64)) -> Value {
        json!({ "type": "success", "result": { "type": "object", "value": [
            ["width", { "type": "number", "value": viewport.0 }],
            ["height", { "type": "number", "value": viewport.1 }],
        ] } })
    }

    async fn session_with_geometry(
        requested: Arc<Mutex<Vec<Value>>>,
        child_viewport: (f64, f64),
        reply: fn(&Value) -> Value,
    ) -> FirefoxSession {
        let (ff, _tx, _calls) = fake_session_with(move |request| {
            requested.lock().unwrap().push(request.clone());
            if request["method"] == "browsingContext.locateNodes" {
                return json!({ "nodes": [{ "sharedId": "frame" }] });
            }
            if request["method"] == "script.callFunction" && request["params"]["this"].is_null() {
                return viewport_reply(child_viewport);
            }
            reply(request)
        })
        .await;
        ff
    }

    #[test]
    fn a_transform_scales_points_and_boxes_both_ways() {
        let transform = FrameTransform::new((10.0, 20.0), (2.0, 3.0)).unwrap();
        assert_eq!(transform.to_parent((40.0, 50.0)), (90.0, 170.0));
        assert_eq!(transform.to_child((90.0, 170.0)), (40.0, 50.0));
        assert_eq!(
            transform.to_parent_rect((5.0, 6.0, 10.0, 20.0)),
            (20.0, 38.0, 20.0, 60.0)
        );
    }

    #[test]
    fn a_transform_refuses_a_scale_it_cannot_invert() {
        assert!(FrameTransform::new((0.0, 0.0), (0.0, 1.0)).is_err());
        assert!(FrameTransform::new((0.0, 0.0), (1.0, -1.0)).is_err());
        assert!(FrameTransform::new((f64::NAN, 0.0), (1.0, 1.0)).is_err());
        assert!(FrameTransform::new((0.0, 0.0), (f64::INFINITY, 1.0)).is_err());
    }

    #[test]
    fn the_context_tree_yields_the_hops_out_of_a_frame() {
        let tree = frame_tree();
        assert_eq!(
            path(&tree, "deep"),
            Some(vec![
                ("top".to_string(), "mid".to_string()),
                ("mid".to_string(), "deep".to_string()),
            ])
        );
        assert_eq!(path(&tree, "top"), Some(Vec::new()));
        assert_eq!(path(&tree, "missing"), None);
        assert_eq!(chain(&tree, "deep", "top"), path(&tree, "deep"));
        assert_eq!(
            chain(&tree, "deep", "mid"),
            Some(vec![("mid".to_string(), "deep".to_string())])
        );
        assert_eq!(chain(&tree, "mid", "deep"), None);
        assert_eq!(
            chain(&tree, "side", "top"),
            Some(vec![("top".to_string(), "side".to_string())])
        );
    }

    #[tokio::test]
    async fn a_frame_reports_its_own_scale_and_the_parent_viewport() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let ff = session_with_geometry(requested.clone(), (640.0, 480.0), |_| {
            geometry((100.0, 200.0), (2.0, 4.0))
        })
        .await;

        let hop = of(&ff, "top", "deep", Some((640.0, 480.0))).await.unwrap();

        assert_eq!(hop.transform.to_parent((40.0, 25.0)), (180.0, 300.0));
        assert_eq!(hop.parent_viewport, (800.0, 600.0));
        let requests = requested.lock().unwrap();
        assert_eq!(requests[0]["method"], "browsingContext.locateNodes");
        assert_eq!(requests[0]["params"]["locator"]["value"]["context"], "deep");
        assert_eq!(requests[1]["method"], "script.callFunction");
        assert_eq!(requests[1]["params"]["this"]["sharedId"], "frame");
        assert_eq!(requests[1]["params"]["target"]["context"], "top");
        assert_eq!(requests[1]["params"]["target"]["sandbox"], SANDBOX);
    }

    #[tokio::test]
    async fn a_frame_reports_a_rotation_it_cannot_map() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let ff = session_with_geometry(requested.clone(), (640.0, 480.0), |_| {
            json!({ "type": "success", "result": { "type": "object", "value": [
                ["error", { "type": "string", "value": "the frame is rotated, skewed or mirrored" }],
            ] } })
        })
        .await;

        let error = of(&ff, "top", "deep", Some((640.0, 480.0)))
            .await
            .unwrap_err();

        assert!(error.contains("rotated"), "{error}");
    }

    #[tokio::test]
    async fn a_frame_without_a_viewport_is_refused() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let ff = session_with_geometry(requested.clone(), (640.0, 480.0), |_| {
            json!({ "type": "success", "result": { "type": "object", "value": [
                ["originX", { "type": "number", "value": 0 }],
                ["originY", { "type": "number", "value": 0 }],
                ["scaleX", { "type": "number", "value": 1 }],
                ["scaleY", { "type": "number", "value": 1 }],
            ] } })
        })
        .await;

        assert!(of(&ff, "top", "deep", Some((640.0, 480.0))).await.is_err());
    }

    #[tokio::test]
    async fn a_box_lifts_through_every_frame_holding_it() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let ff = session_with_geometry(requested.clone(), (640.0, 480.0), |request| {
            if request["params"]["target"]["context"] == "mid" {
                geometry((10.0, 20.0), (2.0, 2.0))
            } else {
                geometry((5.0, 7.0), (1.0, 1.0))
            }
        })
        .await;

        let hops = chain(&frame_tree(), "deep", "top").unwrap();
        let lifted = lift_rect(&ff, &hops, (0.0, 0.0, 10.0, 20.0)).await.unwrap();

        assert_eq!(lifted, (15.0, 27.0, 20.0, 40.0));
        let requests = requested.lock().unwrap();
        let contexts: Vec<&str> = requests
            .iter()
            .filter(|request| request["method"] == "script.callFunction")
            .filter_map(|request| request["params"]["target"]["context"].as_str())
            .collect();
        assert_eq!(contexts, ["deep", "mid", "top"]);
    }

    #[tokio::test]
    async fn a_box_a_rotated_frame_holds_is_refused() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let ff = session_with_geometry(requested.clone(), (640.0, 480.0), |_| {
            json!({ "type": "success", "result": { "type": "object", "value": [
                ["error", { "type": "string", "value": "the frame is rotated, skewed or mirrored" }],
            ] } })
        })
        .await;

        let hops = chain(&frame_tree(), "deep", "top").unwrap();
        let error = lift_rect(&ff, &hops, (0.0, 0.0, 10.0, 20.0))
            .await
            .unwrap_err();

        assert!(error.contains("rotated"), "{error}");
    }

    #[test]
    fn a_box_a_sibling_frame_holds_is_not_lifted() {
        let tree = frame_tree();
        assert_eq!(chain(&tree, "deep", "side"), None);
        assert_eq!(chain(&tree, "side", "mid"), None);
    }

    #[tokio::test]
    async fn a_point_round_trips_through_every_frame() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let ff = session_with_geometry(requested.clone(), (640.0, 480.0), |request| {
            if request["params"]["target"]["context"] == "mid" {
                geometry((10.0, 20.0), (2.0, 2.0))
            } else {
                geometry((5.0, 7.0), (0.5, 0.5))
            }
        })
        .await;

        let hops = path(&frame_tree(), "deep").unwrap();
        let out = to_root(&ff, &hops, (40.0, 50.0)).await.unwrap();
        requested.lock().unwrap().clear();
        let back = to_context(&ff, &hops, out).await.unwrap();

        assert_eq!(out, (50.0, 67.0));
        assert_eq!(back, (40.0, 50.0));
        let requests = requested.lock().unwrap();
        let viewport_contexts: Vec<_> = requests
            .iter()
            .filter(|request| {
                request["method"] == "script.callFunction" && request["params"]["this"].is_null()
            })
            .map(|request| request["params"]["target"]["context"].as_str().unwrap())
            .collect();
        assert_eq!(viewport_contexts, ["mid", "deep"]);
    }
}
