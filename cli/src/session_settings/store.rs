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

    #[cfg(unix)]
    fn check_owner_only(&self) -> Result<(), StoreError> {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&self.path)
            .map_err(|e| self.unreadable(&e.to_string()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(self.unreadable(&format!(
                "it is accessible by other users (mode {:04o})",
                mode & 0o7777
            )));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn check_owner_only(&self) -> Result<(), StoreError> {
        Ok(())
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
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.unreadable(&e.to_string())),
        };
        self.check_owner_only()?;
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
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&tmp)
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
/// which clears on `close` whether or not a daemon can start.
pub fn clear_file(session: &str) {
    let _ = fs::remove_file(SessionFile::Settings.path(session));
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
        Running::boot_from(Running::from_env(), store, session)
    }

    pub(super) fn boot_from(
        mut seed: Running,
        store: &dyn Store,
        session: &str,
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
                return Err(StoreError::SecuredUnreadable(format!(
                    "{}. To clear it, run: agent-browser --session {} close",
                    why, session
                )))
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

        let err = Running::boot_from(Running::defaults(), &store, "work").unwrap_err();

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
        assert!(Running::boot_from(Running::defaults(), &store, "s").is_err());
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

        let err = Running::boot_from(Running::defaults(), &store, "s").unwrap_err();

        let StoreError::SecuredUnreadable(message) = err else {
            panic!("expected SecuredUnreadable, got {err:?}");
        };
        assert!(message.contains("0644"), "{message}");
        assert!(message.contains("s.settings"), "{message}");
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

        let running = Running::boot_from(seed, &store, "s").unwrap();

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

        let running = Running::boot_from(seed, &store, "s").unwrap();

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

        let running = Running::boot_from(seed, &store, "s").unwrap();

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

        Running::boot_from(seed, &store, "s").unwrap();

        assert_eq!(store.stored(), None);
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

        clear_file("s");

        assert!(!path.exists());
        assert!(other.exists());
    }
}
