//! Client for the Camoufox Python sidecar.
//!
//! Speaks the JSON-line protocol documented in
//! `packages/camoufox-sidecar/camoufox_sidecar/protocol.py`:
//!
//! ```text
//! request:   {"id": N, "cmd": "<name>", "args": {...}}
//! response:  {"id": N, "ok": true,  "result": {...}}
//!            {"id": N, "ok": false, "error": {"code": "...", "message": "..."}}
//! event:     {"event": "<name>", "data": {...}}
//! ```
//!
//! The client owns the subprocess's stdin (writer) and stdout (reader task).
//! A monotonic request id plus a pending `HashMap<u64, oneshot::Sender>`
//! demultiplexes responses back to the matching `call`. Asynchronous frames
//! (the `ready` event carries the sidecar pid; other events like
//! `page.console` are forwarded in later units) fan out on a broadcast
//! channel that callers can `subscribe` to.
//!
//! Errors from the sidecar arrive as `{code, message}` objects; we surface
//! them as `"<code>: <message>"` strings to match the rest of the Rust
//! daemon's `Result<_, String>` convention. The error-code catalog the
//! sidecar may emit today is:
//!
//! - `invalid-frame`            — malformed JSON on the wire
//! - `not-yet-supported`        — unknown command (post-Unit 3 this is the
//!                                dominant failure while more commands are
//!                                ported in Units 4–5)
//! - `invalid-args`             — well-formed frame with a bad args shape
//! - `unknown-launch-option`    — launch kwarg not on the sidecar's allowlist
//! - `unsupported-launch-option` — explicitly rejected launch kwarg
//!   (`persistent_context`, `user_data_dir`)
//! - `already-launched`         — second `launch` without a `close`
//! - `camoufox-not-installed`   — `import camoufox` failed or binary missing
//! - `launch-failed`            — any other launch-time failure
//! - `internal-error`           — uncaught exception inside a handler

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{broadcast, oneshot};

const EVENT_CHANNEL_CAPACITY: usize = 64;
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// A named event forwarded from the sidecar. Callers that don't care about
/// events can simply never subscribe; the broadcast channel is bounded, so a
/// slow consumer that lags drops old events rather than backpressuring the
/// reader.
#[derive(Debug, Clone)]
pub struct CamoufoxEvent {
    pub name: String,
    pub data: Value,
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// Sidecar client. Cheap to clone via `Arc`; construct once per session.
pub struct CamoufoxClient {
    writer: tokio::sync::Mutex<ChildStdin>,
    pending: PendingMap,
    next_id: AtomicU64,
    events: broadcast::Sender<CamoufoxEvent>,
    /// Signals the reader loop to shut down on Drop.
    shutdown: Arc<tokio::sync::Notify>,
    _reader: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl CamoufoxClient {
    /// Consume the sidecar's stdin/stdout handles, wait for the `ready`
    /// event (with the reported pid), and spawn the background reader task.
    /// On success returns `(Arc<Self>, Option<sidecar_pid>)`. The pid is
    /// `None` only if the sidecar omits it — older sidecars may, but current
    /// ones always attach it.
    pub async fn start(
        stdin: ChildStdin,
        stdout: ChildStdout,
        ready_timeout: Duration,
    ) -> Result<(Arc<Self>, Option<u32>), String> {
        let mut reader = BufReader::new(stdout);

        // Read the first frame: must be the `ready` event. Anything else is
        // either a protocol bug on the sidecar side or a premature exit,
        // both of which we treat as a readiness failure so the error is
        // actionable.
        let ready_line =
            tokio::time::timeout(ready_timeout, read_one_nonblank_line(&mut reader))
                .await
                .map_err(|_| {
                    format!(
                        "timed out after {}ms waiting for camoufox-sidecar `ready` event",
                        ready_timeout.as_millis()
                    )
                })?
                .map_err(|e| format!("reading first sidecar frame: {}", e))?;

        let ready_frame: Value = serde_json::from_str(&ready_line).map_err(|e| {
            format!(
                "first sidecar frame was not valid JSON: {} (frame: {:?})",
                e, ready_line
            )
        })?;
        let pid = parse_ready_frame(&ready_frame)?;

        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let shutdown = Arc::new(tokio::sync::Notify::new());

        let reader_task = spawn_reader(reader, pending.clone(), events_tx.clone(), shutdown.clone());

        let client = Arc::new(Self {
            writer: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            events: events_tx,
            shutdown,
            _reader: std::sync::Mutex::new(Some(reader_task)),
        });
        Ok((client, pid))
    }

    /// Send a request and await the response. Times out after
    /// `DEFAULT_CALL_TIMEOUT` so a misbehaving sidecar cannot wedge a caller
    /// indefinitely.
    pub async fn call(&self, cmd: &str, args: Value) -> Result<Value, String> {
        self.call_with_timeout(cmd, args, DEFAULT_CALL_TIMEOUT).await
    }

    pub async fn call_with_timeout(
        &self,
        cmd: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("camoufox pending map poisoned");
            pending.insert(id, tx);
        }

        let frame = json!({ "id": id, "cmd": cmd, "args": args });
        let serialized = serde_json::to_string(&frame).expect("frame serializes");
        {
            let mut writer = self.writer.lock().await;
            writer
                .write_all(serialized.as_bytes())
                .await
                .map_err(|e| format!("sending `{}` to camoufox-sidecar: {}", cmd, e))?;
            writer
                .write_all(b"\n")
                .await
                .map_err(|e| format!("sending newline to camoufox-sidecar: {}", e))?;
            writer
                .flush()
                .await
                .map_err(|e| format!("flushing camoufox-sidecar stdin: {}", e))?;
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_canceled)) => {
                self.drop_pending(id);
                Err(format!(
                    "camoufox-sidecar dropped response for `{}` (reader task exited)",
                    cmd
                ))
            }
            Err(_) => {
                self.drop_pending(id);
                Err(format!(
                    "camoufox-sidecar `{}` timed out after {}ms",
                    cmd,
                    timeout.as_millis()
                ))
            }
        }
    }

    fn drop_pending(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }

    /// Subscribe to events emitted by the sidecar. New subscribers only see
    /// events sent after they subscribe (consistent with `tokio::broadcast`
    /// semantics); the `ready` event is consumed during `start()` and does
    /// not reach subscribers.
    pub fn subscribe(&self) -> broadcast::Receiver<CamoufoxEvent> {
        self.events.subscribe()
    }

    /// Send the `close` command and wait for its response. Note that
    /// receiving the response does NOT imply the sidecar process has
    /// exited — the sidecar sets its shutdown event right after responding,
    /// and the OS-level reap happens shortly after. Callers must wait on the
    /// process handle separately if they need that guarantee (see
    /// `CamoufoxProcess::wait_or_kill`).
    pub async fn close(&self) -> Result<Value, String> {
        self.call_with_timeout("close", json!({}), CLOSE_TIMEOUT)
            .await
    }
}

impl std::fmt::Debug for CamoufoxClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CamoufoxClient").finish_non_exhaustive()
    }
}

impl Drop for CamoufoxClient {
    fn drop(&mut self) {
        // Signal the reader task to exit and fail any in-flight calls so
        // awaiters get a clean error instead of hanging forever.
        self.shutdown.notify_waiters();
        if let Ok(mut pending) = self.pending.lock() {
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err(
                    "camoufox-sidecar client dropped while request was in flight".to_string(),
                ));
            }
        }
        if let Ok(mut slot) = self._reader.lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
    }
}

async fn read_one_nonblank_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<String> {
    loop {
        let mut buf = String::new();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "camoufox-sidecar stdout closed before first frame",
            ));
        }
        let trimmed = buf.trim_end_matches(['\r', '\n']).to_string();
        if trimmed.trim().is_empty() {
            continue;
        }
        return Ok(trimmed);
    }
}

fn parse_ready_frame(frame: &Value) -> Result<Option<u32>, String> {
    let event = frame
        .get("event")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "expected first frame to be a ready event, got: {}",
                frame
            )
        })?;
    if event != "ready" {
        return Err(format!(
            "expected first frame to be `ready`, got event `{}`",
            event
        ));
    }
    let pid = frame
        .get("data")
        .and_then(|d| d.get("pid"))
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok());
    Ok(pid)
}

fn spawn_reader(
    mut reader: BufReader<ChildStdout>,
    pending: PendingMap,
    events: broadcast::Sender<CamoufoxEvent>,
    shutdown: Arc<tokio::sync::Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            let read = tokio::select! {
                biased;
                _ = shutdown.notified() => { return; }
                r = reader.read_line(&mut line) => r,
            };
            match read {
                Ok(0) => break, // stdout closed → sidecar exited
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\r', '\n']).trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    dispatch_frame(trimmed, &pending, &events);
                }
                Err(_) => break,
            }
        }
        // Sidecar exited: fail every pending call so callers don't hang.
        if let Ok(mut p) = pending.lock() {
            for (_, tx) in p.drain() {
                let _ = tx.send(Err(
                    "camoufox-sidecar closed stdout before responding".to_string()
                ));
            }
        }
    })
}

fn dispatch_frame(line: &str, pending: &PendingMap, events: &broadcast::Sender<CamoufoxEvent>) {
    let frame: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "[agent-browser] camoufox-sidecar sent malformed JSON on stdout: {} ({:?})",
                e, line
            );
            return;
        }
    };

    if let Some(event) = frame.get("event").and_then(|v| v.as_str()) {
        let data = frame.get("data").cloned().unwrap_or(Value::Null);
        // Ignore send errors: if no subscribers are attached we just drop the
        // event, which is the intended behavior.
        let _ = events.send(CamoufoxEvent {
            name: event.to_string(),
            data,
        });
        return;
    }

    let Some(id) = frame.get("id").and_then(|v| v.as_u64()) else {
        // Responses must carry an id; if not, log and drop.
        eprintln!(
            "[agent-browser] camoufox-sidecar response had no id: {:?}",
            line
        );
        return;
    };

    let tx = {
        let mut p = match pending.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        p.remove(&id)
    };

    let Some(tx) = tx else {
        // Late response for a request we already timed out on.
        return;
    };

    let ok = frame.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let result = if ok {
        let value = frame.get("result").cloned().unwrap_or(Value::Null);
        Ok(value)
    } else {
        let err = frame.get("error");
        let code = err
            .and_then(|e| e.get("code"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let message = err
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("no message provided");
        Err(format!("{}: {}", code, message))
    };
    let _ = tx.send(result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ready_frame_extracts_pid() {
        let frame = json!({"event": "ready", "data": {"pid": 12345}});
        assert_eq!(parse_ready_frame(&frame).unwrap(), Some(12345));
    }

    #[test]
    fn parse_ready_frame_rejects_wrong_event() {
        let frame = json!({"event": "closed", "data": {}});
        let err = parse_ready_frame(&frame).unwrap_err();
        assert!(err.contains("expected first frame to be `ready`"));
    }

    #[test]
    fn parse_ready_frame_rejects_response_frame() {
        let frame = json!({"id": 1, "ok": true, "result": {}});
        let err = parse_ready_frame(&frame).unwrap_err();
        assert!(err.contains("ready event"));
    }

    #[test]
    fn parse_ready_frame_tolerates_missing_pid() {
        let frame = json!({"event": "ready", "data": {}});
        assert_eq!(parse_ready_frame(&frame).unwrap(), None);
    }

    #[test]
    fn dispatch_frame_routes_response_to_pending() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _) = broadcast::channel::<CamoufoxEvent>(8);

        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().insert(7, tx);

        dispatch_frame(
            r#"{"id":7,"ok":true,"result":{"hello":"world"}}"#,
            &pending,
            &events_tx,
        );
        let got = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(got["hello"], json!("world"));
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn dispatch_frame_surfaces_error_code() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _) = broadcast::channel::<CamoufoxEvent>(8);

        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().insert(9, tx);

        dispatch_frame(
            r#"{"id":9,"ok":false,"error":{"code":"launch-failed","message":"boom"}}"#,
            &pending,
            &events_tx,
        );
        let err = rx.blocking_recv().unwrap().unwrap_err();
        assert_eq!(err, "launch-failed: boom");
    }

    #[test]
    fn dispatch_frame_fans_events() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, mut events_rx) = broadcast::channel::<CamoufoxEvent>(8);

        dispatch_frame(
            r#"{"event":"page.console","data":{"level":"warn","text":"hi"}}"#,
            &pending,
            &events_tx,
        );

        let evt = events_rx.try_recv().unwrap();
        assert_eq!(evt.name, "page.console");
        assert_eq!(evt.data["text"], json!("hi"));
    }

    #[test]
    fn dispatch_frame_ignores_unknown_id() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _) = broadcast::channel::<CamoufoxEvent>(8);

        // Should not panic or crash.
        dispatch_frame(
            r#"{"id":999,"ok":true,"result":{}}"#,
            &pending,
            &events_tx,
        );
        assert!(pending.lock().unwrap().is_empty());
    }
}
