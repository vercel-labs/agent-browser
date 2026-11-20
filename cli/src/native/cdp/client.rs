use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::net::Shutdown;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::de::IgnoredAny;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;

use super::types::{CdpCommand, CdpError, CdpEvent, CdpMessage};

/// A command awaiting its response, with the session it was sent to so the
/// reader can fail it as soon as that session detaches (#1991).
struct PendingCommand<T> {
    session_id: Option<String>,
    tx: oneshot::Sender<T>,
}

type PendingMap = Arc<Mutex<HashMap<u64, PendingCommand<CdpMessage>>>>;

/// Responses whose caller wants the raw JSON text rather than a parsed
/// `serde_json::Value`. The accessibility tree of a large page is several
/// megabytes of JSON, and building a `Value` from it only to deserialize it
/// again costs more than the command itself, so those callers parse once.
type RawPendingMap = Arc<Mutex<HashMap<u64, PendingCommand<String>>>>;

/// Detached sessions remembered so a later command to one fails at once.
/// A page that keeps replacing a cross-site iframe detaches thousands of
/// sessions, so the oldest are forgotten past this count.
const DETACHED_SESSION_LIMIT: usize = 1024;

/// Sessions Chrome reported with `Target.detachedFromTarget`. Chrome does
/// not answer a command sent to a detached session, so without this a
/// command sent after the detach waits out the whole command timeout.
#[derive(Default)]
struct DetachedSessions {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl DetachedSessions {
    fn insert(&mut self, session_id: &str) {
        if !self.ids.insert(session_id.to_string()) {
            return;
        }
        self.order.push_back(session_id.to_string());
        if self.order.len() > DETACHED_SESSION_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.ids.remove(&oldest);
            }
        }
    }

    fn remove(&mut self, session_id: &str) {
        if self.ids.remove(session_id) {
            self.order.retain(|known| known != session_id);
        }
    }

    fn contains(&self, session_id: &str) -> bool {
        self.ids.contains(session_id)
    }
}

type DetachedSessionSet = Arc<std::sync::Mutex<DetachedSessions>>;

const SESSION_DETACHED_PREFIX: &str = "Target closed: CDP session";

fn session_detached_message(session_id: &str) -> String {
    format!("{} {} detached", SESSION_DETACHED_PREFIX, session_id)
}

/// Chrome's protocol error for a command sent to a session it no longer has.
const SESSION_NOT_FOUND_MESSAGE: &str = "Session with given id not found";

/// Whether a command failed because its CDP session no longer exists: the
/// session detached before or while the command was pending, or Chrome no
/// longer knows the session id.
///
/// Only the whole error that `send_command` returns for such a command
/// matches. Other errors can carry page-controlled text (for example the
/// description of an exception thrown while a script was injected), and
/// that text must not pass as a detach.
pub fn is_session_detached_error(error: &str) -> bool {
    let Some((method, reason)) = error
        .strip_prefix("CDP error (")
        .and_then(|rest| rest.split_once("): "))
    else {
        return false;
    };
    if method.is_empty()
        || !method
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
    {
        return false;
    }
    if let Some(session_id) = reason
        .strip_prefix(SESSION_DETACHED_PREFIX)
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(|rest| rest.strip_suffix(" detached"))
    {
        return !session_id.is_empty() && !session_id.contains(char::is_whitespace);
    }
    reason
        .strip_prefix(SESSION_NOT_FOUND_MESSAGE)
        .is_some_and(|rest| rest.is_empty() || rest == ".")
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

/// Sessions whose events go to one private receiver instead of the broadcast.
type PrivateSessions = Arc<std::sync::Mutex<HashMap<String, mpsc::Sender<CdpEvent>>>>;

/// Events buffered for a private session receiver that has fallen behind.
/// Newer events are dropped once it is full; a screencast consumer that
/// cannot keep up should lose frames, not stall the reader.
const PRIVATE_SESSION_BUFFER: usize = 16;

/// Lossless taps for events that must each get a reply (paused requests,
/// auth challenges, targets waiting for the debugger). Unlike the bounded
/// broadcast, a tap never drops an event when its reader falls behind.
type MethodTaps =
    Arc<std::sync::Mutex<Vec<(&'static [&'static str], mpsc::UnboundedSender<CdpEvent>)>>>;

/// Interval between WebSocket ping frames sent to keep the connection alive
/// through intermediate proxies (reverse proxies, load balancers, service meshes).
const WS_KEEPALIVE_INTERVAL_SECS: u64 = 30;
const TRANSPORT_CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const CONNECTION_CLOSED_ERROR: &str = "CDP connection closed";

/// Upper bound on the CDP WebSocket handshake (TCP connect plus HTTP upgrade).
/// An endpoint that accepts the TCP connection but never answers the upgrade
/// request would otherwise park the caller, and one layer up the whole daemon
/// command loop, forever (#1713). Matches the 30s per-command response
/// timeout in `send_command`.
const CDP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

fn normalize_websocket_root_path(url: &str) -> String {
    let Some(scheme_end) = url.find("://").map(|index| index + 3) else {
        return url.to_string();
    };
    let authority = &url[scheme_end..];
    let Some(query_offset) = authority.find('?') else {
        return url.to_string();
    };
    if authority[..query_offset].contains('/') {
        return url.to_string();
    }
    let query_index = scheme_end + query_offset;
    format!("{}/{}", &url[..query_index], &url[query_index..])
}

/// Raw incoming CDP message (text) broadcast to all subscribers.
/// Used by the inspect proxy to forward responses and events to DevTools.
#[derive(Debug, Clone)]
pub struct RawCdpMessage {
    pub text: String,
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
struct CdpIdEnvelope {
    id: Option<u64>,
    #[serde(rename = "result")]
    _result: Option<IgnoredAny>,
    #[serde(rename = "error")]
    _error: Option<IgnoredAny>,
    #[serde(rename = "method")]
    _method: Option<IgnoredAny>,
    #[serde(rename = "params")]
    _params: Option<IgnoredAny>,
    #[serde(rename = "sessionId")]
    _session_id: Option<IgnoredAny>,
}

fn sanitize_lone_surrogates(msg: &str) -> Option<String> {
    const ESCAPE_LEN: usize = 6;
    const HIGH_START: u32 = 0xD800;
    const LOW_START: u32 = 0xDC00;
    const SURROGATE_END: u32 = 0xE000;

    let read_escape = |bytes: &[u8], at: usize| -> Option<u32> {
        let end = at.checked_add(ESCAPE_LEN)?;
        if end > bytes.len() || bytes[at] != b'\\' || bytes[at + 1] != b'u' {
            return None;
        }
        let hex = &bytes[at + 2..end];
        if !hex.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()
    };

    let bytes = msg.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] != b'u' {
            i += 2;
            continue;
        }

        let Some(unit) = read_escape(bytes, i) else {
            i += 1;
            continue;
        };

        let is_high = (HIGH_START..LOW_START).contains(&unit);
        let is_low = (LOW_START..SURROGATE_END).contains(&unit);
        let paired = is_high
            && read_escape(bytes, i + ESCAPE_LEN)
                .is_some_and(|next| (LOW_START..SURROGATE_END).contains(&next));

        if paired {
            i += ESCAPE_LEN * 2;
            continue;
        }

        if is_high || is_low {
            let buf = out.get_or_insert_with(String::new);
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

fn extract_command_id(msg: &str) -> Option<u64> {
    let envelope: CdpIdEnvelope = serde_json::from_str(msg).ok()?;
    envelope.id.filter(|id| *id > 0)
}

/// Response envelope for [`CdpClient::send_command_typed_raw`]. Unknown fields,
/// `sessionId` among them, are ignored.
#[derive(Deserialize)]
struct RawCommandResponse<T> {
    result: Option<T>,
    error: Option<CdpError>,
}

/// Read the command id from a CDP response without parsing the message.
///
/// Responses start with `{"id":<number>,`, so the id is available from the
/// first bytes even when the payload is megabytes of JSON. Returns `None` for
/// anything that is not shaped like a response envelope, which sends the
/// message down the normal parsed path.
fn leading_response_id(msg: &str) -> Option<u64> {
    let rest = msg.strip_prefix("{\"id\":")?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 || end > 20 {
        return None;
    }
    if !matches!(rest.as_bytes().get(end), None | Some(b',') | Some(b'}')) {
        return None;
    }
    rest[..end].parse::<u64>().ok().filter(|id| *id > 0)
}

/// Mark `session_id` detached and fail every command still waiting on it.
/// Both pending maps stay locked while the session is marked, and senders
/// check the mark under the same lock before registering, so a command is
/// either failed here or refused at send time; it never waits out the
/// timeout in between.
async fn release_detached_session(
    pending: &PendingMap,
    raw_pending: &RawPendingMap,
    detached: &DetachedSessionSet,
    session_id: &str,
) {
    let mut pending = pending.lock().await;
    let mut raw_pending = raw_pending.lock().await;
    detached
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session_id);

    let message = session_detached_message(session_id);
    let ids: Vec<u64> = pending
        .iter()
        .filter(|(_, command)| command.session_id.as_deref() == Some(session_id))
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        if let Some(command) = pending.remove(&id) {
            let _ = command.tx.send(CdpMessage {
                id: Some(id),
                result: None,
                error: Some(CdpError {
                    code: None,
                    message: message.clone(),
                    data: None,
                }),
                method: None,
                params: None,
                session_id: Some(session_id.to_string()),
            });
        }
    }

    let raw_ids: Vec<u64> = raw_pending
        .iter()
        .filter(|(_, command)| command.session_id.as_deref() == Some(session_id))
        .map(|(id, _)| *id)
        .collect();
    for id in raw_ids {
        if let Some(command) = raw_pending.remove(&id) {
            let _ = command
                .tx
                .send(serde_json::json!({ "id": id, "error": { "message": message } }).to_string());
        }
    }
}

pub struct CdpClient {
    ws_tx: WsTx,
    socket: socket2::Socket,
    closed: Arc<AtomicBool>,
    close_complete: AtomicBool,
    close_lock: Mutex<()>,
    cancel_tx: watch::Sender<bool>,
    next_id: AtomicU64,
    pending: PendingMap,
    raw_pending: RawPendingMap,
    event_tx: broadcast::Sender<CdpEvent>,
    raw_tx: broadcast::Sender<RawCdpMessage>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    keepalive_handle: Mutex<Option<JoinHandle<()>>>,
    private_sessions: PrivateSessions,
    method_taps: MethodTaps,
    detached_sessions: DetachedSessionSet,
}

/// Removes a pending entry if `send_command` is cancelled mid-await (e.g. an
/// outer timeout on the liveness probe), so a command whose response never
/// comes can't leak until the connection closes (#1528). Normal exits disarm
/// it via `done`.
struct PendingGuard {
    pending: PendingMap,
    raw_pending: RawPendingMap,
    id: u64,
    done: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let pending = self.pending.clone();
        let raw_pending = self.raw_pending.clone();
        let id = self.id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                pending.lock().await.remove(&id);
                raw_pending.lock().await.remove(&id);
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
        let normalized_url = normalize_websocket_root_path(url);
        let mut request = normalized_url
            .as_str()
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

        Self::connect_request(request, ws_config, None, CDP_HANDSHAKE_TIMEOUT).await
    }

    async fn connect_request(
        request: Request<()>,
        ws_config: WebSocketConfig,
        connector: Option<Connector>,
        handshake_timeout: Duration,
    ) -> Result<Self, String> {
        let (ws_stream, _) = tokio::time::timeout(
            handshake_timeout,
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(ws_config),
                false,
                connector,
            ),
        )
        .await
        .map_err(|_| {
            format!(
                "CDP WebSocket connect failed: no handshake response within {}s (endpoint accepted the connection but never completed the WebSocket upgrade)",
                handshake_timeout.as_secs()
            )
        })?
        .map_err(|e| format!("CDP WebSocket connect failed: {}", e))?;

        enable_tcp_keepalive(ws_stream.get_ref());
        let tcp_stream = underlying_tcp_stream(ws_stream.get_ref())
            .ok_or_else(|| "Unsupported CDP stream type".to_string())?;
        let socket = socket2::SockRef::from(tcp_stream)
            .try_clone()
            .map_err(|error| format!("Failed to duplicate CDP socket: {}", error))?;

        let (ws_tx, mut ws_rx) = ws_stream.split();
        let ws_tx = Arc::new(Mutex::new(ws_tx));

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let raw_pending: RawPendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(4096);
        let (raw_tx, _) = broadcast::channel(4096);
        let closed = Arc::new(AtomicBool::new(false));

        let private_sessions: PrivateSessions = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let method_taps: MethodTaps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let detached_sessions: DetachedSessionSet = Arc::default();

        let pending_clone = pending.clone();
        let raw_pending_clone = raw_pending.clone();
        let event_tx_clone = event_tx.clone();
        let raw_tx_clone = raw_tx.clone();
        let closed_reader = closed.clone();
        let private_clone = private_sessions.clone();
        let taps_clone = method_taps.clone();
        let detached_clone = detached_sessions.clone();

        // Notify used to stop the keepalive task when the reader loop exits.
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let reader_cancel_tx = cancel_tx.clone();

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

                // Responses registered for raw delivery skip the `Value` parse
                // entirely: the caller deserializes the payload once, straight
                // from this text (large accessibility trees).
                if let Some(id) = leading_response_id(&msg) {
                    let waiter = raw_pending_clone.lock().await.remove(&id);
                    if let Some(waiter) = waiter {
                        let _ = waiter.tx.send(msg);
                        continue;
                    }
                }

                let parsed: CdpMessage = match serde_json::from_str(&msg) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        let repaired = sanitize_lone_surrogates(&msg);
                        let candidate = repaired.as_deref().unwrap_or(&msg);
                        match serde_json::from_str(candidate) {
                            Ok(parsed) => parsed,
                            Err(error) => {
                                if let Some(id) = extract_command_id(candidate) {
                                    // Hand the unparsable text to a raw waiter
                                    // so it fails fast instead of timing out.
                                    let raw_waiter = raw_pending_clone.lock().await.remove(&id);
                                    if let Some(waiter) = raw_waiter {
                                        let _ = waiter.tx.send(candidate.to_string());
                                        continue;
                                    }
                                    let waiter = pending_clone.lock().await.remove(&id);
                                    if let Some(waiter) = waiter {
                                        let _ = waiter.tx.send(CdpMessage {
                                            id: Some(id),
                                            result: None,
                                            error: Some(CdpError {
                                                code: None,
                                                message: format!(
                                                    "Malformed CDP response: {}",
                                                    error
                                                ),
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
                    // Responses from servers that do not lead with the id still
                    // reach a raw waiter, at the cost of the parse above.
                    let raw_waiter = raw_pending_clone.lock().await.remove(&id);
                    if let Some(waiter) = raw_waiter {
                        let _ = waiter.tx.send(msg);
                        continue;
                    }
                    // Response to a command
                    let mut pending = pending_clone.lock().await;
                    if let Some(waiter) = pending.remove(&id) {
                        let _ = waiter.tx.send(parsed);
                    }
                } else if let Some(ref method) = parsed.method {
                    // Event
                    let event = CdpEvent {
                        method: method.clone(),
                        params: parsed.params.clone().unwrap_or(Value::Null),
                        session_id: parsed.session_id.clone(),
                    };
                    // Chrome never answers a command to a detached session.
                    // Settle the session before subscribers see the event, so
                    // a handler reacting to it cannot send into the gap.
                    let target_session = event.params.get("sessionId").and_then(Value::as_str);
                    match (event.method.as_str(), target_session) {
                        ("Target.detachedFromTarget", Some(sid)) => {
                            release_detached_session(
                                &pending_clone,
                                &raw_pending_clone,
                                &detached_clone,
                                sid,
                            )
                            .await;
                        }
                        ("Target.attachedToTarget", Some(sid)) => {
                            detached_clone
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .remove(sid);
                        }
                        _ => {}
                    }
                    let routed = event.session_id.as_deref().is_some_and(|sid| {
                        let mut routes = private_clone.lock().unwrap_or_else(|e| e.into_inner());
                        match routes.get(sid) {
                            Some(tx) => match tx.try_send(event.clone()) {
                                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    routes.remove(sid);
                                    false
                                }
                            },
                            None => false,
                        }
                    });
                    if !routed {
                        let mut taps = taps_clone.lock().unwrap_or_else(|e| e.into_inner());
                        taps.retain(|(methods, tx)| {
                            !methods.contains(&event.method.as_str())
                                || tx.send(event.clone()).is_ok()
                        });
                        drop(taps);
                        let _ = event_tx_clone.send(event);
                    }
                }
            }

            // Reader loop exited (connection closed or error). Drop all pending
            // command senders so callers get an immediate channel-closed error
            // instead of waiting for the 30-second timeout.
            closed_reader.store(true, Ordering::SeqCst);
            pending_clone.lock().await.clear();
            raw_pending_clone.lock().await.clear();

            // Stop the keepalive task because the connection is gone.
            let _ = reader_cancel_tx.send(true);
        });

        // Spawn a keepalive task that sends WebSocket Ping frames at a regular
        // interval. This prevents intermediate proxies (Envoy, nginx, OpenResty,
        // cloud load balancers) from closing idle WebSocket connections. If the
        // send fails, the connection is dead and we stop pinging.
        let keepalive_tx = ws_tx.clone();
        let closed_keepalive = closed.clone();
        let keepalive_handle = tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(WS_KEEPALIVE_INTERVAL_SECS);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = cancel_rx.changed() => break,
                }
                let mut tx = keepalive_tx.lock().await;
                if closed_keepalive.load(Ordering::SeqCst) {
                    break;
                }
                if tx.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            ws_tx,
            socket,
            closed,
            close_complete: AtomicBool::new(false),
            close_lock: Mutex::new(()),
            cancel_tx,
            next_id: AtomicU64::new(1),
            pending,
            raw_pending,
            event_tx,
            raw_tx,
            reader_handle: Mutex::new(Some(reader_handle)),
            keepalive_handle: Mutex::new(Some(keepalive_handle)),
            private_sessions,
            method_taps,
            detached_sessions,
        })
    }

    /// Refuse a command to a session Chrome already reported detached.
    /// Callers hold the pending map that the command registers in, which
    /// orders this check against [`release_detached_session`].
    fn ensure_session_attached(
        &self,
        method: &str,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        let Some(session_id) = session_id else {
            return Ok(());
        };
        let detached = self
            .detached_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(session_id);
        if detached {
            Err(format!(
                "CDP error ({}): {}",
                method,
                session_detached_message(session_id)
            ))
        } else {
            Ok(())
        }
    }

    fn ensure_open(&self) -> Result<(), String> {
        if self.closed.load(Ordering::SeqCst) {
            Err(CONNECTION_CLOSED_ERROR.to_string())
        } else {
            Ok(())
        }
    }

    async fn abort_task(handle: &Mutex<Option<JoinHandle<()>>>) {
        let Some(task) = handle.lock().await.take() else {
            return;
        };
        task.abort();
        let _ = tokio::time::timeout(TRANSPORT_CLOSE_TIMEOUT, task).await;
    }

    pub async fn close(&self) {
        let _close_guard = self.close_lock.lock().await;
        if self.close_complete.load(Ordering::SeqCst) {
            return;
        }
        self.closed.store(true, Ordering::SeqCst);

        let _ = self.cancel_tx.send(true);
        Self::abort_task(&self.keepalive_handle).await;

        let _ = tokio::time::timeout(TRANSPORT_CLOSE_TIMEOUT, async {
            let mut ws_tx = self.ws_tx.lock().await;
            let _ = ws_tx.close().await;
        })
        .await;

        let _ = self.socket.shutdown(Shutdown::Both);
        Self::abort_task(&self.reader_handle).await;
        self.pending.lock().await.clear();
        self.raw_pending.lock().await.clear();
        self.close_complete.store(true, Ordering::SeqCst);
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

        // Cleans up the pending entry if this future is cancelled mid-await (#1528).
        let mut guard = PendingGuard {
            pending: self.pending.clone(),
            raw_pending: self.raw_pending.clone(),
            id,
            done: false,
        };

        {
            let mut ws_tx = self.ws_tx.lock().await;
            self.ensure_open()?;
            let mut pending = self.pending.lock().await;
            if let Err(error) = self.ensure_session_attached(method, cmd.session_id.as_deref()) {
                guard.done = true;
                return Err(error);
            }
            pending.insert(
                id,
                PendingCommand {
                    session_id: cmd.session_id.clone(),
                    tx,
                },
            );
            drop(pending);
            if let Err(error) = ws_tx.send(Message::Text(json)).await {
                self.pending.lock().await.remove(&id);
                guard.done = true;
                return Err(format!("Failed to send CDP command: {}", error));
            }
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

    /// Deliver every event whose method is in `methods` to the returned
    /// receiver without loss. The events still reach the broadcast. Dropping
    /// the receiver ends the tap on the next matching event.
    pub fn subscribe_methods(
        &self,
        methods: &'static [&'static str],
    ) -> mpsc::UnboundedReceiver<CdpEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.method_taps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((methods, tx));
        rx
    }

    /// Deliver every event carrying `session_id` to the returned receiver
    /// instead of the broadcast, so a high-volume session (a screencast) is
    /// neither cloned to every subscriber nor able to overflow their buffers.
    /// Events without a session id are unaffected. Call
    /// [`unsubscribe_session`](Self::unsubscribe_session) when done; a dropped
    /// receiver also ends the route on the next event for that session.
    pub fn subscribe_session(&self, session_id: &str) -> mpsc::Receiver<CdpEvent> {
        let (tx, rx) = mpsc::channel(PRIVATE_SESSION_BUFFER);
        self.private_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), tx);
        rx
    }

    pub fn unsubscribe_session(&self, session_id: &str) {
        self.private_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
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
            closed: self.closed.clone(),
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

    /// Send a command and deserialize its `result` straight from the response
    /// text, skipping the intermediate `serde_json::Value`.
    ///
    /// The reader hands the response over unparsed, so the payload is
    /// traversed once instead of twice. Use this for commands whose responses
    /// can be megabytes (the full accessibility tree of a large page).
    pub async fn send_command_typed_raw<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        session_id: Option<&str>,
    ) -> Result<R, String> {
        let params_value = serde_json::to_value(params)
            .map_err(|e| format!("Failed to serialize params: {}", e))?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cmd = CdpCommand {
            id,
            method: method.to_string(),
            params: Some(params_value),
            session_id: session_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        };
        let json = serde_json::to_string(&cmd)
            .map_err(|e| format!("Failed to serialize CDP command: {}", e))?;

        let (tx, rx) = oneshot::channel();

        // Cleans up the pending entry if this future is cancelled mid-await (#1528).
        let mut guard = PendingGuard {
            pending: self.pending.clone(),
            raw_pending: self.raw_pending.clone(),
            id,
            done: false,
        };

        {
            let mut ws_tx = self.ws_tx.lock().await;
            self.ensure_open()?;
            let mut raw_pending = self.raw_pending.lock().await;
            if let Err(error) = self.ensure_session_attached(method, cmd.session_id.as_deref()) {
                guard.done = true;
                return Err(error);
            }
            raw_pending.insert(
                id,
                PendingCommand {
                    session_id: cmd.session_id.clone(),
                    tx,
                },
            );
            drop(raw_pending);
            if let Err(error) = ws_tx.send(Message::Text(json)).await {
                self.raw_pending.lock().await.remove(&id);
                guard.done = true;
                return Err(format!("Failed to send CDP command: {}", error));
            }
        }

        let text = match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(text)) => {
                guard.done = true;
                text
            }
            Ok(Err(_)) => {
                guard.done = true;
                return Err("CDP response channel closed".to_string());
            }
            Err(_) => {
                guard.done = true;
                self.raw_pending.lock().await.remove(&id);
                return Err(format!("CDP command timed out: {}", method));
            }
        };

        let parse = |text: &str| -> Result<RawCommandResponse<R>, String> {
            serde_json::from_str(text)
                .map_err(|e| format!("Failed to deserialize CDP response for {}: {}", method, e))
        };
        // Chrome can emit lone surrogate escapes; repair them the same way the
        // shared reader does and retry once.
        let response = parse(&text).or_else(|error| match sanitize_lone_surrogates(&text) {
            Some(repaired) => parse(&repaired),
            None => Err(error),
        })?;

        if let Some(error) = response.error {
            return Err(format!("CDP error ({}): {}", method, error));
        }
        response.result.ok_or_else(|| {
            format!(
                "Failed to deserialize CDP response for {}: missing result",
                method
            )
        })
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
        self.ensure_open()?;
        ws_tx
            .send(Message::Text(json))
            .await
            .map_err(|e| format!("Failed to send CDP command: {}", e))
    }

    /// Send raw JSON through the WebSocket without tracking a response.
    /// Used by the inspect proxy to forward DevTools frontend messages.
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        let mut ws_tx = self.ws_tx.lock().await;
        self.ensure_open()?;
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

    /// Test-only: count of in-flight raw commands still awaiting a response,
    /// so a test can assert close, reader EOF and cancellation released them.
    #[cfg(test)]
    pub(crate) async fn raw_pending_len(&self) -> usize {
        self.raw_pending.lock().await.len()
    }
}

/// Lightweight handle for the inspect WebSocket proxy, holding only
/// the cloneable parts of CdpClient needed for bidirectional message forwarding.
pub struct InspectProxyHandle {
    ws_tx: WsTx,
    raw_tx: broadcast::Sender<RawCdpMessage>,
    closed: Arc<AtomicBool>,
}

impl InspectProxyHandle {
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        let mut ws_tx = self.ws_tx.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(CONNECTION_CLOSED_ERROR.to_string());
        }
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
    let Some(tcp_stream) = underlying_tcp_stream(stream) else {
        return;
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

fn underlying_tcp_stream(
    stream: &tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
) -> Option<&tokio::net::TcpStream> {
    match stream {
        tokio_tungstenite::MaybeTlsStream::Plain(stream) => Some(stream),
        tokio_tungstenite::MaybeTlsStream::Rustls(stream) => Some(stream.get_ref().0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use serde_json::json;
    use std::io::Read;
    use std::net::TcpListener as StdTcpListener;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    const TEST_CERT_DER: &str = "MIIBPDCB46ADAgECAgkA9xwXmIhbzQIwCgYIKoZIzj0EAwIwFDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDgzMDA1NTU0NFoXDTM2MDgyNzA1NTU0NFowFDESMBAGA1UEAwwJbG9jYWxob3N0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEQdmPO0sXXFRmVn7+8MViXGom2rYy8aV3Oc0pNZhUmFGaPl4MRFzc1G0yaV/WRBPF/BUh2LGsXCvUsn7NqaYkFaMeMBwwGgYDVR0RBBMwEYIJbG9jYWxob3N0hwR/AAABMAoGCCqGSM49BAMCA0gAMEUCIQC32oureGNAupABEmonQPAQBD7OdJjXmUxoVQNr0UB+6AIgb23GMwxTjwHyLG4tZcJST7r3cCW8K/Y+1h618j2f+Uw=";
    const TEST_KEY_DER: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgIVZST8Vhc5f3OW+4kF78f8o1nQCRwjW/Yo8CXQ35Y9uhRANCAARB2Y87SxdcVGZWfv7wxWJcaibatjLxpXc5zSk1mFSYUZo+XgxEXNzUbTJpX9ZEE8X8FSHYsaxcK9Syfs2ppiQV";

    #[test]
    fn repairs_only_unpaired_surrogate_escapes() {
        let msg = r#"{"id":1,"result":{"high":"\ud800","low":"\udfff","pair":"\ud83d\ude00","escaped":"\\ud800","text":"café 中文"}}"#;
        let repaired = sanitize_lone_surrogates(msg).expect("lone surrogates should be repaired");
        let parsed: CdpMessage = serde_json::from_str(&repaired).expect("repaired message");
        let result = parsed.result.expect("result");

        assert_eq!(result["high"], "\u{fffd}");
        assert_eq!(result["low"], "\u{fffd}");
        assert_eq!(result["pair"], "😀");
        assert_eq!(result["escaped"], r"\ud800");
        assert_eq!(result["text"], "café 中文");
    }

    #[test]
    fn extracts_only_positive_top_level_command_ids() {
        assert_eq!(
            extract_command_id(r#"{"id":7,"result":{"value":"\ud800"}}"#),
            Some(7)
        );
        assert_eq!(
            extract_command_id(r#"{"result":{"id":42,"value":"\ud800"}}"#),
            None
        );
        assert_eq!(
            extract_command_id(r#"{"id":-1,"result":{"value":"\ud800"}}"#),
            None
        );
        assert_eq!(
            extract_command_id(r#"{"method":"Fetch.requestPaused","params":{"value":"\ud800"}}"#),
            None
        );
        assert_eq!(extract_command_id(r#"{"id":0,"result":{}}"#), None);
    }

    #[test]
    fn reads_response_ids_without_parsing_the_payload() {
        assert_eq!(
            leading_response_id(r#"{"id":42,"result":{"nodes":[]}}"#),
            Some(42)
        );
        assert_eq!(
            leading_response_id(r#"{"id":42,"result":{"nodes":[]},"sessionId":"s1"}"#),
            Some(42)
        );
        assert_eq!(leading_response_id(r#"{"id":42}"#), Some(42));
        assert_eq!(leading_response_id(r#"{"result":{"id":42}}"#), None);
        assert_eq!(
            leading_response_id(r#"{"method":"Page.loadEventFired","params":{}}"#),
            None
        );
        assert_eq!(leading_response_id(r#"{"id":0,"result":{}}"#), None);
        assert_eq!(leading_response_id(r#"{"id":-1,"result":{}}"#), None);
        assert_eq!(leading_response_id(r#"{"id":"42","result":{}}"#), None);
        assert_eq!(leading_response_id(""), None);
        assert_eq!(
            leading_response_id(r#"{"id":12345678901234567890123,"result":{}}"#),
            None
        );
    }

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct RawNodes {
        nodes: Vec<u32>,
    }

    #[tokio::test]
    async fn raw_command_parses_leading_id_responses_and_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let request: Value = serde_json::from_str(text).unwrap();
                let id = request["id"].as_u64().unwrap();
                let reply = match request["method"].as_str().unwrap() {
                    "Raw.Ok" => format!(r#"{{"id":{id},"result":{{"nodes":[1,2,3]}}}}"#),
                    "Raw.Error" => {
                        format!(r#"{{"id":{id},"error":{{"code":-32000,"message":"boom"}}}}"#)
                    }
                    // A proxy that puts sessionId first still reaches the waiter.
                    "Raw.LateId" => {
                        format!(r#"{{"sessionId":"s1","id":{id},"result":{{"nodes":[4]}}}}"#)
                    }
                    "Raw.Repaired" => {
                        format!(r#"{{"id":{id},"result":{{"nodes":[5],"name":"\ud800"}}}}"#)
                    }
                    _ => format!(r#"{{"id":{id},"result":{{}}}}"#),
                };
                ws.send(Message::Text(reply)).await.unwrap();
            }
        });

        let client = CdpClient::connect(&url).await.unwrap();

        let ok: RawNodes = client
            .send_command_typed_raw("Raw.Ok", &json!({}), None)
            .await
            .unwrap();
        assert_eq!(
            ok,
            RawNodes {
                nodes: vec![1, 2, 3]
            }
        );

        let error = client
            .send_command_typed_raw::<_, RawNodes>("Raw.Error", &json!({}), None)
            .await
            .unwrap_err();
        assert!(error.contains("boom"), "unexpected error: {error}");

        let late: RawNodes = client
            .send_command_typed_raw("Raw.LateId", &json!({}), None)
            .await
            .unwrap();
        assert_eq!(late, RawNodes { nodes: vec![4] });

        // A raw waiter whose payload needs surrogate repair still deserializes.
        let repaired: RawNodes = client
            .send_command_typed_raw("Raw.Repaired", &json!({}), None)
            .await
            .unwrap();
        assert_eq!(repaired, RawNodes { nodes: vec![5] });

        client.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn repairs_response_and_event_then_processes_next_command() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            let first = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let first_id = serde_json::from_str::<Value>(&first).unwrap()["id"]
                .as_u64()
                .unwrap();
            ws.send(Message::Text(format!(
                r#"{{"id":{first_id},"result":{{"value":"\ud800"}}}}"#
            )))
            .await
            .unwrap();
            ws.send(Message::Text(
                r#"{"method":"Fetch.requestPaused","params":{"requestId":"r1","request":{"url":"https://example.test/\udfff"}},"sessionId":"s1"}"#.to_string(),
            ))
            .await
            .unwrap();

            let second = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let second_id = serde_json::from_str::<Value>(&second).unwrap()["id"]
                .as_u64()
                .unwrap();
            ws.send(Message::Text(
                json!({"id": second_id, "result": {"value": "ok"}}).to_string(),
            ))
            .await
            .unwrap();
        });

        let client = CdpClient::connect(&url).await.unwrap();
        let mut events = client.subscribe();

        let first = tokio::time::timeout(
            Duration::from_secs(1),
            client.send_command_no_params("Accessibility.getFullAXTree", None),
        )
        .await
        .expect("surrogate response should complete")
        .unwrap();
        assert_eq!(first["value"], "\u{fffd}");

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("surrogate event should arrive")
            .unwrap();
        assert_eq!(event.method, "Fetch.requestPaused");
        assert_eq!(
            event.params["request"]["url"],
            "https://example.test/\u{fffd}"
        );

        let second = client
            .send_command_no_params("Browser.getVersion", None)
            .await
            .unwrap();
        assert_eq!(second["value"], "ok");

        client.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_response_fails_only_its_top_level_pending_command() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            let first = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let first_id = serde_json::from_str::<Value>(&first).unwrap()["id"]
                .as_u64()
                .unwrap();
            ws.send(Message::Text(format!(
                r#"{{"id":{first_id},"result":{{"value":"\ud800"}},"method":42}}"#
            )))
            .await
            .unwrap();

            let second = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let second_id = serde_json::from_str::<Value>(&second).unwrap()["id"]
                .as_u64()
                .unwrap();
            ws.send(Message::Text(format!(
                r#"{{"result":{{"id":{second_id},"value":"\ud800"}},"method":42}}"#
            )))
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({"id": second_id, "result": {"value": "still-open"}}).to_string(),
            ))
            .await
            .unwrap();
        });

        let client = CdpClient::connect(&url).await.unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            client.send_command_no_params("Broken.response", None),
        )
        .await
        .expect("malformed response should not time out")
        .unwrap_err();
        assert!(error.contains("Malformed CDP response"), "{error}");

        let result = client
            .send_command_no_params("Browser.getVersion", None)
            .await
            .unwrap();
        assert_eq!(result["value"], "still-open");

        client.close().await;
        server.await.unwrap();
    }

    /// A command pending on a session that detaches fails at once (#1991),
    /// for both response paths, while another session's command still gets
    /// its answer.
    #[tokio::test]
    async fn detach_fails_only_that_sessions_pending_commands() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut commands = Vec::new();
            for _ in 0..3 {
                let text = ws.next().await.unwrap().unwrap().into_text().unwrap();
                commands.push(serde_json::from_str::<Value>(&text).unwrap());
            }
            // Chrome detaches the frame session and never answers its
            // commands.
            ws.send(Message::Text(
                json!({
                    "method": "Target.detachedFromTarget",
                    "params": { "sessionId": "S-FRAME", "targetId": "F" },
                    "sessionId": "S-PAGE"
                })
                .to_string(),
            ))
            .await
            .unwrap();
            let page = commands
                .iter()
                .find(|command| command["sessionId"] == "S-PAGE")
                .unwrap();
            ws.send(Message::Text(
                json!({"id": page["id"], "result": {"value": "page"}}).to_string(),
            ))
            .await
            .unwrap();
            while ws.next().await.is_some() {}
        });

        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let frame = {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .send_command_no_params("Page.enable", Some("S-FRAME"))
                    .await
            })
        };
        let frame_raw = {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .send_command_typed_raw::<_, RawNodes>(
                        "Accessibility.getFullAXTree",
                        &json!({}),
                        Some("S-FRAME"),
                    )
                    .await
            })
        };
        let page = {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .send_command_no_params("Runtime.evaluate", Some("S-PAGE"))
                    .await
            })
        };

        let wait = Duration::from_secs(2);
        let error = tokio::time::timeout(wait, frame)
            .await
            .expect("a detached session's command should fail at once")
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error,
            "CDP error (Page.enable): Target closed: CDP session S-FRAME detached"
        );
        assert!(is_session_detached_error(&error));
        let raw_error = tokio::time::timeout(wait, frame_raw)
            .await
            .expect("a detached session's raw command should fail at once")
            .unwrap()
            .unwrap_err();
        assert!(is_session_detached_error(&raw_error), "{raw_error}");
        let result = tokio::time::timeout(wait, page)
            .await
            .expect("the page session's command should still be answered")
            .unwrap()
            .unwrap();
        assert_eq!(result["value"], "page");
        assert_eq!(client.pending_len().await, 0);
        assert_eq!(client.raw_pending_len().await, 0);

        client.close().await;
        server.await.unwrap();
    }

    /// A command to a session whose detach was already read fails without
    /// being sent, since Chrome would never answer it. A session id that is
    /// attached again accepts commands again.
    #[tokio::test]
    async fn command_to_an_already_detached_session_fails_at_send_time() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (reattach_tx, reattach_rx) = oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Text(
                json!({
                    "method": "Target.detachedFromTarget",
                    "params": { "sessionId": "S-GONE", "targetId": "F" }
                })
                .to_string(),
            ))
            .await
            .unwrap();
            reattach_rx.await.unwrap();
            ws.send(Message::Text(
                json!({
                    "method": "Target.attachedToTarget",
                    "params": {
                        "sessionId": "S-GONE",
                        "targetInfo": { "targetId": "F", "type": "iframe" },
                        "waitingForDebugger": true
                    }
                })
                .to_string(),
            ))
            .await
            .unwrap();
            // The first command to reach the socket must be the one sent
            // after the reattach.
            let text = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let command: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(command["method"], "Runtime.enable");
            assert_eq!(command["sessionId"], "S-GONE");
            ws.send(Message::Text(
                json!({"id": command["id"], "result": {}}).to_string(),
            ))
            .await
            .unwrap();
            while ws.next().await.is_some() {}
        });

        let client = CdpClient::connect(&url).await.unwrap();
        let mut events = client.subscribe();
        let detach = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detach.method, "Target.detachedFromTarget");

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            client.send_command_no_params("Page.enable", Some("S-GONE")),
        )
        .await
        .expect("a command to a detached session should not wait")
        .unwrap_err();
        assert_eq!(
            error,
            "CDP error (Page.enable): Target closed: CDP session S-GONE detached"
        );
        let raw_error = tokio::time::timeout(
            Duration::from_secs(1),
            client.send_command_typed_raw::<_, RawNodes>(
                "Accessibility.getFullAXTree",
                &json!({}),
                Some("S-GONE"),
            ),
        )
        .await
        .expect("a raw command to a detached session should not wait")
        .unwrap_err();
        assert!(is_session_detached_error(&raw_error), "{raw_error}");
        assert_eq!(client.pending_len().await, 0);
        assert_eq!(client.raw_pending_len().await, 0);

        reattach_tx.send(()).unwrap();
        let attach = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attach.method, "Target.attachedToTarget");
        tokio::time::timeout(
            Duration::from_secs(2),
            client.send_command_no_params("Runtime.enable", Some("S-GONE")),
        )
        .await
        .expect("a reattached session should be answered")
        .unwrap();

        client.close().await;
        server.await.unwrap();
    }

    #[test]
    fn detached_sessions_forget_the_oldest_past_the_limit() {
        let mut detached = DetachedSessions::default();
        for index in 0..=DETACHED_SESSION_LIMIT {
            detached.insert(&format!("S-{index}"));
        }
        detached.insert("S-1");
        assert!(!detached.contains("S-0"));
        assert!(detached.contains("S-1"));
        assert!(detached.contains(&format!("S-{DETACHED_SESSION_LIMIT}")));
        assert_eq!(detached.ids.len(), DETACHED_SESSION_LIMIT);
        assert_eq!(detached.order.len(), DETACHED_SESSION_LIMIT);

        detached.remove("S-1");
        assert!(!detached.contains("S-1"));
        assert_eq!(detached.order.len(), DETACHED_SESSION_LIMIT - 1);
    }

    #[test]
    fn recognizes_errors_for_sessions_that_no_longer_exist() {
        assert!(is_session_detached_error(&format!(
            "CDP error (Page.enable): {}",
            session_detached_message("S")
        )));
        assert!(is_session_detached_error(
            "CDP error (Page.enable): Session with given id not found."
        ));
        assert!(!is_session_detached_error(
            "CDP command timed out: Page.enable"
        ));
    }

    #[test]
    fn page_controlled_text_is_not_a_detach() {
        // The description of an exception a page throws while the domain
        // filter is injected ends up in the error, so a live frame can put
        // any text there. It must still count as a failure, not a detach.
        for thrown in [
            "Error: Session with given id not found".to_string(),
            format!("Error: {}", session_detached_message("S")),
            format!(
                "Error: CDP error (Page.enable): {}",
                session_detached_message("S")
            ),
            "Error: CDP error (Page.enable): Session with given id not found.".to_string(),
        ] {
            let error = format!(
                "Failed to apply domain filter to the current execution context: {}",
                thrown
            );
            assert!(!is_session_detached_error(&error), "{error}");
        }

        // Extra text after the reason is not one of the client's or
        // Chrome's detach errors either.
        for error in [
            "CDP error (Runtime.evaluate): Session with given id not found. Error: x",
            "CDP error (Runtime.evaluate): Target closed: CDP session S detached: x",
            "CDP error (Runtime.evaluate): Target closed: CDP session  detached",
            "CDP error (): Session with given id not found.",
            "CDP error (Page.enable) x: Session with given id not found.",
        ] {
            assert!(!is_session_detached_error(error), "{error}");
        }
    }

    #[tokio::test]
    async fn close_releases_pending_and_rejects_all_write_paths() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (command_seen_tx, command_seen_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            let _ = command_seen_tx.send(());
            while ws.next().await.is_some() {}
        });

        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let inspect = client.inspect_handle();
        let command_client = client.clone();
        let command = tokio::spawn(async move {
            command_client
                .send_command_no_params("Never.responds", None)
                .await
        });

        command_seen_rx.await.unwrap();
        assert_eq!(client.pending_len().await, 1);
        client.close().await;

        let command_error = tokio::time::timeout(Duration::from_secs(1), command)
            .await
            .expect("pending command should be released")
            .unwrap()
            .unwrap_err();
        assert_eq!(command_error, "CDP response channel closed");
        assert_eq!(client.pending_len().await, 0);

        assert_eq!(
            client
                .send_command_no_params("Browser.getVersion", None)
                .await
                .unwrap_err(),
            CONNECTION_CLOSED_ERROR
        );
        assert_eq!(
            client
                .send_command_no_wait("Browser.getVersion", None, None)
                .await
                .unwrap_err(),
            CONNECTION_CLOSED_ERROR
        );
        assert_eq!(
            client.send_raw("{}".to_string()).await.unwrap_err(),
            CONNECTION_CLOSED_ERROR
        );
        assert_eq!(
            inspect.send_raw("{}".to_string()).await.unwrap_err(),
            CONNECTION_CLOSED_ERROR
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn close_releases_raw_pending_and_rejects_later_raw_commands() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (command_seen_tx, command_seen_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            let _ = command_seen_tx.send(());
            while ws.next().await.is_some() {}
        });

        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let command_client = client.clone();
        let command = tokio::spawn(async move {
            command_client
                .send_command_typed_raw::<_, RawNodes>("Never.responds", &json!({}), None)
                .await
        });

        command_seen_rx.await.unwrap();
        assert_eq!(client.raw_pending_len().await, 1);
        client.close().await;

        let command_error = tokio::time::timeout(Duration::from_secs(1), command)
            .await
            .expect("raw command should be released")
            .unwrap()
            .unwrap_err();
        assert_eq!(command_error, "CDP response channel closed");
        assert_eq!(client.raw_pending_len().await, 0);

        assert_eq!(
            client
                .send_command_typed_raw::<_, RawNodes>("Browser.getVersion", &json!({}), None)
                .await
                .unwrap_err(),
            CONNECTION_CLOSED_ERROR
        );
        assert_eq!(client.raw_pending_len().await, 0);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn reader_eof_releases_raw_pending_commands() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            // Drop the connection without answering the command.
            drop(ws);
        });

        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let command_client = client.clone();
        let command = tokio::spawn(async move {
            command_client
                .send_command_typed_raw::<_, RawNodes>("Never.responds", &json!({}), None)
                .await
        });

        let command_error = tokio::time::timeout(Duration::from_secs(1), command)
            .await
            .expect("reader EOF should release the raw command")
            .unwrap()
            .unwrap_err();
        assert_eq!(command_error, "CDP response channel closed");
        assert_eq!(client.raw_pending_len().await, 0);

        client.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_raw_command_leaves_no_pending_entry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (command_seen_tx, command_seen_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            let _ = command_seen_tx.send(());
            while ws.next().await.is_some() {}
        });

        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let params = json!({});
        let mut command =
            Box::pin(client.send_command_typed_raw::<_, RawNodes>("Never.responds", &params, None));

        tokio::select! {
            _ = command_seen_rx => {}
            result = &mut command => panic!("raw command should not complete: {result:?}"),
        }
        assert_eq!(client.raw_pending_len().await, 1);

        // Drop the future mid-await, as an outer timeout would. The guard
        // clears the entry from a spawned task, so give it a moment to run.
        drop(command);
        tokio::time::timeout(Duration::from_secs(1), async {
            while client.raw_pending_len().await != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled raw command should leave no pending entry");
        assert_eq!(client.pending_len().await, 0);

        client.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_close_sends_close_and_forces_tcp_eof_with_inspect_alive() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let close = tokio::time::timeout(Duration::from_secs(1), ws.next())
                .await
                .expect("server should receive close")
                .expect("stream should yield close")
                .expect("close frame should parse");
            assert!(matches!(close, Message::Close(_)));

            let mut byte = [0u8; 1];
            let count = tokio::time::timeout(Duration::from_secs(1), ws.get_mut().read(&mut byte))
                .await
                .expect("server should observe TCP shutdown")
                .expect("server TCP read should succeed");
            assert_eq!(count, 0);
        });

        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let inspect = client.inspect_handle();
        let close_a = {
            let client = client.clone();
            tokio::spawn(async move { client.close().await })
        };
        let close_b = {
            let client = client.clone();
            tokio::spawn(async move { client.close().await })
        };

        tokio::time::timeout(Duration::from_secs(2), async {
            close_a.await.unwrap();
            close_b.await.unwrap();
            client.close().await;
        })
        .await
        .expect("concurrent close should be bounded");

        server.await.unwrap();
        drop(inspect);
    }

    #[tokio::test]
    async fn close_is_bounded_when_the_sink_is_busy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (accepted_tx, accepted_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = accepted_tx.send(());
            while ws.next().await.is_some() {}
        });

        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        accepted_rx.await.unwrap();
        let sink_guard = client.ws_tx.lock().await;
        let close_client = client.clone();
        tokio::time::timeout(Duration::from_secs(1), async move {
            close_client.close().await;
        })
        .await
        .expect("close should not wait indefinitely for the sink");
        drop(sink_guard);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rustls_close_forces_underlying_tcp_eof() {
        let certificate = CertificateDer::from(
            base64::engine::general_purpose::STANDARD
                .decode(TEST_CERT_DER)
                .unwrap(),
        );
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            base64::engine::general_purpose::STANDARD
                .decode(TEST_KEY_DER)
                .unwrap(),
        ));
        let server_config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![certificate.clone()], key)
                .unwrap(),
        );

        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let connection = rustls::ServerConnection::new(server_config).unwrap();
            let tls = rustls::StreamOwned::new(connection, stream);
            let mut ws = tokio_tungstenite::tungstenite::accept(tls).unwrap();
            assert!(matches!(ws.read().unwrap(), Message::Close(_)));

            let mut byte = [0u8; 1];
            let count = ws
                .get_mut()
                .get_mut()
                .read(&mut byte)
                .expect("server TCP read should succeed");
            assert_eq!(count, 0);
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate).unwrap();
        let client_config = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let request = format!("wss://127.0.0.1:{port}")
            .into_client_request()
            .unwrap();
        let config = WebSocketConfig {
            max_message_size: None,
            max_frame_size: None,
            ..Default::default()
        };
        let client = CdpClient::connect_request(
            request,
            config,
            Some(Connector::Rustls(client_config)),
            CDP_HANDSHAKE_TIMEOUT,
        )
        .await
        .unwrap();

        client.close().await;
        server.join().unwrap();
    }

    /// Regression for #1713: a TCP endpoint that accepts but never answers the
    /// WebSocket upgrade must produce a bounded error instead of parking the
    /// caller (and, one layer up, the daemon command loop) forever.
    #[tokio::test]
    async fn connect_silent_endpoint_handshake_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept the connection and hold it silent — no HTTP 101 ever comes,
        // mirroring the deterministic wedge from the issue report.
        let holder = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let request = format!("ws://{}", addr)
            .into_client_request()
            .expect("valid ws url");
        let started = std::time::Instant::now();
        let err = match CdpClient::connect_request(
            request,
            WebSocketConfig::default(),
            None,
            Duration::from_millis(200),
        )
        .await
        {
            Ok(_) => panic!("silent endpoint must fail with a handshake timeout"),
            Err(err) => err,
        };

        assert!(
            err.contains("handshake response"),
            "unexpected error: {}",
            err
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "handshake error must be bounded, took {:?}",
            started.elapsed()
        );

        holder.abort();
    }

    #[test]
    fn normalizes_only_root_websocket_queries() {
        assert_eq!(
            normalize_websocket_root_path("wss://browser.example?token=a%2Fb"),
            "wss://browser.example/?token=a%2Fb"
        );
        assert_eq!(
            normalize_websocket_root_path("ws://[::1]:9222?token=test"),
            "ws://[::1]:9222/?token=test"
        );
        assert_eq!(
            normalize_websocket_root_path("wss://user:pass@browser.example?token=test"),
            "wss://user:pass@browser.example/?token=test"
        );
        assert_eq!(
            normalize_websocket_root_path("wss://browser.example?"),
            "wss://browser.example/?"
        );
        assert_eq!(
            normalize_websocket_root_path("wss://browser.example/?token=a%2Fb"),
            "wss://browser.example/?token=a%2Fb"
        );
        assert_eq!(
            normalize_websocket_root_path("wss://browser.example/cdp?token=a%2Fb"),
            "wss://browser.example/cdp?token=a%2Fb"
        );
    }

    #[tokio::test]
    async fn root_websocket_url_with_query_sends_slash_request_target() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (path_tx, path_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut path_tx = Some(path_tx);
            let _ = tokio_tungstenite::accept_hdr_async(
                stream,
                move |
                    request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                    response: tokio_tungstenite::tungstenite::handshake::server::Response,
                | {
                    if let Some(tx) = path_tx.take() {
                        let path = request
                            .uri()
                            .path_and_query()
                            .map(|value| value.as_str().to_string())
                            .unwrap_or_default();
                        let _ = tx.send(path);
                    }
                    Ok(response)
                },
            )
            .await
            .unwrap();
        });

        let url = format!("ws://127.0.0.1:{}?token=a%2Fb&scope=browser%20test", port);
        let _client = CdpClient::connect(&url).await.unwrap();

        assert_eq!(path_rx.await.unwrap(), "/?token=a%2Fb&scope=browser%20test");
        server.await.unwrap();
    }
    /// Events on a privately subscribed session reach only that receiver;
    /// everything else still reaches broadcast subscribers.
    #[tokio::test]
    async fn private_session_events_bypass_the_broadcast() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ready_rx.await.unwrap();
            for (method, session) in [
                ("Page.screencastFrame", Some("S-REC")),
                ("Page.screencastFrame", Some("S-PAGE")),
                ("Target.detachedFromTarget", None),
            ] {
                let mut msg = serde_json::json!({ "method": method, "params": {} });
                if let Some(sid) = session {
                    msg["sessionId"] = serde_json::json!(sid);
                }
                ws.send(Message::Text(msg.to_string())).await.unwrap();
            }
            // Keep the connection open until the client is done reading.
            let _ = ws.next().await;
        });

        let client = CdpClient::connect(&format!("ws://127.0.0.1:{}", port))
            .await
            .unwrap();
        let mut broadcast_rx = client.subscribe();
        let mut private_rx = client.subscribe_session("S-REC");
        ready_tx.send(()).unwrap();

        let wait = std::time::Duration::from_secs(2);
        let first = tokio::time::timeout(wait, broadcast_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.session_id.as_deref(), Some("S-PAGE"));
        let second = tokio::time::timeout(wait, broadcast_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.method, "Target.detachedFromTarget");

        let private = tokio::time::timeout(wait, private_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(private.session_id.as_deref(), Some("S-REC"));
        assert!(
            private_rx.try_recv().is_err(),
            "only S-REC events are routed privately"
        );

        client.unsubscribe_session("S-REC");
        drop(client);
        server.abort();
    }

    #[tokio::test]
    async fn method_tap_keeps_every_event_when_the_broadcast_lags() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ready_rx.await.unwrap();
            for i in 0..7000 {
                let method = if i % 7 == 0 {
                    "Fetch.requestPaused"
                } else {
                    "Network.dataReceived"
                };
                let msg = serde_json::json!({ "method": method, "params": { "n": i } });
                ws.send(Message::Text(msg.to_string())).await.unwrap();
            }
            let _ = ws.next().await;
        });

        let client = CdpClient::connect(&format!("ws://127.0.0.1:{}", port))
            .await
            .unwrap();
        let mut broadcast_rx = client.subscribe();
        let mut tap = client.subscribe_methods(&["Fetch.requestPaused"]);
        ready_tx.send(()).unwrap();

        let wait = std::time::Duration::from_secs(5);
        for expected in (0..7000).step_by(7) {
            let event = tokio::time::timeout(wait, tap.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.method, "Fetch.requestPaused");
            assert_eq!(event.params["n"], expected);
        }
        assert!(matches!(
            broadcast_rx.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));

        drop(client);
        server.abort();
    }
}
