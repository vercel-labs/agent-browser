//! Shared machinery for spawn-and-poll engine launchers (Lightpanda,
//! AginxBrowser): a bounded stdout/stderr capture buffer drained by background
//! threads, and a readiness loop that polls the engine's CDP `/json/version`
//! endpoint until it answers, fails fast if the child exits, and reports rich
//! diagnostics (last probe error + captured logs) on failure.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::discovery::discover_cdp_url_with_timeout;

const MAX_LOG_LINES: usize = 40;

pub(crate) const CDP_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const CDP_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(500);

/// Bounded, thread-safe snapshot of the last `MAX_LOG_LINES` lines the
/// launched engine wrote to stdout and stderr, for launch-error reporting.
#[derive(Clone, Default)]
pub(crate) struct LaunchLogBuffer {
    stdout: Arc<Mutex<VecDeque<String>>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl LaunchLogBuffer {
    fn push_stdout(&self, line: String) {
        push_bounded(&self.stdout, line);
    }

    fn push_stderr(&self, line: String) {
        push_bounded(&self.stderr, line);
    }

    fn snapshot_stdout(&self) -> Vec<String> {
        self.stdout
            .lock()
            .expect("stdout log buffer poisoned")
            .iter()
            .cloned()
            .collect()
    }

    fn snapshot_stderr(&self) -> Vec<String> {
        self.stderr
            .lock()
            .expect("stderr log buffer poisoned")
            .iter()
            .cloned()
            .collect()
    }
}

fn push_bounded(buffer: &Mutex<VecDeque<String>>, line: String) {
    let mut guard = buffer.lock().expect("log buffer poisoned");
    if guard.len() >= MAX_LOG_LINES {
        guard.pop_front();
    }
    guard.push_back(line);
}

/// Take ownership of the child's piped stdout/stderr and drain them into
/// `LaunchLogBuffer` on background threads, so a chatty engine can never fill
/// its pipe buffer and block. The returned join handles intentionally outlive
/// the launch call and are stored on the process struct; they end when the
/// pipes close (child exit).
pub(crate) fn start_log_drainers(
    child: &mut Child,
    engine: &str,
) -> Result<(LaunchLogBuffer, Vec<std::thread::JoinHandle<()>>), String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("Failed to capture {} stdout", engine))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("Failed to capture {} stderr", engine))?;

    let logs = LaunchLogBuffer::default();
    let stdout_logs = logs.clone();
    let stderr_logs = logs.clone();

    let stdout_handle =
        std::thread::spawn(move || drain_reader(stdout, move |line| stdout_logs.push_stdout(line)));
    let stderr_handle =
        std::thread::spawn(move || drain_reader(stderr, move |line| stderr_logs.push_stderr(line)));

    Ok((logs, vec![stdout_handle, stderr_handle]))
}

fn drain_reader<R, F>(reader: R, mut push: F)
where
    R: std::io::Read,
    F: FnMut(String),
{
    for line in BufReader::new(reader).lines() {
        match line {
            Ok(line) => push(line),
            Err(_) => break,
        }
    }
}

/// Poll the engine's CDP discovery endpoint on `port` until it answers,
/// the child exits, or `startup_timeout` elapses. On failure the error
/// includes the last probe error and the captured log tail.
pub(crate) async fn wait_for_cdp_ready(
    child: &mut Child,
    port: u16,
    logs: &LaunchLogBuffer,
    startup_timeout: Duration,
    engine: &str,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + startup_timeout;
    let mut last_probe_error = None;

    loop {
        if let Ok(Some(status)) = child.try_wait() {
            // Give the drainer threads a brief window to flush the last log lines
            // before we snapshot them.  This is best-effort: lines written just
            // before exit may still be missing, but the most useful output (early
            // startup errors) will already be in the buffer.
            tokio::time::sleep(Duration::from_millis(25)).await;
            return Err(launch_error(
                &format!(
                    "{} exited before CDP became ready (status: {})",
                    engine, status
                ),
                logs,
                last_probe_error.as_deref(),
                engine,
            ));
        }

        match discover_cdp_url_with_timeout("127.0.0.1", port, None, CDP_DISCOVERY_TIMEOUT).await {
            Ok(ws_url) => return Ok(ws_url),
            Err(err) => last_probe_error = Some(err),
        }

        if std::time::Instant::now() >= deadline {
            return Err(launch_error(
                &format!(
                    "Timed out after {}ms waiting for {} CDP endpoint on port {}",
                    startup_timeout.as_millis(),
                    engine,
                    port
                ),
                logs,
                last_probe_error.as_deref(),
                engine,
            ));
        }

        tokio::time::sleep(CDP_POLL_INTERVAL).await;
    }
}

/// Format a launch failure with whatever diagnostics were collected: the last
/// CDP probe error, then the stderr and stdout tails.
pub(crate) fn launch_error(
    message: &str,
    logs: &LaunchLogBuffer,
    last_probe_error: Option<&str>,
    engine: &str,
) -> String {
    let stdout_lines = logs.snapshot_stdout();
    let stderr_lines = logs.snapshot_stderr();
    let mut details = Vec::new();

    if let Some(err) = last_probe_error {
        details.push(format!("Last probe error: {}", err));
    }

    if !stderr_lines.is_empty() {
        details.push(format!(
            "{} stderr (last {} lines):\n  {}",
            engine,
            stderr_lines.len(),
            stderr_lines.join("\n  ")
        ));
    }

    if !stdout_lines.is_empty() {
        details.push(format!(
            "{} stdout (last {} lines):\n  {}",
            engine,
            stdout_lines.len(),
            stdout_lines.join("\n  ")
        ));
    }

    if details.is_empty() {
        format!("{} (no stdout/stderr output from {})", message, engine)
    } else {
        format!("{}\n{}", message, details.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launch_error_no_logs() {
        let logs = LaunchLogBuffer::default();
        let msg = launch_error("Lightpanda exited", &logs, None, "Lightpanda");
        assert!(msg.contains("no stdout/stderr output"));
    }

    #[test]
    fn test_launch_error_with_lines() {
        let logs = LaunchLogBuffer::default();
        logs.push_stdout("stdout line".to_string());
        logs.push_stderr("stderr line".to_string());
        let msg = launch_error(
            "AginxBrowser exited",
            &logs,
            Some("connect failed"),
            "AginxBrowser",
        );
        assert!(msg.contains("stdout line"));
        assert!(msg.contains("stderr line"));
        assert!(msg.contains("Last probe error: connect failed"));
    }

    #[test]
    fn test_log_buffer_is_bounded() {
        let logs = LaunchLogBuffer::default();
        for i in 0..(MAX_LOG_LINES + 10) {
            logs.push_stdout(format!("line {}", i));
        }
        assert_eq!(logs.snapshot_stdout().len(), MAX_LOG_LINES);
        assert_eq!(logs.snapshot_stdout()[0], format!("line {}", 10));
    }
}
