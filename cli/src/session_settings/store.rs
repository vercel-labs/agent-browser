//! Durable storage for the security settings.
//!
//! Allowed domains, the action policy and the confirm categories contain what
//! a session may do, so they outlive the daemon: a restart (idle expiry, crash,
//! kill, upgrade) must not widen containment. They are kept in an owner-only
//! per-session file until `close`.
//!
//! A file that exists but cannot be trusted (unreadable, corrupt, readable by
//! others on Unix) is an error, never an empty session: the daemon refuses to
//! start and says how to clear it.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session_files::SessionFile;

use super::running::{Running, SecurityView};
use super::spec::Key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// A stored value exists but cannot be read, parsed or trusted.
    SecuredUnreadable(String),
    /// A value could not be written or cleared.
    PersistFailed(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::SecuredUnreadable(message) | StoreError::PersistFailed(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// Where the security settings of one session are kept.
pub trait Store: Send + Sync {
    /// `Ok(None)` means nothing is stored; `Err` means something is stored and
    /// cannot be used.
    fn load(&self) -> Result<Option<SecurityView>, StoreError>;
    /// Replaces what is stored. Durable when it returns.
    fn save(&self, view: &SecurityView) -> Result<(), StoreError>;
    /// Forgets what is stored, readable or not.
    fn clear(&self) -> Result<(), StoreError>;
}

/// The on-disk document.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allowed_domains: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confirm_actions: Option<String>,
}

impl From<&SecurityView> for Document {
    fn from(view: &SecurityView) -> Document {
        Document {
            allowed_domains: view.allowed_domains.clone(),
            action_policy: view.action_policy_path.clone(),
            confirm_actions: view.confirm_actions.clone(),
        }
    }
}

impl From<Document> for SecurityView {
    fn from(doc: Document) -> SecurityView {
        SecurityView {
            allowed_domains: doc.allowed_domains,
            action_policy_path: doc.action_policy,
            confirm_actions: doc.confirm_actions,
        }
    }
}

/// Why a file with this owner and mode cannot be trusted, if it cannot: it
/// must belong to the daemon's user and be closed to everyone else.
#[cfg(unix)]
fn untrusted_reason(uid: u32, euid: u32, mode: u32) -> Option<String> {
    if uid != euid {
        return Some("it belongs to another user".to_string());
    }
    (mode & 0o077 != 0).then(|| {
        format!(
            "it is accessible by other users (mode {:04o})",
            mode & 0o7777
        )
    })
}

/// `{socket_dir}/{session}.settings`. Windows has no mode check: the file
/// inherits the access rules of the socket directory.
pub struct FsStore {
    path: PathBuf,
}

impl FsStore {
    pub fn new(dir: &Path, session: &str) -> FsStore {
        FsStore {
            path: SessionFile::Settings.path_in(dir, session),
        }
    }

    /// Reads the file without following a symlink. On Unix the opened file
    /// must also belong to this user and be owner-only.
    fn read_trusted(&self) -> Result<Option<String>, StoreError> {
        let meta = match fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.unreadable(&e.to_string())),
        };
        if meta.file_type().is_symlink() {
            return Err(self.unreadable("it is a symbolic link"));
        }
        if !meta.is_file() {
            return Err(self.unreadable("it is not a regular file"));
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = match options.open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.unreadable(&e.to_string())),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file
                .metadata()
                .map_err(|e| self.unreadable(&e.to_string()))?;
            // SAFETY: geteuid has no preconditions and cannot fail.
            let euid = unsafe { libc::geteuid() };
            if let Some(why) = untrusted_reason(opened.uid(), euid, opened.mode()) {
                return Err(self.unreadable(&why));
            }
        }
        let mut raw = String::new();
        std::io::Read::read_to_string(&mut file, &mut raw)
            .map_err(|e| self.unreadable(&e.to_string()))?;
        Ok(Some(raw))
    }

    fn unreadable(&self, why: &str) -> StoreError {
        StoreError::SecuredUnreadable(format!(
            "session settings file {} cannot be used: {}",
            self.path.display(),
            why
        ))
    }

    fn persist_failed(&self, what: &str, path: &Path, e: &dyn fmt::Display) -> StoreError {
        StoreError::PersistFailed(format!("cannot {} {}: {}", what, path.display(), e))
    }
}

impl Store for FsStore {
    fn load(&self) -> Result<Option<SecurityView>, StoreError> {
        let Some(raw) = self.read_trusted()? else {
            return Ok(None);
        };
        serde_json::from_str::<Document>(&raw)
            .map(|doc| Some(doc.into()))
            .map_err(|e| self.unreadable(&format!("it is not valid ({})", e)))
    }

    fn save(&self, view: &SecurityView) -> Result<(), StoreError> {
        let raw = serde_json::to_string(&Document::from(view)).map_err(|e| {
            StoreError::PersistFailed(format!("cannot serialize session settings: {}", e))
        })?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| self.persist_failed("create socket dir", dir, &e))?;
        }
        let tmp = self
            .path
            .with_extension(format!("settings.tmp.{}", std::process::id()));
        let written = (|| -> Result<(), StoreError> {
            // A fresh file only: a name an attacker planted (a symlink, say)
            // is removed and never written through.
            let open = || {
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                }
                options.open(&tmp)
            };
            let mut file = match open() {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    fs::remove_file(&tmp).map_err(|e| self.persist_failed("remove", &tmp, &e))?;
                    open()
                }
                other => other,
            }
            .map_err(|e| self.persist_failed("create", &tmp, &e))?;
            file.write_all(raw.as_bytes())
                .map_err(|e| self.persist_failed("write", &tmp, &e))?;
            file.sync_all()
                .map_err(|e| self.persist_failed("sync", &tmp, &e))?;
            drop(file);
            fs::rename(&tmp, &self.path).map_err(|e| self.persist_failed("replace", &self.path, &e))
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }

    fn clear(&self) -> Result<(), StoreError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(self.persist_failed("remove", &self.path, &e)),
        }
    }
}

/// Removes the settings file of `session` without reading it. For the CLI,
/// which clears on `close` whether or not a daemon can start. An empty
/// directory in its place is removed too, so `close` can clear any entry the
/// daemon refuses; anything else that cannot be removed is an error naming
/// the path.
pub fn clear_file(session: &str) -> Result<(), String> {
    clear_path(&SessionFile::Settings.path(session))
}

fn clear_path(path: &Path) -> Result<(), String> {
    let err = match fs::remove_file(path) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => e,
    };
    // A directory, or on Windows a directory symlink, is not a file.
    let is_dir_like = fs::symlink_metadata(path)
        .map(|m| m.is_dir() || m.file_type().is_symlink())
        .unwrap_or(false);
    if is_dir_like && fs::remove_dir(path).is_ok() {
        return Ok(());
    }
    Err(format!(
        "session settings {} could not be removed ({err}); remove it by hand",
        path.display()
    ))
}

/// Starts the error a daemon prints when it refuses to start because of the
/// stored settings file.
const REFUSAL_PREFIX: &str = "Stored session settings cannot be used";

/// Whether a daemon start failure (its stderr, as the parent reports it) is
/// that refusal.
pub fn is_refusal(error: &str) -> bool {
    error.contains(REFUSAL_PREFIX)
}

/// `arg` as one shell word: unchanged when it is plain, single-quoted
/// otherwise.
fn shell_arg(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// The sessions with a settings file in `dir` that `is_live` rejects: what
/// `close --all` finds for sessions whose daemon is gone (idle exit, kill, a
/// refused start). They are closed like any session, so the stored settings
/// still govern the close.
pub fn orphan_sessions(dir: &Path, is_live: impl Fn(&str) -> bool) -> Vec<String> {
    let extension = format!(".{}", SessionFile::Settings.extension());
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut sessions: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let session = name.strip_suffix(&extension)?.to_string();
            (!session.is_empty() && !is_live(&session)).then_some(session)
        })
        .collect();
    sessions.sort();
    sessions
}

/// In-memory store for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemStore {
    value: std::sync::Mutex<Option<SecurityView>>,
}

#[cfg(test)]
impl MemStore {
    pub fn stored(&self) -> Option<SecurityView> {
        self.value.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Store for MemStore {
    fn load(&self) -> Result<Option<SecurityView>, StoreError> {
        Ok(self.stored())
    }

    fn save(&self, view: &SecurityView) -> Result<(), StoreError> {
        *self.value.lock().unwrap() = Some(view.clone());
        Ok(())
    }

    fn clear(&self) -> Result<(), StoreError> {
        *self.value.lock().unwrap() = None;
        Ok(())
    }
}

impl Running {
    /// The session's settings at daemon start: the environment seed, then the
    /// stored security settings on top. The stored file wins, the environment
    /// only fills in when there is none, and a seed that carries security
    /// settings is stored so the next start keeps them.
    pub fn boot(store: &dyn Store, session: &str) -> Result<Running, StoreError> {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let namespace = env("AGENT_BROWSER_NAMESPACE");
        let socket_dir = env("AGENT_BROWSER_SOCKET_DIR");
        Running::boot_from(
            Running::from_env(),
            store,
            session,
            namespace.as_deref(),
            socket_dir.as_deref(),
        )
    }

    pub(super) fn boot_from(
        mut seed: Running,
        store: &dyn Store,
        session: &str,
        namespace: Option<&str>,
        socket_dir: Option<&str>,
    ) -> Result<Running, StoreError> {
        match store.load() {
            Ok(Some(view)) => seed.set_security(&view),
            Ok(None) => {
                let view = seed.security();
                if view != SecurityView::default() {
                    store.save(&view)?;
                }
            }
            Err(StoreError::SecuredUnreadable(why)) => {
                // The parent shows at most 500 bytes of this, so the way out
                // comes first and the detail is cut.
                // The hinted `close` must reach the same socket dir and
                // namespace as this daemon, or it clears nothing.
                let socket_dir = socket_dir
                    .map(|dir| format!("AGENT_BROWSER_SOCKET_DIR={} ", shell_arg(dir)))
                    .unwrap_or_default();
                let namespace = namespace
                    .map(|ns| format!(" --namespace {}", shell_arg(ns)))
                    .unwrap_or_default();
                let mut end = why.len().min(300);
                while !why.is_char_boundary(end) {
                    end -= 1;
                }
                return Err(StoreError::SecuredUnreadable(format!(
                    "{}; to clear them run: {}agent-browser{} --session {} close. {}",
                    REFUSAL_PREFIX,
                    socket_dir,
                    namespace,
                    shell_arg(session),
                    &why[..end]
                )));
            }
            Err(e) => return Err(e),
        }
        Ok(seed)
    }

    /// Replaces the three security settings with `view`; a setting it does not
    /// carry goes back to its default.
    pub fn set_security(&mut self, view: &SecurityView) {
        self.put_back(
            Key::AllowedDomains,
            view.allowed_domains.clone().map(Value::from),
        );
        self.put_back(
            Key::ActionPolicy,
            view.action_policy_path.clone().map(Value::from),
        );
        self.put_back(
            Key::ConfirmActions,
            view.confirm_actions.clone().map(Value::from),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;

    fn full_view() -> SecurityView {
        SecurityView {
            allowed_domains: Some(vec!["example.com".into(), "*.example.org".into()]),
            action_policy_path: Some("relative/policy.json".into()),
            confirm_actions: Some("navigate,click".into()),
        }
    }

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> Running {
        Running::from_lookup(|name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        })
    }

    /// A file as the store would have left it, owner-only, with `contents`.
    fn write_private(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn test_fs_store_round_trips_every_setting() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path(), "s");
        assert_eq!(store.load(), Ok(None));

        store.save(&full_view()).unwrap();

        assert_eq!(store.load(), Ok(Some(full_view())));
    }

    #[test]
    fn test_fs_store_keeps_an_empty_list_apart_from_no_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path(), "s");
        let cleared = SecurityView {
            allowed_domains: Some(Vec::new()),
            ..SecurityView::default()
        };

        store.save(&cleared).unwrap();

        assert_eq!(store.load(), Ok(Some(cleared)));
    }

    #[test]
    fn test_fs_store_replaces_the_previous_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path(), "s");
        store.save(&full_view()).unwrap();
        let next = SecurityView {
            confirm_actions: Some("eval".into()),
            ..SecurityView::default()
        };

        store.save(&next).unwrap();

        assert_eq!(store.load(), Ok(Some(next)));
    }

    #[cfg(unix)]
    #[test]
    fn test_fs_store_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path(), "s");

        store.save(&full_view()).unwrap();
        store.save(&full_view()).unwrap();

        let mode = fs::metadata(dir.path().join("s.settings"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn test_fs_store_leaves_no_temp_file_after_a_save() {
        let dir = tempfile::tempdir().unwrap();
        FsStore::new(dir.path(), "s").save(&full_view()).unwrap();
        assert_eq!(entries(dir.path()), ["s.settings"]);
    }

    #[test]
    fn test_fs_store_removes_the_temp_file_when_the_save_fails() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file belongs makes the final rename fail.
        fs::create_dir(dir.path().join("s.settings")).unwrap();
        let store = FsStore::new(dir.path(), "s");

        let err = store.save(&full_view()).unwrap_err();

        assert!(matches!(err, StoreError::PersistFailed(_)), "{err}");
        assert_eq!(entries(dir.path()), ["s.settings"]);
    }

    #[test]
    fn test_fs_store_clear_removes_the_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path(), "s");
        store.save(&full_view()).unwrap();

        store.clear().unwrap();
        store.clear().unwrap();

        assert_eq!(store.load(), Ok(None));
        assert!(entries(dir.path()).is_empty());
    }

    #[test]
    fn test_fs_store_clear_removes_a_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        write_private(&dir.path().join("s.settings"), "{not json");
        let store = FsStore::new(dir.path(), "s");
        assert!(store.load().is_err());

        store.clear().unwrap();

        assert_eq!(store.load(), Ok(None));
    }

    #[test]
    fn test_fs_store_sessions_do_not_share_a_file() {
        let dir = tempfile::tempdir().unwrap();
        FsStore::new(dir.path(), "a").save(&full_view()).unwrap();
        assert_eq!(FsStore::new(dir.path(), "b").load(), Ok(None));
    }

    #[test]
    fn test_fs_store_directories_are_namespaces() {
        let one = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        FsStore::new(one.path(), "s").save(&full_view()).unwrap();
        assert_eq!(FsStore::new(other.path(), "s").load(), Ok(None));
    }

    #[test]
    fn test_boot_refuses_a_corrupt_file_and_names_the_way_out() {
        let dir = tempfile::tempdir().unwrap();
        write_private(&dir.path().join("work.settings"), "{not json");
        let store = FsStore::new(dir.path(), "work");

        let err = Running::boot_from(Running::defaults(), &store, "work", None, None).unwrap_err();

        let StoreError::SecuredUnreadable(message) = err else {
            panic!("expected SecuredUnreadable, got {err:?}");
        };
        assert!(message.contains("work.settings"), "{message}");
        assert!(
            message.contains("agent-browser --session work close"),
            "{message}"
        );
    }

    #[test]
    fn test_boot_refuses_a_file_with_the_wrong_shape() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("s.settings"),
            r#"{"allowedDomains":"a.com"}"#,
        )
        .unwrap();
        let store = FsStore::new(dir.path(), "s");
        assert!(Running::boot_from(Running::defaults(), &store, "s", None, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_boot_refuses_a_file_other_users_can_read() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path(), "s");
        store.save(&full_view()).unwrap();
        let path = dir.path().join("s.settings");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let err = Running::boot_from(Running::defaults(), &store, "s", None, None).unwrap_err();

        let StoreError::SecuredUnreadable(message) = err else {
            panic!("expected SecuredUnreadable, got {err:?}");
        };
        assert!(message.contains("0644"), "{message}");
        assert!(message.contains("s.settings"), "{message}");
    }

    #[test]
    fn test_boot_refusal_puts_the_way_out_first_and_fits_the_stderr_cap() {
        let root = tempfile::tempdir().unwrap();
        // A long socket directory makes the file part of the message long.
        let dir = root.path().join("d".repeat(150)).join("e".repeat(150));
        fs::create_dir_all(&dir).unwrap();
        write_private(&dir.join("work.settings"), "{not json");
        let store = FsStore::new(&dir, "work");

        let err = Running::boot_from(Running::defaults(), &store, "work", None, None).unwrap_err();

        let message = err.to_string();
        assert!(message.len() < 500, "{} bytes: {message}", message.len());
        let hint = message.find("agent-browser --session work close").unwrap();
        assert!(hint < message.find("work.settings").unwrap_or(usize::MAX));
        assert!(
            message.contains("dddd"),
            "the file is still named: {message}"
        );
    }

    #[test]
    fn test_boot_refusal_names_the_namespace_when_one_is_in_effect() {
        let dir = tempfile::tempdir().unwrap();
        write_private(&dir.path().join("s.settings"), "{not json");
        let store = FsStore::new(dir.path(), "s");

        let err =
            Running::boot_from(Running::defaults(), &store, "s", Some("team a"), None).unwrap_err();

        assert!(
            err.to_string()
                .contains("agent-browser --namespace 'team a' --session s close"),
            "{err}"
        );
    }

    #[test]
    fn test_boot_refusal_names_the_socket_dir_when_one_is_set() {
        let dir = tempfile::tempdir().unwrap();
        write_private(&dir.path().join("s.settings"), "{not json");
        let store = FsStore::new(dir.path(), "s");

        let err = Running::boot_from(
            Running::defaults(),
            &store,
            "s",
            Some("team"),
            Some("/tmp/ab sock"),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains(
                "AGENT_BROWSER_SOCKET_DIR='/tmp/ab sock' agent-browser --namespace team --session s close"
            ),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_load_refuses_a_symlink_even_to_a_good_file() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        write_private(&real, r#"{"allowedDomains":["a.com"]}"#);
        std::os::unix::fs::symlink(&real, dir.path().join("s.settings")).unwrap();

        let err = FsStore::new(dir.path(), "s").load().unwrap_err();

        assert!(err.to_string().contains("symbolic link"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn test_untrusted_reason_checks_owner_and_mode() {
        assert_eq!(untrusted_reason(501, 501, 0o100600), None);
        assert!(untrusted_reason(0, 501, 0o100600)
            .unwrap()
            .contains("another user"));
        assert!(untrusted_reason(501, 501, 0o100640)
            .unwrap()
            .contains("0640"));
    }

    #[cfg(unix)]
    #[test]
    fn test_save_never_writes_through_a_planted_temp_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, "precious").unwrap();
        let tmp = dir
            .path()
            .join(format!("s.settings.tmp.{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();
        let store = FsStore::new(dir.path(), "s");

        store.save(&full_view()).unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
        assert_eq!(store.load(), Ok(Some(full_view())));
        assert!(!tmp.exists() && tmp.symlink_metadata().is_err());
    }

    #[test]
    fn test_save_replaces_a_stale_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir
            .path()
            .join(format!("s.settings.tmp.{}", std::process::id()));
        fs::write(&tmp, "stale").unwrap();
        let store = FsStore::new(dir.path(), "s");

        store.save(&full_view()).unwrap();

        assert_eq!(store.load(), Ok(Some(full_view())));
        assert_eq!(entries(dir.path()), ["s.settings"]);
    }

    #[test]
    fn test_shell_arg_quotes_only_when_needed() {
        assert_eq!(shell_arg("work-1.a"), "work-1.a");
        assert_eq!(shell_arg("team a"), "'team a'");
        assert_eq!(shell_arg("it's"), "'it'\\''s'");
        assert_eq!(shell_arg(""), "''");
        assert_eq!(shell_arg("a;rm"), "'a;rm'");
    }

    #[test]
    fn test_boot_without_a_file_makes_either_policy_variable_absolute() {
        let cwd = std::env::current_dir().unwrap();
        let expected = cwd.join("rel/policy.json");
        for pairs in [
            &[("AGENT_BROWSER_ACTION_POLICY", "rel/policy.json")][..],
            &[("AGENT_BROWSER_POLICY", "rel/policy.json")][..],
        ] {
            let seed = Running::from_lookup(|name| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            });
            assert_eq!(
                seed.security().action_policy_path.as_deref(),
                expected.to_str(),
                "{pairs:?}"
            );
        }
    }

    #[test]
    fn test_boot_stored_settings_win_over_the_environment() {
        let store = MemStore::default();
        store.save(&full_view()).unwrap();
        let seed = lookup(&[
            ("AGENT_BROWSER_ALLOWED_DOMAINS", "other.com"),
            ("AGENT_BROWSER_ACTION_POLICY", "/env/policy.json"),
            ("AGENT_BROWSER_CONFIRM_ACTIONS", "eval"),
        ]);

        let running = Running::boot_from(seed, &store, "s", None, None).unwrap();

        assert_eq!(running.security(), full_view());
    }

    #[test]
    fn test_boot_stored_file_is_complete_for_the_three_settings() {
        let store = MemStore::default();
        store
            .save(&SecurityView {
                allowed_domains: Some(vec!["a.com".into()]),
                ..SecurityView::default()
            })
            .unwrap();
        let seed = lookup(&[("AGENT_BROWSER_CONFIRM_ACTIONS", "eval")]);

        let running = Running::boot_from(seed, &store, "s", None, None).unwrap();

        assert_eq!(running.security().confirm_actions, None);
        assert_eq!(
            running.security().allowed_domains,
            Some(vec!["a.com".to_string()])
        );
    }

    #[test]
    fn test_boot_without_a_file_uses_the_environment_and_stores_it() {
        let store = MemStore::default();
        let seed = lookup(&[("AGENT_BROWSER_ALLOWED_DOMAINS", "a.com, b.com")]);

        let running = Running::boot_from(seed, &store, "s", None, None).unwrap();

        let expected = SecurityView {
            allowed_domains: Some(vec!["a.com".into(), "b.com".into()]),
            ..SecurityView::default()
        };
        assert_eq!(running.security(), expected);
        assert_eq!(store.stored(), Some(expected));
    }

    #[test]
    fn test_boot_without_a_file_or_security_settings_stores_nothing() {
        let store = MemStore::default();
        let seed = lookup(&[("AGENT_BROWSER_PROXY", "http://p:1")]);

        Running::boot_from(seed, &store, "s", None, None).unwrap();

        assert_eq!(store.stored(), None);
    }

    #[test]
    fn test_orphan_sessions_lists_dead_sessions_only_and_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "dead.settings",
            "live.settings",
            "dead.target",
            "x.settings.tmp.1",
        ] {
            fs::write(dir.path().join(name), "{}").unwrap();
        }

        let orphans = orphan_sessions(dir.path(), |s| s == "live");

        assert_eq!(orphans, ["dead"]);
        assert_eq!(entries(dir.path()).len(), 4);
    }

    #[test]
    fn test_orphan_sessions_on_a_missing_directory_is_empty() {
        assert!(orphan_sessions(Path::new("/nonexistent/ab-dir"), |_| false).is_empty());
    }

    #[test]
    fn test_clear_file_removes_the_session_file_in_the_socket_dir() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_SOCKET_DIR",
            "AGENT_BROWSER_NAMESPACE",
            "XDG_RUNTIME_DIR",
        ]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
        guard.remove("AGENT_BROWSER_NAMESPACE");
        let path = SessionFile::Settings.path("s");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, "{not json");
        let other = SessionFile::Settings.path("other");
        fs::write(&other, "{}").unwrap();

        clear_file("s").unwrap();

        assert!(!path.exists());
        assert!(other.exists());
    }

    #[test]
    fn test_clear_path_removes_an_empty_directory_in_place_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.settings");
        fs::create_dir(&path).unwrap();

        clear_path(&path).unwrap();

        assert!(!path.exists());
        clear_path(&path).unwrap();
    }

    #[test]
    fn test_clear_path_names_an_entry_it_cannot_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.settings");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), "x").unwrap();

        let err = clear_path(&path).unwrap_err();

        assert!(err.contains("s.settings"), "{err}");
        assert!(path.join("keep").exists());
    }
}
