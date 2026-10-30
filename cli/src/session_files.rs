//! Per-session files in the socket directory.
//!
//! Every file a session keeps next to its socket is declared here once,
//! together with whether it only describes a running daemon. Cleanup derives
//! its list from that, so a new file is removed by every cleanup path as soon
//! as it is declared.

use std::fs;
use std::path::{Path, PathBuf};

use crate::connection::get_socket_dir;

/// A file stored in the socket directory as `<session>.<extension>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionFile {
    /// Unix domain socket the daemon listens on (Unix only).
    Socket,
    /// TCP port the daemon listens on (Windows only).
    Port,
    /// Daemon pid. Also tells which daemon owns the other files.
    Pid,
    /// Version of the running daemon.
    Version,
    /// Fingerprint of the options the daemon was started with.
    Config,
    /// Port of the stream server.
    Stream,
    /// Browser engine, read by dashboard discovery.
    Engine,
    /// Browser provider, read by dashboard discovery.
    Provider,
    /// Loaded extensions, read by dashboard discovery.
    Extensions,
    /// Tab binding. Survives daemon restarts on purpose (see `tab_binding`).
    Target,
    /// Debug log written when `AGENT_BROWSER_DEBUG` is set.
    Log,
    /// Startup lock. Keep the inode across daemon restarts so all callers lock the same file.
    StartupLock,
}

impl SessionFile {
    /// Every session file. Keep in sync when adding a variant.
    pub const ALL: &'static [SessionFile] = &[
        SessionFile::Socket,
        SessionFile::Port,
        SessionFile::Pid,
        SessionFile::Version,
        SessionFile::Config,
        SessionFile::Stream,
        SessionFile::Engine,
        SessionFile::Provider,
        SessionFile::Extensions,
        SessionFile::Target,
        SessionFile::Log,
        SessionFile::StartupLock,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            SessionFile::Socket => "sock",
            SessionFile::Port => "port",
            SessionFile::Pid => "pid",
            SessionFile::Version => "version",
            SessionFile::Config => "config",
            SessionFile::Stream => "stream",
            SessionFile::Engine => "engine",
            SessionFile::Provider => "provider",
            SessionFile::Extensions => "extensions",
            SessionFile::Target => "target",
            SessionFile::Log => "log",
            SessionFile::StartupLock => "startup.lock",
        }
    }

    /// Whether the file only describes a running daemon and is removed with it.
    pub fn is_daemon_file(self) -> bool {
        match self {
            SessionFile::Socket
            | SessionFile::Port
            | SessionFile::Pid
            | SessionFile::Version
            | SessionFile::Config
            | SessionFile::Stream
            | SessionFile::Engine
            | SessionFile::Provider
            | SessionFile::Extensions => true,
            SessionFile::Target | SessionFile::Log | SessionFile::StartupLock => false,
        }
    }

    /// Path of this file for `session` in the socket directory.
    pub fn path(self, session: &str) -> PathBuf {
        self.path_in(&get_socket_dir(), session)
    }

    /// Path of this file for `session` in `dir`.
    pub fn path_in(self, dir: &Path, session: &str) -> PathBuf {
        dir.join(format!("{}.{}", session, self.extension()))
    }
}

/// Remove every daemon file of `session` in `dir`.
pub fn remove_daemon_files(dir: &Path, session: &str) {
    for file in SessionFile::ALL.iter().filter(|f| f.is_daemon_file()) {
        let _ = fs::remove_file(file.path_in(dir, session));
    }
}

/// Whether `<session>.pid` in `dir` holds `pid`.
pub fn owns_session(dir: &Path, session: &str, pid: u32) -> bool {
    fs::read_to_string(SessionFile::Pid.path_in(dir, session))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        == Some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_all_files(dir: &Path, session: &str) {
        for file in SessionFile::ALL {
            fs::write(file.path_in(dir, session), "x").unwrap();
        }
    }

    #[test]
    fn test_all_has_unique_extensions() {
        let mut extensions: Vec<_> = SessionFile::ALL.iter().map(|f| f.extension()).collect();
        extensions.sort_unstable();
        extensions.dedup();
        assert_eq!(extensions.len(), SessionFile::ALL.len());
    }

    #[test]
    fn test_remove_daemon_files_keeps_persistent_files() {
        let dir = tempfile::tempdir().unwrap();
        write_all_files(dir.path(), "s");
        remove_daemon_files(dir.path(), "s");
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["s.log", "s.startup.lock", "s.target"]);
        assert_eq!(
            fs::read_to_string(SessionFile::Target.path_in(dir.path(), "s")).unwrap(),
            "x"
        );
    }

    #[test]
    fn test_remove_daemon_files_leaves_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        write_all_files(dir.path(), "other");
        remove_daemon_files(dir.path(), "s");
        for file in SessionFile::ALL {
            assert!(file.path_in(dir.path(), "other").exists());
        }
    }

    #[test]
    fn test_owns_session_when_pid_matches() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(SessionFile::Pid.path_in(dir.path(), "s"), "4242").unwrap();
        assert!(owns_session(dir.path(), "s", 4242));
    }

    #[test]
    fn test_owns_session_false_for_newer_daemon() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(SessionFile::Pid.path_in(dir.path(), "s"), "5151\n").unwrap();
        assert!(!owns_session(dir.path(), "s", 4242));
    }

    #[test]
    fn test_owns_session_false_without_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!owns_session(dir.path(), "s", 4242));
    }
}
