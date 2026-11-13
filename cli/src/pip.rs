//! `agent-browser pip`: a small always-on-top window that shows the active
//! session live, using the dashboard's `/pip` view inside a native macOS panel.

use std::fs;
use std::path::{Path, PathBuf};

use crate::connection::{get_socket_dir, is_pid_alive};

/// Helper binary name inside the npm package `bin/` directory.
pub const HELPER_PACKAGED_NAME: &str = "agent-browser-pip-darwin";
/// Helper binary name produced by `scripts/build-pip.sh` next to a cargo build.
pub const HELPER_DEV_NAME: &str = "agent-browser-pip";
pub const DEFAULT_DASHBOARD_PORT: u16 = 4848;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipCommand {
    Open { port: Option<u16> },
    Stop,
}

/// Parse `pip` arguments: `pip [--port <n>]` or `pip stop`.
pub fn parse_pip_command(args: &[String]) -> Result<PipCommand, String> {
    match args.first().map(String::as_str) {
        Some("stop") => {
            if let Some(argument) = args.get(1) {
                return Err(format!("PiP stop does not accept argument '{argument}'."));
            }
            Ok(PipCommand::Stop)
        }
        Some("--port") => {
            let value = args
                .get(1)
                .filter(|value| !value.starts_with('-'))
                .ok_or_else(|| "PiP option '--port' requires a value.".to_string())?;
            let port = value
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or_else(|| {
                    format!(
                        "Invalid dashboard port '{value}'. Expected an integer from 1 to 65535."
                    )
                })?;
            if let Some(argument) = args.get(2) {
                return Err(format!("Unexpected PiP argument: {argument}"));
            }
            Ok(PipCommand::Open { port: Some(port) })
        }
        Some(value) if value.starts_with('-') => Err(format!("Unknown PiP option: {value}")),
        Some(value) => Err(format!("Unknown PiP subcommand: {value}")),
        None => Ok(PipCommand::Open { port: None }),
    }
}

pub fn pid_path() -> PathBuf {
    get_socket_dir().join("pip.pid")
}

/// URL of the dashboard the CLI verified. The server binds 127.0.0.1, so the
/// window loads that address rather than a name that can resolve elsewhere.
pub fn pip_url(dashboard_port: u16) -> String {
    format!("http://127.0.0.1:{dashboard_port}/pip")
}

/// Where the helper may live relative to the running CLI binary.
pub fn helper_candidates(exe: &Path) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    vec![dir.join(HELPER_PACKAGED_NAME), dir.join(HELPER_DEV_NAME)]
}

pub fn find_helper(exe: &Path) -> Option<PathBuf> {
    helper_candidates(exe)
        .into_iter()
        .find(|path| path.is_file())
}

/// PID of a PiP window that is still open, if any.
pub fn running_pid() -> Option<u32> {
    let pid = fs::read_to_string(pid_path())
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    if is_pid_alive(pid) {
        Some(pid)
    } else {
        let _ = fs::remove_file(pid_path());
        None
    }
}

/// Start the helper detached from this process and record its PID.
pub fn spawn_helper(helper: &Path, url: &str) -> Result<u32, String> {
    // Package managers do not always keep the executable bit on extra binaries.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(helper) {
            if metadata.permissions().mode() & 0o111 == 0 {
                let _ = fs::set_permissions(helper, fs::Permissions::from_mode(0o755));
            }
        }
    }

    let mut cmd = std::process::Command::new(helper);
    cmd.arg(url);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("Failed to open PiP window: {error}"))?;
    let pid = child.id();
    let socket_dir = get_socket_dir();
    let _ = fs::create_dir_all(&socket_dir);
    fs::write(pid_path(), pid.to_string())
        .map_err(|error| format!("Failed to save PiP state: {error}"))?;
    Ok(pid)
}

/// Close the PiP window. Returns whether a window was open.
pub fn stop() -> bool {
    let Some(pid) = running_pid() else {
        return false;
    };
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = fs::remove_file(pid_path());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_open_and_stop() {
        assert_eq!(parse_pip_command(&[]), Ok(PipCommand::Open { port: None }));
        assert_eq!(
            parse_pip_command(&args(&["--port", "4900"])),
            Ok(PipCommand::Open { port: Some(4900) })
        );
        assert_eq!(parse_pip_command(&args(&["stop"])), Ok(PipCommand::Stop));
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse_pip_command(&args(&["--port"])).is_err());
        assert!(parse_pip_command(&args(&["--port", "0"])).is_err());
        assert!(parse_pip_command(&args(&["--port", "70000"])).is_err());
        assert!(parse_pip_command(&args(&["--port", "4900", "extra"])).is_err());
        assert!(parse_pip_command(&args(&["stop", "now"])).is_err());
        assert!(parse_pip_command(&args(&["--size", "large"])).is_err());
        assert!(parse_pip_command(&args(&["open"])).is_err());
    }

    #[test]
    fn looks_for_the_helper_next_to_the_cli() {
        let exe = Path::new("/opt/agent-browser/bin/agent-browser-darwin-arm64");
        assert_eq!(
            helper_candidates(exe),
            vec![
                PathBuf::from("/opt/agent-browser/bin/agent-browser-pip-darwin"),
                PathBuf::from("/opt/agent-browser/bin/agent-browser-pip"),
            ]
        );
    }

    #[test]
    fn builds_the_dashboard_pip_url() {
        assert_eq!(pip_url(4848), "http://127.0.0.1:4848/pip");
    }
}
