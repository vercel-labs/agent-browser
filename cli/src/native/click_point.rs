//! Placement of a ref's click point in the viewport that receives the input.
//!
//! Page script measures an element inside one origin, so its frame walk stops
//! at an origin boundary and cannot follow a frame drawn with a transform. The
//! browser reports an element's content quads in the viewport of the session
//! that owns its frame, which stays correct through both, so the pressed point
//! comes from there and a browser hit test confirms where the point lands.

use serde_json::Value;

use super::cdp::client::CdpClient;

/// A point in the viewport of a CDP session, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickPoint {
    pub x: f64,
    pub y: f64,
}

/// Move `point` into the viewport of `dispatch_session`, then confirm the
/// browser would deliver a press there inside the ref's own frame or a frame
/// nested in it. `point` is the page-script measurement inside the ref's frame
/// and is used only when the browser reports no box for the element.
pub async fn place_in_dispatch_viewport(
    client: &CdpClient,
    dispatch_session: &str,
    frame_id: Option<&str>,
    backend_node_id: i64,
    point: &mut ClickPoint,
    target: &str,
) -> Result<(), String> {
    let tree = match frame_id {
        Some(_) => Some(frame_tree(client, dispatch_session).await?),
        None => None,
    };
    *point = dispatch_point(
        client,
        dispatch_session,
        backend_node_id,
        *point,
        tree.as_ref(),
        frame_id,
        target,
    )
    .await?;
    let Some(frame_id) = frame_id else {
        return Ok(());
    };
    let (origin_x, origin_y) = viewport_origin(client, dispatch_session).await?;
    let landed =
        hit_test_frame(client, dispatch_session, *point, origin_x, origin_y, target).await?;
    if frame_subtree_contains(tree.as_ref(), frame_id, &landed) {
        return Ok(());
    }
    Err(format!(
        "Element '{}' cannot be clicked safely: a click at ({}, {}) would land outside its frame, so the input could reach a different element. Open the frame's own URL to interact with it.",
        target, point.x, point.y
    ))
}

/// The point to press in the dispatch viewport: the center of the element's
/// largest content quad, so a press reaches the element rather than a box the
/// page script computed. An element in the session's own root frame keeps its
/// page-script point when the browser reports no box, since that point is
/// already in the dispatch viewport.
async fn dispatch_point(
    client: &CdpClient,
    dispatch_session: &str,
    backend_node_id: i64,
    measured: ClickPoint,
    tree: Option<&Value>,
    frame_id: Option<&str>,
    target: &str,
) -> Result<ClickPoint, String> {
    if let Some(center) = content_quad_center(client, dispatch_session, backend_node_id).await? {
        return Ok(whole_pixels(center));
    }
    if frame_id.is_none_or(|frame| tree.and_then(root_frame_id) == Some(frame)) {
        return Ok(whole_pixels(measured));
    }
    Err(format!(
        "Element '{}' cannot be clicked safely: the browser reports no box for it, so its position inside frame {} is unknown. Open the frame's own URL to interact with it.",
        target,
        frame_id.unwrap_or_default()
    ))
}

/// Center of the largest of the element's content quads, in CSS pixels of
/// `session`'s root viewport. A browser reports one quad per line box, so the
/// largest is the piece a press is most likely to reach.
async fn content_quad_center(
    client: &CdpClient,
    session: &str,
    backend_node_id: i64,
) -> Result<Option<ClickPoint>, String> {
    let response = client
        .send_command(
            "DOM.getContentQuads",
            Some(serde_json::json!({ "backendNodeId": backend_node_id })),
            Some(session),
        )
        .await
        .map_err(|error| format!("Cannot read where the element sits in the page: {}", error))?;
    Ok(response
        .get("quads")
        .and_then(Value::as_array)
        .and_then(|quads| largest_quad_center(quads.iter().filter_map(quad))))
}

fn largest_quad_center(quads: impl Iterator<Item = [f64; 8]>) -> Option<ClickPoint> {
    quads
        .max_by(|left, right| quad_area(left).total_cmp(&quad_area(right)))
        .map(|quad| quad_center(&quad))
}

fn quad(value: &Value) -> Option<[f64; 8]> {
    let numbers = value.as_array()?;
    if numbers.len() != 8 {
        return None;
    }
    let mut quad = [0.0; 8];
    for (slot, number) in quad.iter_mut().zip(numbers) {
        *slot = number.as_f64()?;
    }
    Some(quad)
}

fn quad_center(quad: &[f64; 8]) -> ClickPoint {
    let mut x = 0.0;
    let mut y = 0.0;
    for corner in 0..4 {
        x += quad[corner * 2];
        y += quad[corner * 2 + 1];
    }
    ClickPoint {
        x: x / 4.0,
        y: y / 4.0,
    }
}

fn quad_area(quad: &[f64; 8]) -> f64 {
    let mut sum = 0.0;
    for corner in 0..4 {
        let next = (corner + 1) % 4;
        sum += quad[corner * 2] * quad[next * 2 + 1] - quad[next * 2] * quad[corner * 2 + 1];
    }
    (sum / 2.0).abs()
}

/// Whole pixels keep the pressed point and the hit-tested point identical.
fn whole_pixels(point: ClickPoint) -> ClickPoint {
    ClickPoint {
        x: point.x.round(),
        y: point.y.round(),
    }
}

/// Document coordinates of the dispatch session's viewport origin. Box geometry
/// and input events are viewport-relative, while `DOM.getNodeForLocation` takes
/// document coordinates, so a scrolled page differs by this offset.
async fn viewport_origin(client: &CdpClient, session: &str) -> Result<(f64, f64), String> {
    let metrics = client
        .send_command_no_params("Page.getLayoutMetrics", Some(session))
        .await
        .map_err(|error| format!("Cannot read the page viewport origin: {}", error))?;
    let viewport = metrics
        .get("visualViewport")
        .or_else(|| metrics.get("cssVisualViewport"));
    let x = viewport
        .and_then(|viewport| viewport.get("pageX"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let y = viewport
        .and_then(|viewport| viewport.get("pageY"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Ok((x, y))
}

/// Id of the frame that holds the point, as the browser sees it.
async fn hit_test_frame(
    client: &CdpClient,
    session: &str,
    point: ClickPoint,
    origin_x: f64,
    origin_y: f64,
    target: &str,
) -> Result<String, String> {
    let refusal = |detail: &str| {
        format!(
            "Cannot confirm which element would receive a click at ({}, {}), so the click on '{}' was refused: {}",
            point.x, point.y, target, detail
        )
    };
    let hit = client
        .send_command(
            "DOM.getNodeForLocation",
            Some(serde_json::json!({
                "x": point.x as i64 + origin_x.round() as i64,
                "y": point.y as i64 + origin_y.round() as i64,
                "includeUserAgentShadowDOM": false,
                "ignorePointerEventsNone": false,
            })),
            Some(session),
        )
        .await
        .map_err(|error| refusal(&error.to_string()))?;
    hit.get("frameId")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| refusal("the browser named no frame"))
}

async fn frame_tree(client: &CdpClient, session: &str) -> Result<Value, String> {
    client
        .send_command_no_params("Page.getFrameTree", Some(session))
        .await
        .map_err(|error| format!("Cannot read the page frame tree: {}", error))
}

fn root_frame_id(tree: &Value) -> Option<&str> {
    tree.get("frameTree")
        .and_then(|root| root.get("frame"))
        .and_then(|frame| frame.get("id"))
        .and_then(Value::as_str)
}

/// Whether the browser placed the point in `ancestor` or in a frame nested
/// inside it. A nested frame draws within its parent, so its content is inside
/// the parent's frame as well.
fn frame_subtree_contains(tree: Option<&Value>, ancestor: &str, frame: &str) -> bool {
    tree.and_then(|tree| tree.get("frameTree"))
        .and_then(|root| find_frame_node(root, ancestor))
        .is_some_and(|node| subtree_has_frame(node, frame))
}

fn find_frame_node<'a>(node: &'a Value, frame_id: &str) -> Option<&'a Value> {
    if frame_id_of(node) == Some(frame_id) {
        return Some(node);
    }
    child_frames(node).find_map(|child| find_frame_node(child, frame_id))
}

fn subtree_has_frame(node: &Value, frame_id: &str) -> bool {
    frame_id_of(node) == Some(frame_id)
        || child_frames(node).any(|child| subtree_has_frame(child, frame_id))
}

fn frame_id_of(node: &Value) -> Option<&str> {
    node.get("frame")
        .and_then(|frame| frame.get("id"))
        .and_then(Value::as_str)
}

fn child_frames(node: &Value) -> impl Iterator<Item = &Value> {
    node.get("childFrames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: ClickPoint, expected: ClickPoint) {
        assert!(
            (actual.x - expected.x).abs() < 0.001 && (actual.y - expected.y).abs() < 0.001,
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn test_quad_center_follows_a_rotated_frame() {
        let square = [0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0];
        close(quad_center(&square), ClickPoint { x: 5.0, y: 5.0 });
        let angle = std::f64::consts::FRAC_PI_6;
        let (sin, cos) = angle.sin_cos();
        let rotate = |x: f64, y: f64| (x * cos - y * sin + 100.0, x * sin + y * cos + 50.0);
        let rotated = [
            rotate(0.0, 0.0),
            rotate(10.0, 0.0),
            rotate(10.0, 10.0),
            rotate(0.0, 10.0),
        ];
        let quad = [
            rotated[0].0,
            rotated[0].1,
            rotated[1].0,
            rotated[1].1,
            rotated[2].0,
            rotated[2].1,
            rotated[3].0,
            rotated[3].1,
        ];
        let center = rotate(5.0, 5.0);
        close(
            quad_center(&quad),
            ClickPoint {
                x: center.0,
                y: center.1,
            },
        );
        assert!((quad_area(&quad) - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_largest_quad_center_picks_the_biggest_line_box() {
        let short = [0.0, 0.0, 20.0, 0.0, 20.0, 10.0, 0.0, 10.0];
        let long = [0.0, 10.0, 200.0, 10.0, 200.0, 20.0, 0.0, 20.0];
        close(
            largest_quad_center([short, long].into_iter()).unwrap(),
            ClickPoint { x: 100.0, y: 15.0 },
        );
        assert_eq!(largest_quad_center(std::iter::empty()), None);
    }

    #[test]
    fn test_quad_rejects_values_that_are_not_four_corners() {
        assert_eq!(quad(&serde_json::json!([0, 0, 1, 1])), None);
        assert_eq!(quad(&serde_json::json!("0 0 1 1")), None);
        assert!(quad(&serde_json::json!([0, 0, 1, 0, 1, 1, 0, 1])).is_some());
    }

    #[test]
    fn test_frame_subtree_contains_accepts_a_nested_frame_only() {
        let tree = serde_json::json!({
            "frameTree": {
                "frame": { "id": "root" },
                "childFrames": [{
                    "frame": { "id": "outer" },
                    "childFrames": [{
                        "frame": { "id": "inner" },
                        "childFrames": [{ "frame": { "id": "deep" } }]
                    }]
                }, {
                    "frame": { "id": "sibling" }
                }]
            }
        });
        assert_eq!(root_frame_id(&tree), Some("root"));
        assert!(frame_subtree_contains(Some(&tree), "outer", "outer"));
        assert!(frame_subtree_contains(Some(&tree), "outer", "inner"));
        assert!(frame_subtree_contains(Some(&tree), "outer", "deep"));
        assert!(!frame_subtree_contains(Some(&tree), "outer", "sibling"));
        assert!(!frame_subtree_contains(Some(&tree), "outer", "root"));
        assert!(!frame_subtree_contains(Some(&tree), "missing", "root"));
        assert!(!frame_subtree_contains(None, "outer", "inner"));
    }
}
