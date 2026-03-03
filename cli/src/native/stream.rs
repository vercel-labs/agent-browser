use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::tungstenite::Message;

use super::cdp::client::CdpClient;

/// Frame metadata from CDP Page.screencastFrame events.
#[derive(Debug, Clone)]
pub struct FrameMetadata {
    pub offset_top: f64,
    pub page_scale_factor: f64,
    pub device_width: u32,
    pub device_height: u32,
    pub scroll_offset_x: f64,
    pub scroll_offset_y: f64,
    pub timestamp: u64,
}

impl Default for FrameMetadata {
    fn default() -> Self {
        Self {
            offset_top: 0.0,
            page_scale_factor: 1.0,
            device_width: 1280,
            device_height: 720,
            scroll_offset_x: 0.0,
            scroll_offset_y: 0.0,
            timestamp: 0,
        }
    }
}

pub struct StreamServer {
    port: u16,
    frame_tx: broadcast::Sender<String>,
    client_count: Arc<Mutex<usize>>,
}

impl StreamServer {
    pub async fn start(
        preferred_port: u16,
        client: Arc<CdpClient>,
        session_id: String,
    ) -> Result<Self, String> {
        let addr = format!("127.0.0.1:{}", preferred_port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("Failed to bind stream server: {}", e))?;

        let actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to get stream address: {}", e))?;
        let port = actual_addr.port();

        let (frame_tx, _) = broadcast::channel::<String>(64);
        let client_count = Arc::new(Mutex::new(0usize));

        let frame_tx_clone = frame_tx.clone();
        let client_count_clone = client_count.clone();

        tokio::spawn(async move {
            accept_loop(
                listener,
                frame_tx_clone,
                client_count_clone,
                client,
                session_id,
            )
            .await;
        });

        Ok(Self {
            port,
            frame_tx,
            client_count,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Broadcast a raw frame string (legacy).
    pub fn broadcast_frame(&self, frame_json: &str) {
        let _ = self.frame_tx.send(frame_json.to_string());
    }

    /// Broadcast a screencast frame with structured metadata.
    pub fn broadcast_screencast_frame(&self, base64_data: &str, metadata: &FrameMetadata) {
        let msg = json!({
            "type": "frame",
            "data": base64_data,
            "metadata": {
                "offsetTop": metadata.offset_top,
                "pageScaleFactor": metadata.page_scale_factor,
                "deviceWidth": metadata.device_width,
                "deviceHeight": metadata.device_height,
                "scrollOffsetX": metadata.scroll_offset_x,
                "scrollOffsetY": metadata.scroll_offset_y,
                "timestamp": metadata.timestamp,
            }
        });
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Broadcast a status message to all connected clients.
    pub fn broadcast_status(
        &self,
        connected: bool,
        screencasting: bool,
        viewport_width: u32,
        viewport_height: u32,
    ) {
        let msg = json!({
            "type": "status",
            "connected": connected,
            "screencasting": screencasting,
            "viewportWidth": viewport_width,
            "viewportHeight": viewport_height,
        });
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Broadcast an error message to all connected clients.
    pub fn broadcast_error(&self, message: &str) {
        let msg = json!({
            "type": "error",
            "message": message,
        });
        let _ = self.frame_tx.send(msg.to_string());
    }
}

async fn accept_loop(
    listener: TcpListener,
    frame_tx: broadcast::Sender<String>,
    client_count: Arc<Mutex<usize>>,
    cdp_client: Arc<CdpClient>,
    session_id: String,
) {
    while let Ok((stream, addr)) = listener.accept().await {
        let frame_rx = frame_tx.subscribe();
        let client_count = client_count.clone();
        let cdp = cdp_client.clone();
        let sid = session_id.clone();

        tokio::spawn(async move {
            handle_ws_client(stream, addr, frame_rx, client_count, cdp, sid).await;
        });
    }
}

async fn handle_ws_client(
    stream: tokio::net::TcpStream,
    _addr: SocketAddr,
    mut frame_rx: broadcast::Receiver<String>,
    client_count: Arc<Mutex<usize>>,
    cdp_client: Arc<CdpClient>,
    session_id: String,
) {
    // Origin checking on WebSocket handshake
    let callback =
        |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
         resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let origin = req
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            if !is_allowed_origin(origin.as_deref()) {
                let mut reject =
                    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(Some(
                        "Origin not allowed".to_string(),
                    ));
                *reject.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
                return Err(reject);
            }
            Ok(resp)
        };

    let ws_stream = match tokio_tungstenite::accept_hdr_async(stream, callback).await {
        Ok(ws) => ws,
        Err(_) => return,
    };

    {
        let mut count = client_count.lock().await;
        *count += 1;
    }

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    loop {
        tokio::select! {
            frame = frame_rx.recv() => {
                match frame {
                    Ok(data) => {
                        if ws_tx.send(Message::Text(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        handle_client_message(&text, &cdp_client, &session_id).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    {
        let mut count = client_count.lock().await;
        *count = count.saturating_sub(1);
    }
}

async fn handle_client_message(msg: &str, client: &CdpClient, session_id: &str) {
    let parsed: Value = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(_) => return,
    };

    let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "input_mouse" => {
            let _ = client
                .send_command(
                    "Input.dispatchMouseEvent",
                    Some(json!({
                        "type": parsed.get("eventType").and_then(|v| v.as_str()).unwrap_or("mouseMoved"),
                        "x": parsed.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "y": parsed.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "button": parsed.get("button").and_then(|v| v.as_str()).unwrap_or("none"),
                        "clickCount": parsed.get("clickCount").and_then(|v| v.as_i64()).unwrap_or(0),
                        "deltaX": parsed.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "deltaY": parsed.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "modifiers": parsed.get("modifiers").and_then(|v| v.as_i64()).unwrap_or(0),
                    })),
                    Some(session_id),
                )
                .await;
        }
        "input_keyboard" => {
            let _ = client
                .send_command(
                    "Input.dispatchKeyEvent",
                    Some(json!({
                        "type": parsed.get("eventType").and_then(|v| v.as_str()).unwrap_or("keyDown"),
                        "key": parsed.get("key"),
                        "code": parsed.get("code"),
                        "text": parsed.get("text"),
                        "modifiers": parsed.get("modifiers").and_then(|v| v.as_i64()).unwrap_or(0),
                    })),
                    Some(session_id),
                )
                .await;
        }
        "input_touch" => {
            let _ = client
                .send_command(
                    "Input.dispatchTouchEvent",
                    Some(json!({
                        "type": parsed.get("eventType").and_then(|v| v.as_str()).unwrap_or("touchStart"),
                        "touchPoints": parsed.get("touchPoints").unwrap_or(&json!([])),
                        "modifiers": parsed.get("modifiers").and_then(|v| v.as_i64()).unwrap_or(0),
                    })),
                    Some(session_id),
                )
                .await;
        }
        "status" => {
            // Client requesting status -- handled via broadcast_status from the caller
        }
        _ => {}
    }
}

pub fn is_allowed_origin(origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(o) => {
            if o.starts_with("file://") {
                return true;
            }
            if let Ok(url) = url::Url::parse(o) {
                let host = url.host_str().unwrap_or("");
                host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
            } else {
                false
            }
        }
    }
}

pub async fn start_screencast(
    client: &CdpClient,
    session_id: &str,
    format: &str,
    quality: i32,
    max_width: i32,
    max_height: i32,
) -> Result<(), String> {
    client
        .send_command(
            "Page.startScreencast",
            Some(json!({
                "format": format,
                "quality": quality,
                "maxWidth": max_width,
                "maxHeight": max_height,
                "everyNthFrame": 1,
            })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

pub async fn stop_screencast(client: &CdpClient, session_id: &str) -> Result<(), String> {
    client
        .send_command_no_params("Page.stopScreencast", Some(session_id))
        .await?;
    Ok(())
}

pub async fn ack_screencast_frame(
    client: &CdpClient,
    session_id: &str,
    screencast_session_id: i64,
) -> Result<(), String> {
    client
        .send_command(
            "Page.screencastFrameAck",
            Some(json!({ "sessionId": screencast_session_id })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_origin_none() {
        assert!(is_allowed_origin(None));
    }

    #[test]
    fn test_allowed_origin_file() {
        assert!(is_allowed_origin(Some("file:///path/to/file")));
    }

    #[test]
    fn test_allowed_origin_localhost() {
        assert!(is_allowed_origin(Some("http://localhost:3000")));
        assert!(is_allowed_origin(Some("http://127.0.0.1:8080")));
    }

    #[test]
    fn test_disallowed_origin() {
        assert!(!is_allowed_origin(Some("http://evil.com")));
    }

    #[test]
    fn test_frame_metadata_default() {
        let meta = FrameMetadata::default();
        assert_eq!(meta.device_width, 1280);
        assert_eq!(meta.device_height, 720);
        assert_eq!(meta.page_scale_factor, 1.0);
    }
}
