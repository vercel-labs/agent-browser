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

/// Return the user-facing policy category for a protocol action.
fn action_category(action: &str) -> Option<&'static str> {
    match action {
        "navigate" | "back" | "forward" | "reload" | "tab_new" => Some("navigate"),
        "click" | "dblclick" | "tap" => Some("click"),
        "fill" | "type" | "keyboard" | "inserttext" | "select" | "multiselect" | "check"
        | "uncheck" | "clear" | "selectall" | "setvalue" => Some("fill"),
        "evaluate" | "evalhandle" | "addscript" | "addinitscript" | "addstyle" | "expose"
        | "setcontent" => Some("eval"),
        "download" | "waitfordownload" => Some("download"),
        "upload" => Some("upload"),
        "snapshot" | "screenshot" | "pdf" | "diff_snapshot" | "diff_screenshot" | "diff_url" => {
            Some("snapshot")
        }
        "scroll" | "scrollintoview" => Some("scroll"),
        "wait" | "waitforurl" | "waitforloadstate" | "waitforfunction" => Some("wait"),
        "read" => Some("read"),
        "gettext" | "content" | "innerhtml" | "innertext" | "inputvalue" | "url" | "title"
        | "getattribute" | "count" | "boundingbox" | "styles" | "isvisible" | "isenabled"
        | "ischecked" | "responsebody" | "getbyrole" | "getbytext" | "getbylabel"
        | "getbyplaceholder" | "getbyalttext" | "getbytitle" | "getbytestid" | "nth" | "find" => {
            Some("get")
        }
        "hover" | "focus" | "drag" | "press" | "keydown" | "keyup" | "mousemove" | "mousedown"
        | "mouseup" | "wheel" | "dispatch" | "mouse" | "swipe" => Some("interact"),
        "route" | "unroute" | "requests" | "request_detail" | "har_start" | "har_stop" => {
            Some("network")
        }
        "state_save" | "state_load" | "cookies_set" | "storage_set" | "credentials" => {
            Some("state")
        }
        _ => None,
    }
}

fn list_matches_action(list: &[String], action: &str) -> bool {
    let category = action_category(action);
    list.iter()
        .any(|entry| entry == action || category.is_some_and(|category| entry == category))
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
            || action_category(action).is_some_and(|category| self.categories.contains(category))
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
    pub fn check(&self, action: &str) -> PolicyResult {
        if let Some(deny) = &self.deny {
            if list_matches_action(deny, action) {
                return PolicyResult::Deny(format!("Action '{}' is denied by policy", action));
            }
        }

        if let Some(confirm) = &self.confirm {
            if list_matches_action(confirm, action) {
                return PolicyResult::RequiresConfirmation;
            }
        }

        if let Some(allow) = &self.allow {
            if !allow.is_empty() && !list_matches_action(allow, action) {
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
    fn test_policy_categories_cover_protocol_actions() {
        let json = r#"{
            "default": "deny",
            "allow": ["navigate", "snapshot", "click", "scroll", "wait", "get"]
        }"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();

        for action in [
            "reload",
            "back",
            "forward",
            "screenshot",
            "pdf",
            "diff_snapshot",
            "dblclick",
            "tap",
            "scrollintoview",
            "waitforurl",
            "title",
            "url",
            "count",
            "isvisible",
            "getbyrole",
        ] {
            assert_eq!(policy.check(action), PolicyResult::Allow, "{action}");
        }
    }

    #[test]
    fn test_policy_category_deny_and_confirm() {
        let json = r#"{
            "default": "allow",
            "deny": ["eval"],
            "confirm": ["network"]
        }"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();

        assert!(matches!(policy.check("setcontent"), PolicyResult::Deny(_)));
        assert_eq!(
            policy.check("har_start"),
            PolicyResult::RequiresConfirmation
        );
        assert_eq!(
            policy.check("request_detail"),
            PolicyResult::RequiresConfirmation
        );
    }

    #[test]
    fn test_policy_preserves_exact_action_names() {
        let json = r#"{"default": "deny", "allow": ["evalhandle"]}"#;
        let policy: ActionPolicy = serde_json::from_str(json).unwrap();

        assert_eq!(policy.check("evalhandle"), PolicyResult::Allow);
        assert!(matches!(policy.check("evaluate"), PolicyResult::Deny(_)));
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
    fn test_confirm_actions_category_from_env() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_CONFIRM_ACTIONS"]);
        _guard.set("AGENT_BROWSER_CONFIRM_ACTIONS", "eval,get");
        let ca = ConfirmActions::from_env().unwrap();

        assert!(ca.requires_confirmation("setcontent"));
        assert!(ca.requires_confirmation("getbyrole"));
        assert!(!ca.requires_confirmation("click"));
    }
}
