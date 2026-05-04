use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

/// Map an internal action name back to its CLI parent command.
///
/// When a user writes `get title`, the parser emits `{ "action": "title" }`.
/// Policy files use CLI-facing names like `"get"`, so the policy check must
/// also match the parent command.  Returns `None` for actions that are
/// already top-level commands (e.g. `click`, `fill`, `open`).
pub fn action_category(action: &str) -> Option<&'static str> {
    match action {
        // get <sub>
        "title" | "url" | "cdp_url" | "gettext" | "innerhtml" | "inputvalue"
        | "getattribute" | "count" | "boundingbox" | "styles" => Some("get"),
        // is <sub>
        "isvisible" | "isenabled" | "ischecked" => Some("is"),
        // find <sub>
        "getbyrole" | "getbytext" | "getbylabel" | "getbyplaceholder"
        | "getbyalttext" | "getbytitle" | "getbytestid" | "nth" => Some("find"),
        // diff <sub>
        "diff_snapshot" | "diff_screenshot" | "diff_url" => Some("diff"),
        _ => None,
    }
}

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPolicy {
    #[serde(skip)]
    path: PathBuf,
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
            || action_category(action).map_or(false, |cat| self.categories.contains(cat))
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
    pub fn load_if_exists() -> Option<Self> {
        let path = env::var("AGENT_BROWSER_ACTION_POLICY")
            .or_else(|_| env::var("AGENT_BROWSER_POLICY"))
            .ok()?;
        Self::load(&path).ok()
    }

    /// Check whether an action is allowed, denied, or requires confirmation.
    ///
    /// Both the exact internal action name (e.g. `"title"`) and its CLI parent
    /// category (e.g. `"get"`) are tested.  This lets users write
    /// `"allow": ["get"]` and have it cover `get title`, `get url`, etc.
    pub fn check(&self, action: &str) -> PolicyResult {
        let category = action_category(action);

        // Helper: true if the action OR its parent category appears in `list`.
        let matches = |list: &[String]| -> bool {
            list.iter().any(|a| a == action)
                || category.map_or(false, |cat| list.iter().any(|a| a == cat))
        };

        if let Some(deny) = &self.deny {
            if matches(deny) {
                return PolicyResult::Deny(format!("Action '{}' is denied by policy", action));
            }
        }

        if let Some(confirm) = &self.confirm {
            if matches(confirm) {
                return PolicyResult::RequiresConfirmation;
            }
        }

        if let Some(allow) = &self.allow {
            if !allow.is_empty() && !matches(allow) {
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

    /// Reload policy from the file. Re-reads the JSON and updates the policy.
    pub fn reload(&mut self) -> Result<(), String> {
        let contents = fs::read_to_string(&self.path)
            .map_err(|e| format!("Failed to read policy file: {}", e))?;
        let mut policy: ActionPolicy =
            serde_json::from_str(&contents).map_err(|e| format!("Invalid policy JSON: {}", e))?;
        policy.path = self.path.clone();
        *self = policy;
        Ok(())
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
    fn test_action_category_mapping() {
        assert_eq!(action_category("title"), Some("get"));
        assert_eq!(action_category("url"), Some("get"));
        assert_eq!(action_category("gettext"), Some("get"));
        assert_eq!(action_category("innerhtml"), Some("get"));
        assert_eq!(action_category("inputvalue"), Some("get"));
        assert_eq!(action_category("getattribute"), Some("get"));
        assert_eq!(action_category("count"), Some("get"));
        assert_eq!(action_category("boundingbox"), Some("get"));
        assert_eq!(action_category("styles"), Some("get"));
        assert_eq!(action_category("isvisible"), Some("is"));
        assert_eq!(action_category("isenabled"), Some("is"));
        assert_eq!(action_category("ischecked"), Some("is"));
        assert_eq!(action_category("getbyrole"), Some("find"));
        assert_eq!(action_category("getbytitle"), Some("find"));
        assert_eq!(action_category("nth"), Some("find"));
        assert_eq!(action_category("click"), None);
        assert_eq!(action_category("fill"), None);
        assert_eq!(action_category("open"), None);
    }

    #[test]
    fn test_policy_allow_category_covers_subactions() {
        let json = r#"{"allow": ["get", "click"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("click"), PolicyResult::Allow);
        assert_eq!(policy.check("title"), PolicyResult::Allow);
        assert_eq!(policy.check("url"), PolicyResult::Allow);
        assert_eq!(policy.check("gettext"), PolicyResult::Allow);
        assert_eq!(policy.check("innerhtml"), PolicyResult::Allow);
        assert_eq!(policy.check("boundingbox"), PolicyResult::Allow);
        assert!(matches!(policy.check("fill"), PolicyResult::Deny(_)));
        assert!(matches!(policy.check("isvisible"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_policy_deny_category_blocks_subactions() {
        let json = r#"{"deny": ["get"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert!(matches!(policy.check("title"), PolicyResult::Deny(_)));
        assert!(matches!(policy.check("url"), PolicyResult::Deny(_)));
        assert!(matches!(policy.check("gettext"), PolicyResult::Deny(_)));
        assert_eq!(policy.check("click"), PolicyResult::Allow);
    }

    #[test]
    fn test_policy_confirm_category_covers_subactions() {
        let json = r#"{"confirm": ["get"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.check("title"), PolicyResult::RequiresConfirmation);
        assert_eq!(policy.check("url"), PolicyResult::RequiresConfirmation);
        assert_eq!(policy.check("click"), PolicyResult::Allow);
    }

    #[test]
    fn test_policy_deny_category_overrides_allow_subaction() {
        let json = r#"{"allow": ["title"], "deny": ["get"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();
        assert!(matches!(policy.check("title"), PolicyResult::Deny(_)));
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

    #[test]
    fn test_confirm_actions_category_covers_subactions() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_CONFIRM_ACTIONS"]);
        _guard.set("AGENT_BROWSER_CONFIRM_ACTIONS", "get");
        let ca = ConfirmActions::from_env().unwrap();
        assert!(ca.requires_confirmation("title"));
        assert!(ca.requires_confirmation("url"));
        assert!(ca.requires_confirmation("gettext"));
        assert!(!ca.requires_confirmation("click"));
        assert!(!ca.requires_confirmation("fill"));
    }
}
