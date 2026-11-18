//! Daemon side of `poppy join` (experimental).
//!
//! One action covers the whole join, so the one-time code is read from the
//! page, handed to the authorizer, and submitted without leaving the daemon.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use url::Url;

use super::actions::DaemonState;

/// Marks an error string as `poppy:<code>: <message>`.
pub const ERROR_PREFIX: &str = "poppy:";

const READ_CODE: &str = "document.querySelector('form input[name=code]')?.value ?? null";
const SUBMIT: &str = "(() => { const i = document.querySelector('form input[name=code]'); \
                      if (!i || !i.form) return false; i.form.submit(); return true; })()";

fn fail(code: &str, message: impl std::fmt::Display) -> String {
    format!("{}{}: {}", ERROR_PREFIX, code, message)
}

fn same_resource(a: &str, b: &str) -> bool {
    match (Url::parse(a), Url::parse(b)) {
        (Ok(mut a), Ok(mut b)) => {
            a.set_fragment(None);
            b.set_fragment(None);
            a == b
        }
        _ => false,
    }
}

/// Steps 1 to 3 of the browser join, with the authorizer between them.
pub(super) async fn handle_join(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let field = |name: &str| {
        cmd.get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("Missing '{}' parameter", name))
    };
    let (endpoint, authorizer, origin) =
        (field("endpoint")?, field("authorizer")?, field("origin")?);
    let cwd = cmd.get("cwd").and_then(Value::as_str);
    let timeout_ms = state.timeout_ms(cmd);

    state
        .refuse_auto_connect()
        .map_err(|e| fail("auto_connect_refused", e))?;
    let session_id = state
        .browser
        .as_ref()
        .ok_or("Browser not launched")?
        .active_session_id()?
        .to_string();

    let result = run_join(state, &endpoint, &authorizer, &origin, cwd, timeout_ms).await;
    if result.is_err() {
        leave_join_page(state, &session_id).await;
    }
    result
}

async fn run_join(
    state: &mut DaemonState,
    endpoint: &str,
    authorizer: &str,
    origin: &str,
    cwd: Option<&str>,
    timeout_ms: u64,
) -> Result<Value, String> {
    super::actions::handle_navigate(&json!({ "url": endpoint }), state)
        .await
        .map_err(|e| fail("join_failed", e))?;
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let code = mgr
        .evaluate(READ_CODE, None)
        .await
        .map_err(|e| fail("join_failed", e))?
        .as_str()
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .ok_or_else(|| fail("join_failed", "The browser session page has no code form"))?;

    let input = json!({
        "code": code,
        "authorize_url": format!("{}/authorize", endpoint.trim_end_matches('/')),
        "origin": origin,
        "poppy_version": "1.0",
    });
    let signed_in = run_authorizer(authorizer, cwd, &input, &code, timeout_ms).await?;

    // Subscribe before submitting so the navigation cannot be missed.
    let session_id = mgr.active_session_id()?.to_string();
    let mut events = mgr.client.subscribe();
    let submitted = mgr
        .evaluate(SUBMIT, None)
        .await
        .map_err(|e| fail("join_failed", e))?;
    if submitted != json!(true) {
        return Err(fail(
            "join_failed",
            "The page no longer shows the join form",
        ));
    }
    let wait = async {
        loop {
            match events.recv().await {
                Ok(event)
                    if event.session_id.as_deref() == Some(session_id.as_str())
                        && event.method == "Page.frameNavigated"
                        && event.params.pointer("/frame/parentId").is_none() =>
                {
                    return Ok(event
                        .params
                        .pointer("/frame/url")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string());
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(fail("join_failed", "CDP event stream closed"))
                }
            }
        }
    };
    let url = tokio::time::timeout(Duration::from_millis(timeout_ms), wait)
        .await
        .map_err(|_| {
            fail(
                "join_failed",
                format!(
                    "Timed out after {}ms waiting for the join to finish",
                    timeout_ms
                ),
            )
        })??;
    // A Company's error page stays on the endpoint; success leaves it.
    if same_resource(&url, endpoint) {
        return Err(fail(
            "join_failed",
            "The Company did not accept the browser session form",
        ));
    }
    state.ref_map.invalidate_page(&session_id);
    Ok(json!({ "experimental": true, "joined": true, "signed_in": signed_in, "url": url }))
}

/// Returns the tab to `about:blank` after a failed join.
async fn leave_join_page(state: &mut DaemonState, session_id: &str) {
    let Some(mgr) = state
        .browser
        .as_ref()
        .filter(|m| m.has_page_session(session_id))
    else {
        return;
    };
    let _ = mgr
        .client
        .send_command(
            "Page.navigate",
            Some(json!({ "url": "about:blank" })),
            Some(session_id),
        )
        .await;
    state.ref_map.invalidate_page(session_id);
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    }
}

/// Runs the authorizer with `input` as one JSON line on stdin and returns
/// whether it signed the browser in.
async fn run_authorizer(
    command: &str,
    cwd: Option<&str>,
    input: &Value,
    code: &str,
    timeout_ms: u64,
) -> Result<bool, String> {
    let failed = |message: String| fail("authorizer_failed", message);
    let mut cmd = shell_command(command);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| failed(format!("Could not start authorizer: {}", e)))?;
    let line = format!("{}\n", input);
    let mut stdin = child.stdin.take();
    let run = async move {
        if let Some(pipe) = stdin.as_mut() {
            // A broken pipe means the authorizer exited unread; its status explains why.
            let _ = pipe.write_all(line.as_bytes()).await;
        }
        drop(stdin);
        child.wait_with_output().await
    };
    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), run)
        .await
        .map_err(|_| failed(format!("Authorizer did not finish within {}ms", timeout_ms)))?
        .map_err(|e| failed(format!("Could not wait for authorizer: {}", e)))?;
    interpret_output(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        code,
    )
}

/// `{"signed_in": bool}` with exit 0, or `{"error": "<code>"}` with a nonzero exit.
fn interpret_output(success: bool, stdout: &str, code: &str) -> Result<bool, String> {
    let parsed: Option<Value> = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| serde_json::from_str(line.trim()).ok());
    if !success {
        let error = parsed
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty() && !e.contains(code));
        return Err(match error {
            Some(error) => fail(error, "The authorizer reported an error"),
            None => fail(
                "authorizer_failed",
                "The authorizer failed without an error code",
            ),
        });
    }
    parsed
        .as_ref()
        .and_then(|v| v.get("signed_in"))
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            fail(
                "authorizer_failed",
                "The authorizer exited 0 without a JSON line containing signed_in",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorizer_output_is_interpreted() {
        assert_eq!(
            interpret_output(true, "{\"signed_in\":true}\n", "bc_1"),
            Ok(true)
        );
        assert_eq!(
            interpret_output(true, "noise\n{\"signed_in\":false}", "bc_1"),
            Ok(false)
        );
        assert_eq!(
            interpret_output(false, "{\"error\":\"insufficient_scope\"}", "bc_1").unwrap_err(),
            "poppy:insufficient_scope: The authorizer reported an error"
        );
    }

    #[test]
    fn malformed_authorizer_output_is_authorizer_failed() {
        for (success, stdout) in [
            (true, ""),
            (true, "{\"signed_in\":\"yes\"}"),
            (false, ""),
            (false, "not json"),
            (false, "{\"error\":\"\"}"),
            (false, "{\"error\":\"leak_bc_1\"}"),
        ] {
            let err = interpret_output(success, stdout, "bc_1").unwrap_err();
            assert!(
                err.starts_with("poppy:authorizer_failed: "),
                "{stdout}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn the_authorizer_gets_one_json_line_in_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker"), "").unwrap();
        let script = "read line; case \"$line\" in *bc_1*) [ -f marker ] && \
                      echo '{\"signed_in\":true}' || echo '{\"signed_in\":false}';; esac";
        let cwd = dir.path().to_str().unwrap();
        let input = json!({ "code": "bc_1" });
        assert_eq!(
            run_authorizer(script, Some(cwd), &input, "bc_1", 5_000).await,
            Ok(true)
        );
    }

    #[tokio::test]
    async fn a_slow_authorizer_is_stopped_at_the_timeout() {
        let err = run_authorizer("sleep 5", None, &json!({}), "bc_1", 100)
            .await
            .unwrap_err();
        assert!(err.starts_with("poppy:authorizer_failed: "), "{err}");
    }

    #[test]
    fn same_resource_ignores_fragments() {
        assert!(same_resource("http://h/a#x", "http://h/a"));
        assert!(!same_resource("http://h/", "http://h/a"));
    }
}
