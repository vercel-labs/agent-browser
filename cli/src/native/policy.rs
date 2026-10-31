use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

/// Result of a policy check for an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyResult {
    /// Action is allowed.
    Allow,
    /// Action is blocked with the given reason.
    Deny(String),
    /// Action requires confirmation before proceeding.
    RequiresConfirmation,
}

/// Policy configuration loaded from a JSON file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionPolicy {
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    load_error: Option<String>,
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    allow: Option<Vec<String>>,
    #[serde(default)]
    deny: Option<Vec<String>>,
    #[serde(default)]
    confirm: Option<Vec<String>>,
}

/// Confirmation categories parsed from AGENT_BROWSER_CONFIRM_ACTIONS.
#[derive(Debug, Clone)]
pub struct ConfirmActions {
    pub categories: HashSet<String>,
}

impl ConfirmActions {
    pub fn from_env() -> Option<Self> {
        let val = env::var("AGENT_BROWSER_CONFIRM_ACTIONS").ok()?;
        if val.is_empty() {
            return None;
        }
        let categories: HashSet<String> = val
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if categories.is_empty() {
            None
        } else {
            Some(Self { categories })
        }
    }

    pub fn requires_confirmation(&self, action: &str) -> bool {
        self.categories.contains(action)
    }
}

impl ActionPolicy {
    /// Load policy from a JSON file at the given path.
    pub fn load(path: &str) -> Result<Self, String> {
        let path_buf = PathBuf::from(path);
        let contents = fs::read_to_string(&path_buf)
            .map_err(|e| format!("Failed to read policy file: {}", e))?;
        let mut policy: ActionPolicy =
            serde_json::from_str(&contents).map_err(|e| format!("Invalid policy JSON: {}", e))?;
        policy.path = path_buf;
        Ok(policy)
    }

    /// Load policy if AGENT_BROWSER_ACTION_POLICY env var is set.
    /// Falls back to AGENT_BROWSER_POLICY for backwards compatibility.
    /// A file that cannot be loaded yields a policy that denies every action except `close`.
    pub fn load_if_exists() -> Option<Self> {
        let path = env::var("AGENT_BROWSER_ACTION_POLICY")
            .or_else(|_| env::var("AGENT_BROWSER_POLICY"))
            .ok()?;
        let mut policy = Self {
            path: PathBuf::from(path),
            ..Self::default()
        };
        policy.reload();
        Some(policy)
    }

    /// Check whether an action is allowed, denied, or requires confirmation.
    pub fn check(&self, action: &str) -> PolicyResult {
        if let Some(err) = &self.load_error {
            if action == "close" {
                return PolicyResult::Allow;
            }
            return PolicyResult::Deny(format!("action policy failed to load: {}", err));
        }

        if let Some(deny) = &self.deny {
            if deny.iter().any(|a| a == action) {
                return PolicyResult::Deny(format!("Action '{}' is denied by policy", action));
            }
        }

        if let Some(confirm) = &self.confirm {
            if confirm.iter().any(|a| a == action) {
                return PolicyResult::RequiresConfirmation;
            }
        }

        if let Some(allow) = self.allow.as_ref().filter(|a| !a.is_empty()) {
            if !allow.iter().any(|a| a == action) {
                let is_default_deny = self
                    .default
                    .as_deref()
                    .map(|d| d.eq_ignore_ascii_case("deny"))
                    .unwrap_or(true);
                if is_default_deny {
                    return PolicyResult::Deny(format!(
                        "Action '{}' is not in the allow list",
                        action
                    ));
                }
            }
        } else if let Some(ref default) = self.default {
            if default.eq_ignore_ascii_case("deny") {
                return PolicyResult::Deny(format!(
                    "Action '{}' denied: default policy is deny",
                    action
                ));
            }
        }

        PolicyResult::Allow
    }

    /// Re-read the policy file. A file that cannot be read or parsed leaves a policy that denies every action except `close`.
    pub fn reload(&mut self) {
        let path = self.path.clone();
        *self = Self::load(&path.to_string_lossy()).unwrap_or_else(|e| Self {
            load_error: Some(format!("{} ({})", e, path.display())),
            path,
            ..Self::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;

    #[test]
    fn test_policy_allow_whitelist() {
        let json = r#"{"allow": ["click", "type"], "deny": [], "confirm": []}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("click"), PolicyResult::Allow);
        assert_eq!(policy.check("type"), PolicyResult::Allow);
        assert!(matches!(policy.check("navigate"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_policy_deny() {
        let json = r#"{"allow": [], "deny": ["delete"], "confirm": []}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert!(matches!(policy.check("delete"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_policy_confirm() {
        let json = r#"{"allow": [], "deny": [], "confirm": ["submit"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("submit"), PolicyResult::RequiresConfirmation);
    }

    #[test]
    fn test_policy_deny_takes_precedence() {
        let json = r#"{"allow": ["danger"], "deny": ["danger"], "confirm": []}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert!(matches!(policy.check("danger"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_policy_confirm_takes_precedence_over_allow() {
        let json = r#"{"allow": ["submit"], "deny": [], "confirm": ["submit"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("submit"), PolicyResult::RequiresConfirmation);
    }

    #[test]
    fn test_policy_empty_allow_allows_all() {
        let json = r#"{"allow": [], "deny": [], "confirm": []}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("anything"), PolicyResult::Allow);
    }

    #[test]
    fn test_policy_missing_allow_allows_all() {
        let json = r#"{"deny": []}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("anything"), PolicyResult::Allow);
    }

    #[test]
    fn test_policy_default_allow() {
        let json = r#"{"default": "allow", "deny": ["navigate"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("click"), PolicyResult::Allow);
        assert!(matches!(policy.check("navigate"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_policy_default_deny() {
        let json = r#"{"default": "deny", "allow": ["click"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("click"), PolicyResult::Allow);
        assert!(matches!(policy.check("navigate"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_policy_default_deny_empty_allow_denies_all() {
        let json = r#"{"default": "deny", "allow": []}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert!(matches!(policy.check("click"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_policy_fails_closed_on_load_and_reload() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ACTION_POLICY", "AGENT_BROWSER_POLICY"]);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("policy.json");
        let path = file.to_string_lossy().to_string();
        guard.set("AGENT_BROWSER_ACTION_POLICY", &path);
        let mut policy = ActionPolicy::load_if_exists().unwrap();
        assert!(matches!(policy.check("evaluate"), PolicyResult::Deny(_)));
        assert_eq!(policy.check("close"), PolicyResult::Allow);

        fs::write(&file, r#"{"default": "allow"}"#).unwrap();
        policy.reload();
        assert_eq!(policy.check("evaluate"), PolicyResult::Allow);

        fs::write(&file, "{bad").unwrap();
        policy.reload();
        match policy.check("evaluate") {
            PolicyResult::Deny(r) => assert!(r.contains(&path)),
            other => panic!("expected deny, got {:?}", other),
        }

        fs::write(&file, r#"{"default": "allow"}"#).unwrap();
        policy.reload();
        assert_eq!(policy.check("evaluate"), PolicyResult::Allow);
    }

    #[test]
    fn test_confirm_actions_from_env() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_CONFIRM_ACTIONS"]);
        _guard.set("AGENT_BROWSER_CONFIRM_ACTIONS", "navigate,click,fill");
        let ca = ConfirmActions::from_env().unwrap();
        assert!(ca.requires_confirmation("navigate"));
        assert!(ca.requires_confirmation("click"));
        assert!(ca.requires_confirmation("fill"));
        assert!(!ca.requires_confirmation("screenshot"));
    }
}
