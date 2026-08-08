use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use super::types::{CdpCommand, CdpError, CdpEvent, CdpMessage};

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<CdpMessage>>>>;

/// Interval between WebSocket ping frames sent to keep the connection alive
/// through intermediate proxies (reverse proxies, load balancers, service meshes).
const WS_KEEPALIVE_INTERVAL_SECS: u64 = 30;

/// Raw incoming CDP message (text) broadcast to all subscribers.
/// Used by the inspect proxy to forward responses and events to DevTools.
#[derive(Debug, Clone)]
pub struct RawCdpMessage {
    pub text: String,
    pub session_id: Option<String>,
}

/// Rewrite unpaired UTF-16 surrogate escapes to the replacement character.
///
/// JSON does not require well-formed UTF-16 (RFC 8259 section 8.2), and Chrome
/// does emit lone surrogates: a flag emoji (U+1F1FA U+1F1F8) is two code points
/// (four UTF-16 units), and layout can split it across `InlineTextBox` nodes,
/// leaving an accessibility node whose entire name is `"\ud83c"`. Rust strings
/// are UTF-8 and cannot hold an unpaired surrogate, so `serde_json` rejects the
/// whole message. Replacing the escape keeps the surrounding response usable —
/// these split fragments are layout artifacts, never user-facing refs.
///
/// Returns `None` when the input has no lone surrogate, so the common path does
/// not allocate.
fn sanitize_lone_surrogates(msg: &str) -> Option<String> {
    const ESCAPE_LEN: usize = 6; // \uXXXX
    const HIGH_START: u32 = 0xD800;
    const LOW_START: u32 = 0xDC00;
    const SURROGATE_END: u32 = 0xE000;

    let read_escape = |bytes: &[u8], at: usize| -> Option<u32> {
        let end = at.checked_add(ESCAPE_LEN)?;
        if end > bytes.len() || bytes[at] != b'\\' || bytes[at + 1] != b'u' {
            return None;
        }
        let hex = &bytes[at + 2..end];
        // from_str_radix would accept a leading sign, which is not a valid
        // escape; require four literal hex digits.
        if !hex.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()
    };

    let bytes = msg.as_bytes();
    let mut out: Option<String> = None;
    // Start of the run of untouched input not yet copied into `out`. Copying by
    // slice rather than char-by-char keeps multi-byte UTF-8 sequences intact and
    // means the escape scan never has to reason about character boundaries.
    let mut copied = 0;
    let mut i = 0;

    while i < bytes.len() {
        // Skip escaped characters so a literal `\\u` in the payload is not
        // mistaken for the start of an escape sequence.
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] != b'u' {
            i += 2;
            continue;
        }

        let Some(unit) = read_escape(bytes, i) else {
            // Advance one byte at a time: every escape starts with an ASCII
            // backslash, so landing mid-sequence in a multi-byte character
            // cannot produce a false match, and the bytes are copied verbatim
            // as part of the surrounding run.
            i += 1;
            continue;
        };

        let is_high = (HIGH_START..LOW_START).contains(&unit);
        let is_low = (LOW_START..SURROGATE_END).contains(&unit);

        // A high surrogate immediately followed by a low one is a valid pair.
        let paired = is_high
            && read_escape(bytes, i + ESCAPE_LEN)
                .is_some_and(|next| (LOW_START..SURROGATE_END).contains(&next));

        if paired {
            i += ESCAPE_LEN * 2;
            continue;
        }

        if is_high || is_low {
            let buf = out.get_or_insert_with(String::new);
            // `copied` and `i` are both escape boundaries (ASCII), so this
            // slice is always on a character boundary.
            buf.push_str(&msg[copied..i]);
            buf.push(char::REPLACEMENT_CHARACTER);
            copied = i + ESCAPE_LEN;
        }
        i += ESCAPE_LEN;
    }

    if let Some(buf) = out.as_mut() {
        buf.push_str(&msg[copied..]);
    }

    out
}

/// Best-effort extraction of a top-level command id from a frame that could not
/// be deserialized into a [`CdpMessage`], so the caller can be failed fast
/// instead of waiting forever for a response that will never be delivered.
///
/// This runs only on the cold path where deserialization already failed, so it
/// can afford a full generic parse. That matters for correctness: a substring
/// scan for `"id"` would happily match one nested inside `result`, and resolving
/// the wrong pending request would report a spurious error on an unrelated
/// command while the genuinely stuck one kept waiting.
///
/// Returns `None` when the frame is not valid JSON at all, has no top-level
/// `id`, or carries an id outside `u64` (the inspect proxy uses negative ids).
fn extract_command_id(msg: &str) -> Option<u64> {
    let value: Value = serde_json::from_str(msg).ok()?;
    value.get("id")?.as_u64()
}

pub struct CdpClient {
    ws_tx: Arc<
        Mutex<
            futures_util::stream::SplitSink<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
                Message,
            >,
        >,
    >,
    next_id: AtomicU64,
    pending: PendingMap,
    event_tx: broadcast::Sender<CdpEvent>,
    raw_tx: broadcast::Sender<RawCdpMessage>,
    _reader_handle: tokio::task::JoinHandle<()>,
    _keepalive_handle: tokio::task::JoinHandle<()>,
}

/// Removes a pending entry if `send_command` is cancelled mid-await (e.g. an
/// outer timeout on the liveness probe), so a command whose response never
/// comes can't leak until the connection closes (#1528). Normal exits disarm
/// it via `done`.
struct PendingGuard {
    pending: PendingMap,
    id: u64,
    done: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let pending = self.pending.clone();
        let id = self.id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                pending.lock().await.remove(&id);
            });
        }
    }
}

impl CdpClient {
    pub async fn connect(url: &str) -> Result<Self, String> {
        Self::connect_with_headers(url, None).await
    }

    pub async fn connect_with_headers(
        url: &str,
        headers: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        let mut request = url
            .into_client_request()
            .map_err(|e| format!("Invalid WebSocket URL: {}", e))?;

        if let Some(hdrs) = headers {
            let req_headers = request.headers_mut();
            for (key, value) in hdrs {
                if let (Ok(name), Ok(val)) = (
                    key.parse::<tokio_tungstenite::tungstenite::http::header::HeaderName>(),
                    value.parse::<tokio_tungstenite::tungstenite::http::header::HeaderValue>(),
                ) {
                    req_headers.insert(name, val);
                }
            }
        }

        let ws_config = WebSocketConfig {
            max_message_size: None,
            max_frame_size: None,
            ..Default::default()
        };

        let (ws_stream, _) =
            tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false)
                .await
                .map_err(|e| format!("CDP WebSocket connect failed: {}", e))?;

        enable_tcp_keepalive(ws_stream.get_ref());

        let (ws_tx, mut ws_rx) = ws_stream.split();
        let ws_tx = Arc::new(Mutex::new(ws_tx));

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(4096);
        let (raw_tx, _) = broadcast::channel(4096);

        let pending_clone = pending.clone();
        let event_tx_clone = event_tx.clone();
        let raw_tx_clone = raw_tx.clone();

        // Notify used to stop the keepalive task when the reader loop exits.
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

        let reader_handle = tokio::spawn(async move {
            while let Some(msg) = ws_rx.next().await {
                // Accept both Text and Binary frames — remote CDP proxies
                // (e.g. Browserless) may send responses as Binary frames.
                let msg = match msg {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Binary(data)) => match String::from_utf8(data) {
                        Ok(text) => text,
                        Err(_) => continue,
                    },
                    Ok(Message::Close(frame)) => {
                        if std::env::var("AGENT_BROWSER_DEBUG").is_ok() {
                            let reason = frame
                                .as_ref()
                                .map(|f| format!("code={}, reason={}", f.code, f.reason))
                                .unwrap_or_else(|| "no frame".to_string());
                            let _ =
                                writeln!(std::io::stderr(), "[cdp] WebSocket Close: {}", reason);
                        }
                        break;
                    }
                    Ok(Message::Pong(_)) => continue,
                    Ok(_) => continue,
                    Err(e) => {
                        if std::env::var("AGENT_BROWSER_DEBUG").is_ok() {
                            let _ = writeln!(std::io::stderr(), "[cdp] WebSocket Error: {}", e);
                        }
                        break;
                    }
                };

                // Broadcast raw message for inspect proxy subscribers before typed parse,
                // so messages with negative IDs (used by the inspect proxy) are still delivered.
                if raw_tx_clone.receiver_count() > 0 {
                    let session_id = serde_json::from_str::<serde_json::Value>(&msg)
                        .ok()
                        .and_then(|v| v.get("sessionId")?.as_str().map(String::from));
                    let _ = raw_tx_clone.send(RawCdpMessage {
                        text: msg.clone(),
                        session_id,
                    });
                }

                let parsed: CdpMessage = match serde_json::from_str(&msg) {
                    Ok(m) => m,
                    Err(_) => {
                        // Chrome can emit lone UTF-16 surrogates (a split flag
                        // emoji in an InlineTextBox name), which serde_json
                        // rejects. Retry with those escapes neutralized before
                        // giving up on the frame.
                        let repaired = sanitize_lone_surrogates(&msg);
                        let candidate = repaired.as_deref().unwrap_or(&msg);

                        match serde_json::from_str::<CdpMessage>(candidate) {
                            Ok(m) => m,
                            Err(_) => {
                                // Never strand a caller: if the frame carries a
                                // command id, fail that request so it returns an
                                // error instead of hanging until the process is
                                // killed. Frames without an id are expected for
                                // inspect proxy messages with negative IDs
                                // (CdpMessage.id is u64); those are handled via
                                // the raw broadcast above.
                                if let Some(id) = extract_command_id(candidate) {
                                    let waiter = pending_clone.lock().await.remove(&id);
                                    if let Some(tx) = waiter {
                                        let _ = tx.send(CdpMessage {
                                            id: Some(id),
                                            result: None,
                                            error: Some(CdpError {
                                                code: None,
                                                message: "Malformed CDP response could not be \
                                                          parsed (unpaired UTF-16 surrogate or \
                                                          invalid JSON)"
                                                    .to_string(),
                                                data: None,
                                            }),
                                            method: None,
                                            params: None,
                                            session_id: None,
                                        });
                                    }
                                }
                                continue;
                            }
                        }
                    }
                };

                if let Some(id) = parsed.id {
                    // Response to a command
                    let mut pending = pending_clone.lock().await;
                    if let Some(tx) = pending.remove(&id) {
                        let _ = tx.send(parsed);
                    }
                } else if let Some(ref method) = parsed.method {
                    // Event
                    let event = CdpEvent {
                        method: method.clone(),
                        params: parsed.params.clone().unwrap_or(Value::Null),
                        session_id: parsed.session_id.clone(),
                    };
                    let _ = event_tx_clone.send(event);
                }
            }

            // Reader loop exited (connection closed or error). Drop all pending
            // command senders so callers get an immediate channel-closed error
            // instead of waiting for the 30-second timeout.
            pending_clone.lock().await.clear();

            // Stop the keepalive task — the connection is gone.
            let _ = cancel_tx.send(true);
        });

        // Spawn a keepalive task that sends WebSocket Ping frames at a regular
        // interval. This prevents intermediate proxies (Envoy, nginx, OpenResty,
        // cloud load balancers) from closing idle WebSocket connections. If the
        // send fails, the connection is dead and we stop pinging.
        let keepalive_tx = ws_tx.clone();
        let keepalive_handle = tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(WS_KEEPALIVE_INTERVAL_SECS);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = cancel_rx.changed() => break,
                }
                let mut tx = keepalive_tx.lock().await;
                if tx.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            ws_tx,
            next_id: AtomicU64::new(1),
            pending,
            event_tx,
            raw_tx,
            _reader_handle: reader_handle,
            _keepalive_handle: keepalive_handle,
        })
    }

    pub async fn send_command(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let cmd = CdpCommand {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        };

        let json = serde_json::to_string(&cmd)
            .map_err(|e| format!("Failed to serialize CDP command: {}", e))?;

        let (tx, rx) = oneshot::channel();

        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // Cleans up the pending entry if this future is cancelled mid-await (#1528).
        let mut guard = PendingGuard {
            pending: self.pending.clone(),
            id,
            done: false,
        };

        {
            let mut ws_tx = self.ws_tx.lock().await;
            ws_tx
                .send(Message::Text(json))
                .await
                .map_err(|e| format!("Failed to send CDP command: {}", e))?;
        }

        let response = match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(resp)) => {
                guard.done = true;
                resp
            }
            Ok(Err(_)) => {
                guard.done = true;
                return Err("CDP response channel closed".to_string());
            }
            Err(_) => {
                guard.done = true;
                self.pending.lock().await.remove(&id);
                return Err(format!("CDP command timed out: {}", method));
            }
        };

        if let Some(error) = response.error {
            return Err(format!("CDP error ({}): {}", method, error));
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.event_tx.subscribe()
    }

    /// Subscribe to all raw incoming CDP messages (responses + events).
    /// Used by the inspect proxy to forward traffic to the DevTools frontend.
    pub fn subscribe_raw(&self) -> broadcast::Receiver<RawCdpMessage> {
        self.raw_tx.subscribe()
    }

    /// Create a lightweight handle for the inspect WebSocket proxy.
    /// Contains only what's needed to forward messages bidirectionally.
    pub fn inspect_handle(&self) -> InspectProxyHandle {
        InspectProxyHandle {
            ws_tx: self.ws_tx.clone(),
            raw_tx: self.raw_tx.clone(),
        }
    }

    pub async fn send_command_typed<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        session_id: Option<&str>,
    ) -> Result<R, String> {
        let params_value = serde_json::to_value(params)
            .map_err(|e| format!("Failed to serialize params: {}", e))?;
        let result = self
            .send_command(method, Some(params_value), session_id)
            .await?;
        serde_json::from_value(result)
            .map_err(|e| format!("Failed to deserialize CDP response for {}: {}", method, e))
    }

    pub async fn send_command_no_params(
        &self,
        method: &str,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        self.send_command(method, None, session_id).await
    }

    /// Send a CDP command without waiting for its response.
    ///
    /// This is useful for best-effort commands where Chrome may not emit a
    /// response for every target session, but the command still needs to be
    /// written before the caller can continue processing events.
    pub async fn send_command_no_wait(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cmd = CdpCommand {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        };

        let json = serde_json::to_string(&cmd)
            .map_err(|e| format!("Failed to serialize CDP command: {}", e))?;

        let mut ws_tx = self.ws_tx.lock().await;
        ws_tx
            .send(Message::Text(json))
            .await
            .map_err(|e| format!("Failed to send CDP command: {}", e))
    }

    /// Send raw JSON through the WebSocket without tracking a response.
    /// Used by the inspect proxy to forward DevTools frontend messages.
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        let mut ws_tx = self.ws_tx.lock().await;
        ws_tx
            .send(Message::Text(json))
            .await
            .map_err(|e| format!("Failed to send raw CDP message: {}", e))
    }

    /// Test-only: count of in-flight commands still awaiting a response, so a
    /// test can assert a cancelled command left no orphaned entry (#1528).
    #[cfg(test)]
    pub(crate) async fn pending_len(&self) -> usize {
        self.pending.lock().await.len()
    }
}

type WsTx = Arc<
    Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    >,
>;

/// Lightweight handle for the inspect WebSocket proxy, holding only
/// the cloneable parts of CdpClient needed for bidirectional message forwarding.
pub struct InspectProxyHandle {
    ws_tx: WsTx,
    raw_tx: broadcast::Sender<RawCdpMessage>,
}

impl InspectProxyHandle {
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        let mut ws_tx = self.ws_tx.lock().await;
        ws_tx
            .send(Message::Text(json))
            .await
            .map_err(|e| format!("Failed to send raw CDP message: {}", e))
    }

    pub fn subscribe_raw(&self) -> broadcast::Receiver<RawCdpMessage> {
        self.raw_tx.subscribe()
    }
}

/// Enable TCP SO_KEEPALIVE on the underlying socket of a WebSocket connection.
/// This is best-effort: failures are silently ignored since the WebSocket-level
/// Ping keepalive provides the primary connection liveness mechanism.
fn enable_tcp_keepalive(stream: &tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>) {
    let tcp_stream = match stream {
        tokio_tungstenite::MaybeTlsStream::Plain(s) => s,
        tokio_tungstenite::MaybeTlsStream::Rustls(s) => s.get_ref().0,
        _ => return,
    };

    // SockRef borrows the fd without taking ownership.
    let sock = socket2::SockRef::from(tcp_stream);
    let keepalive = socket2::TcpKeepalive::new().with_time(std::time::Duration::from_secs(30));

    // with_interval sets TCP_KEEPINTVL — the time between probes after the
    // first keepalive probe goes unanswered. Available on most platforms
    // (Linux, macOS, Windows, FreeBSD, etc.) but not OpenBSD or Haiku.
    #[cfg(not(any(target_os = "openbsd", target_os = "haiku")))]
    let keepalive = keepalive.with_interval(std::time::Duration::from_secs(10));

    let _ = sock.set_tcp_keepalive(&keepalive);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape Chrome emits when layout splits a flag emoji: the
    /// InlineTextBox name is the unpaired high surrogate of a flag emoji.
    const LONE_SURROGATE_RESPONSE: &str = r#"{"id":7,"result":{"nodes":[{"nodeId":"-1000000831","role":{"value":"InlineTextBox"},"name":{"type":"computedString","value":"\ud83c"}}]}}"#;

    #[test]
    fn serde_json_rejects_lone_surrogates() {
        // Guards the premise of the fix: without sanitizing, this frame is
        // unparseable and the reader loop would drop the response.
        assert!(serde_json::from_str::<CdpMessage>(LONE_SURROGATE_RESPONSE).is_err());
    }

    #[test]
    fn sanitizes_lone_surrogate_into_parseable_message() {
        let clean = sanitize_lone_surrogates(LONE_SURROGATE_RESPONSE)
            .expect("lone surrogate should be rewritten");
        let parsed: CdpMessage =
            serde_json::from_str(&clean).expect("sanitized frame should parse");
        assert_eq!(parsed.id, Some(7));

        let name = parsed.result.unwrap()["nodes"][0]["name"]["value"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(name, "\u{FFFD}");
    }

    #[test]
    fn preserves_multi_byte_characters_after_a_lone_surrogate() {
        // Everything after the first rewrite must survive byte for byte. An
        // earlier revision copied the tail one byte at a time, which silently
        // mangled any multi-byte UTF-8 that followed the lone surrogate.
        // The literal text is built with escapes so the source stays ASCII; what
        // reaches the sanitizer is real multi-byte UTF-8.
        let msg = format!(
            "{{\"id\":9,\"result\":{{\"a\":\"\\ud83c\",\"b\":\"{}\",\"c\":\"{} ok\"}}}}",
            "\u{4E2D}\u{6587}", "\u{1F1FA}\u{1F1F8}"
        );
        let clean = sanitize_lone_surrogates(&msg).expect("lone surrogate should be rewritten");
        let parsed: CdpMessage = serde_json::from_str(&clean).expect("should parse");

        let result = parsed.result.unwrap();
        assert_eq!(result["a"].as_str().unwrap(), "\u{FFFD}");
        assert_eq!(result["b"].as_str().unwrap(), "\u{4E2D}\u{6587}");
        assert_eq!(result["c"].as_str().unwrap(), "\u{1F1FA}\u{1F1F8} ok");
    }

    #[test]
    fn preserves_multi_byte_characters_between_lone_surrogates() {
        // The tail after the final rewrite is copied separately from the runs
        // between rewrites; both paths need to stay on character boundaries.
        let msg = format!(
            "{{\"id\":10,\"result\":{{\"a\":\"\\ud83c\",\"b\":\"{}\",\"c\":\"\\udc00\",\"d\":\"{}\"}}}}",
            "\u{4E2D}", "\u{6587}"
        );
        let clean = sanitize_lone_surrogates(&msg).expect("lone surrogates should be rewritten");
        let parsed: CdpMessage = serde_json::from_str(&clean).expect("should parse");

        let result = parsed.result.unwrap();
        assert_eq!(result["a"].as_str().unwrap(), "\u{FFFD}");
        assert_eq!(result["b"].as_str().unwrap(), "\u{4E2D}");
        assert_eq!(result["c"].as_str().unwrap(), "\u{FFFD}");
        assert_eq!(result["d"].as_str().unwrap(), "\u{6587}");
    }

    #[test]
    fn leaves_multi_byte_messages_without_surrogates_untouched() {
        let msg = format!(
            "{{\"id\":11,\"result\":{{\"value\":\"{} {} mixed\"}}}}",
            "\u{4E2D}\u{6587}", "\u{1F1FA}\u{1F1F8}"
        );
        assert!(sanitize_lone_surrogates(&msg).is_none());
    }

    #[test]
    fn leaves_valid_surrogate_pairs_untouched() {
        // A well-formed pair is the whole flag emoji and must survive intact.
        let msg = r#"{"id":1,"result":{"name":"\ud83c\uddfa\ud83c\uddf8"}}"#;
        assert!(sanitize_lone_surrogates(msg).is_none());

        let parsed: CdpMessage = serde_json::from_str(msg).unwrap();
        assert_eq!(
            parsed.result.unwrap()["name"].as_str().unwrap(),
            "\u{1F1FA}\u{1F1F8}"
        );
    }

    #[test]
    fn leaves_ordinary_messages_untouched() {
        let msg = r#"{"id":2,"result":{"value":"plain text, no escapes"}}"#;
        assert!(sanitize_lone_surrogates(msg).is_none());
    }

    #[test]
    fn ignores_escaped_backslash_before_u() {
        // `\\u` is a literal backslash followed by 'u', not an escape sequence.
        let msg = r#"{"id":3,"result":{"value":"C:\\users"}}"#;
        assert!(sanitize_lone_surrogates(msg).is_none());
    }

    #[test]
    fn rewrites_lone_low_surrogate() {
        let msg = r#"{"id":4,"result":{"value":"\udc00"}}"#;
        let clean = sanitize_lone_surrogates(msg).expect("lone low surrogate should be rewritten");
        let parsed: CdpMessage = serde_json::from_str(&clean).unwrap();
        assert_eq!(
            parsed.result.unwrap()["value"].as_str().unwrap(),
            "\u{FFFD}"
        );
    }

    #[test]
    fn survives_backslash_escape_before_a_multi_byte_character() {
        // `\<multi-byte>` is not valid JSON, but a broken relay can produce it.
        // The scan skips two bytes past a backslash escape, which lands inside
        // the character; nothing may slice or index on that offset.
        let msg = "{\"x\":\"\\ud83c\",\"y\":\"\\\u{4E2D}\"}";
        let clean = sanitize_lone_surrogates(msg).expect("lone surrogate should be rewritten");
        assert!(clean.contains('\u{FFFD}'));
        assert!(clean.contains('\u{4E2D}'));
    }

    #[test]
    fn handles_three_consecutive_surrogate_escapes() {
        // high + high + low: the first is unpaired, the second forms a pair.
        let msg = r#"{"id":12,"result":{"value":"\ud83c\ud83c\udf00"}}"#;
        let clean = sanitize_lone_surrogates(msg).expect("first surrogate should be rewritten");
        let parsed: CdpMessage = serde_json::from_str(&clean).expect("should parse");
        assert_eq!(
            parsed.result.unwrap()["value"].as_str().unwrap(),
            "\u{FFFD}\u{1F300}"
        );
    }

    #[test]
    fn handles_uppercase_hex_escapes() {
        // Chrome emits lowercase, but the protocol does not require it.
        let msg = r#"{"id":13,"result":{"value":"\uD83C"}}"#;
        let clean = sanitize_lone_surrogates(msg).expect("uppercase escape should be rewritten");
        let parsed: CdpMessage = serde_json::from_str(&clean).expect("should parse");
        assert_eq!(
            parsed.result.unwrap()["value"].as_str().unwrap(),
            "\u{FFFD}"
        );
    }

    #[test]
    fn extracts_command_id_from_parseable_frame() {
        // The reader hands this the sanitized text, so the frame parses as
        // generic JSON even when it does not fit CdpMessage.
        assert_eq!(
            extract_command_id(r#"{ "id" : 42 , "result":{}}"#),
            Some(42)
        );
        let sanitized = sanitize_lone_surrogates(LONE_SURROGATE_RESPONSE).unwrap();
        assert_eq!(extract_command_id(&sanitized), Some(7));
    }

    #[test]
    fn ignores_ids_nested_inside_the_payload() {
        // A substring scan would return 123 here and fail an unrelated command.
        assert_eq!(
            extract_command_id(r#"{"result":{"root":{"id":123}},"id":7}"#),
            Some(7)
        );
    }

    #[test]
    fn extracts_no_id_from_event_frames() {
        // Events carry no id; they must not resolve a pending command.
        assert_eq!(
            extract_command_id(r#"{"method":"Page.loadEventFired","params":{}}"#),
            None
        );
        // Negative ids belong to the inspect proxy and do not fit u64.
        assert_eq!(extract_command_id(r#"{"id":-3,"result":{}}"#), None);
    }
}
