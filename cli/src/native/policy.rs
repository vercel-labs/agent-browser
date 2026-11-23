use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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

/// The action categories documented in `docs/content/docs/security.mdx`.
///
/// A policy entry in `allow`, `deny` or `confirm`, and a
/// `--confirm-actions` token, matches an action when it names the action itself
/// or the category that contains it. Each protocol action belongs to at most
/// one category. Actions no category covers are listed in the policy tests, so
/// a new action has to be classified deliberately.
const ACTION_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "navigate",
        &[
            "navigate",
            "back",
            "forward",
            "reload",
            "tab_new",
            "pushstate",
        ],
    ),
    ("click", &["click", "dblclick", "tap"]),
    (
        "fill",
        &[
            "fill",
            "type",
            "keyboard",
            "inserttext",
            "select",
            "multiselect",
            "check",
            "uncheck",
            "clear",
            "selectall",
            "setvalue",
        ],
    ),
    (
        "eval",
        &[
            "evaluate",
            "evalhandle",
            "addscript",
            "addinitscript",
            "removeinitscript",
            "addstyle",
            "expose",
            "setcontent",
        ],
    ),
    ("download", &["download", "waitfordownload"]),
    ("upload", &["upload"]),
    (
        "snapshot",
        &[
            "snapshot",
            "screenshot",
            "pdf",
            "diff_snapshot",
            "diff_screenshot",
            "diff_url",
        ],
    ),
    ("scroll", &["scroll", "scrollintoview"]),
    (
        "wait",
        &["wait", "waitforurl", "waitforloadstate", "waitforfunction"],
    ),
    ("read", &["read"]),
    (
        "get",
        &[
            "gettext",
            "innerhtml",
            "innertext",
            "content",
            "url",
            "title",
            "getattribute",
            "count",
            "boundingbox",
            "styles",
            "isvisible",
            "isenabled",
            "ischecked",
            "inputvalue",
            "getbyrole",
            "getbytext",
            "getbylabel",
            "getbyplaceholder",
            "getbyalttext",
            "getbytitle",
            "getbytestid",
            "nth",
            "find",
        ],
    ),
    (
        "interact",
        &[
            "hover",
            "focus",
            "drag",
            "press",
            "keydown",
            "keyup",
            "mousemove",
            "mousedown",
            "mouseup",
            "wheel",
            "mouse",
            "dispatch",
            "swipe",
            "dialog",
            "input_mouse",
            "input_keyboard",
            "input_touch",
        ],
    ),
    (
        "network",
        &[
            "route",
            "unroute",
            "requests",
            "request_detail",
            "har_start",
            "har_stop",
            "responsebody",
            "headers",
            "offline",
        ],
    ),
    (
        "state",
        &[
            "state_save",
            "state_load",
            "state_list",
            "state_show",
            "state_clear",
            "state_clean",
            "state_rename",
            "cookies_get",
            "cookies_set",
            "cookies_clear",
            "storage_get",
            "storage_set",
            "storage_clear",
        ],
    ),
];

/// The documented category that covers `action`, if any.
fn action_category(action: &str) -> Option<&'static str> {
    ACTION_CATEGORIES
        .iter()
        .find_map(|(category, actions)| actions.contains(&action).then_some(*category))
}

/// Whether a policy entry names `action` exactly or by its documented category.
fn entry_matches_action(entry: &str, action: &str) -> bool {
    entry == action || action_category(action) == Some(entry)
}

fn list_matches_action(entries: &[String], action: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry_matches_action(entry, action))
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

/// Confirmation categories parsed from the confirm-actions setting.
#[derive(Debug, Clone)]
pub struct ConfirmActions {
    pub categories: HashSet<String>,
}

impl ConfirmActions {
    pub fn parse(val: &str) -> Option<Self> {
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

    /// Load the policy at `path`. A file that cannot be loaded yields a policy
    /// that denies every action except `close`.
    pub fn from_path(path: &str) -> Self {
        let mut policy = Self {
            path: PathBuf::from(path),
            ..Self::default()
        };
        policy.reload();
        policy
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
            if list_matches_action(deny, action) {
                return PolicyResult::Deny(format!("Action '{}' is denied by policy", action));
            }
        }

        if let Some(confirm) = &self.confirm {
            if list_matches_action(confirm, action) {
                return PolicyResult::RequiresConfirmation;
            }
        }

        if let Some(allow) = self.allow.as_ref().filter(|a| !a.is_empty()) {
            if !list_matches_action(allow, action) {
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
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("policy.json");
        let path = file.to_string_lossy().to_string();
        let mut policy = ActionPolicy::from_path(&path);
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
    fn test_confirm_actions_parse() {
        let ca = ConfirmActions::parse("navigate,click,fill").unwrap();
        assert!(ca.requires_confirmation("navigate"));
        assert!(ca.requires_confirmation("click"));
        assert!(ca.requires_confirmation("fill"));
        assert!(!ca.requires_confirmation("screenshot"));
    }

    fn parse_policy(json: &str) -> ActionPolicy {
        serde_json::from_str(json).unwrap()
    }

    /// The category table on the security page, parsed here so a code change
    /// that is not mirrored there fails a test instead of drifting silently.
    fn documented_categories() -> Vec<(String, Vec<String>)> {
        let docs = include_str!("../../../docs/content/docs/security.mdx");
        let rows = docs
            .split_once("<tr><th>Category</th><th>Actions</th></tr>")
            .and_then(|(_, rest)| rest.split_once("</tbody>"))
            .map(|(rows, _)| rows)
            .expect("the action category table on the security page");
        rows.split("<tr>")
            .skip(1)
            .map(|row| {
                let category = row
                    .split_once("<td><code>")
                    .and_then(|(_, rest)| rest.split_once("</code></td>"))
                    .map(|(category, _)| category.trim().to_string())
                    .expect("a category cell");
                let actions = row
                    .rsplit_once("<td>")
                    .and_then(|(_, rest)| rest.split_once("</td>"))
                    .map(|(actions, _)| actions)
                    .expect("an actions cell");
                let actions = actions
                    .split(',')
                    .map(str::trim)
                    .filter(|action| !action.is_empty())
                    .map(str::to_string)
                    .collect();
                (category, actions)
            })
            .collect()
    }

    #[test]
    fn test_action_categories_match_the_documented_table() {
        let documented = documented_categories();
        assert!(!documented.is_empty());

        for (category, _) in ACTION_CATEGORIES {
            assert!(
                documented.iter().any(|(name, _)| name == category),
                "category '{category}' is not documented"
            );
        }
        for (category, documented_actions) in &documented {
            let (_, mapped_actions) = ACTION_CATEGORIES
                .iter()
                .find(|(name, _)| name == category)
                .unwrap_or_else(|| {
                    panic!("documented category '{category}' has no action mapping")
                });
            let mut mapped: Vec<&str> = mapped_actions.to_vec();
            let mut documented_actions: Vec<&str> =
                documented_actions.iter().map(String::as_str).collect();
            mapped.sort_unstable();
            documented_actions.sort_unstable();
            assert_eq!(
                mapped, documented_actions,
                "category '{category}' maps different actions than the security page"
            );
        }
    }

    #[test]
    fn test_every_row_of_the_documented_table_matches_allow_deny_and_confirm() {
        let documented = documented_categories();
        for (category, actions) in &documented {
            let outside = documented
                .iter()
                .find_map(|(name, actions)| (name != category).then(|| actions[0].clone()))
                .expect("another category to compare against");
            let allowed = parse_policy(&format!(r#"{{"default":"deny","allow":["{category}"]}}"#));
            let denied = parse_policy(&format!(r#"{{"default":"allow","deny":["{category}"]}}"#));
            let confirmed = parse_policy(&format!(
                r#"{{"default":"allow","confirm":["{category}"]}}"#
            ));

            for action in actions {
                assert_eq!(
                    allowed.check(action),
                    PolicyResult::Allow,
                    "allow '{category}' should admit {action}"
                );
                assert!(
                    matches!(denied.check(action), PolicyResult::Deny(_)),
                    "deny '{category}' should block {action}"
                );
                assert_eq!(
                    confirmed.check(action),
                    PolicyResult::RequiresConfirmation,
                    "confirm '{category}' should gate {action}"
                );
            }

            assert!(
                matches!(allowed.check(&outside), PolicyResult::Deny(_)),
                "allow '{category}' should not admit {outside}"
            );
            assert_eq!(denied.check(&outside), PolicyResult::Allow);
            assert_eq!(confirmed.check(&outside), PolicyResult::Allow);
        }
    }

    #[test]
    fn test_documented_restrictive_example() {
        let policy = parse_policy(
            r#"{"default":"deny","allow":["navigate","snapshot","click","scroll","wait","get"]}"#,
        );

        // Every action the issue reported as wrongly denied now passes.
        for action in [
            "click",
            "dblclick",
            "tap",
            "reload",
            "back",
            "screenshot",
            "pdf",
            "diff_snapshot",
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

        for action in ["evaluate", "upload", "hover", "state_save"] {
            assert!(
                matches!(policy.check(action), PolicyResult::Deny(_)),
                "{action} is outside the example's categories"
            );
        }
    }

    #[test]
    fn test_documented_permissive_example() {
        let policy = parse_policy(r#"{"default":"allow","deny":["eval","download","upload"]}"#);

        for action in [
            "evaluate",
            "evalhandle",
            "addscript",
            "addinitscript",
            "addstyle",
            "expose",
            "setcontent",
            "download",
            "waitfordownload",
            "upload",
        ] {
            assert!(
                matches!(policy.check(action), PolicyResult::Deny(_)),
                "{action} is in a denied category"
            );
        }
        for action in ["click", "navigate", "snapshot", "state_save"] {
            assert_eq!(policy.check(action), PolicyResult::Allow, "{action}");
        }
    }

    #[test]
    fn test_deny_eval_blocks_the_eval_family() {
        // The security page recommends denying `eval` as the mitigation for
        // page scripts restoring the blocked WebSocket/EventSource constructors.
        let policy = parse_policy(r#"{"default":"allow","deny":["eval"]}"#);
        for action in [
            "evaluate",
            "evalhandle",
            "addscript",
            "addinitscript",
            "addstyle",
            "expose",
            "setcontent",
        ] {
            assert!(
                matches!(policy.check(action), PolicyResult::Deny(_)),
                "deny eval should block {action}"
            );
        }
        assert_eq!(policy.check("click"), PolicyResult::Allow);
    }

    #[test]
    fn test_state_category_covers_saved_state_and_cookies() {
        let policy = parse_policy(r#"{"default":"allow","deny":["state"]}"#);
        for action in [
            "state_save",
            "state_load",
            "cookies_set",
            "cookies_get",
            "storage_set",
            "storage_get",
        ] {
            assert!(
                matches!(policy.check(action), PolicyResult::Deny(_)),
                "deny state should block {action}"
            );
        }
        assert_eq!(policy.check("click"), PolicyResult::Allow);
        assert_eq!(policy.check("snapshot"), PolicyResult::Allow);
    }

    #[test]
    fn test_exact_action_names_still_match() {
        let policy = parse_policy(r#"{"default":"deny","allow":["evalhandle"]}"#);
        assert_eq!(policy.check("evalhandle"), PolicyResult::Allow);
        assert!(matches!(policy.check("evaluate"), PolicyResult::Deny(_)));

        // Unknown names behave as before: they match no action.
        let policy = parse_policy(r#"{"default":"allow","deny":["not_an_action"]}"#);
        assert_eq!(policy.check("click"), PolicyResult::Allow);
        let policy = parse_policy(r#"{"default":"deny","allow":["not_an_action"]}"#);
        assert!(matches!(policy.check("click"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_deny_category_wins_over_an_exact_allow() {
        let policy = parse_policy(r#"{"default":"allow","allow":["evalhandle"],"deny":["eval"]}"#);
        assert!(matches!(policy.check("evalhandle"), PolicyResult::Deny(_)));
        assert!(matches!(policy.check("evaluate"), PolicyResult::Deny(_)));
    }

    #[test]
    fn test_confirm_actions_match_categories_and_exact_names() {
        let ca = ConfirmActions::parse("get,eval").unwrap();
        for action in [
            "getbyrole",
            "gettext",
            "url",
            "count",
            "isvisible",
            "evaluate",
            "setcontent",
        ] {
            assert!(ca.requires_confirmation(action), "{action}");
        }
        assert!(!ca.requires_confirmation("click"));

        let ca = ConfirmActions::parse("evaluate").unwrap();
        assert!(ca.requires_confirmation("evaluate"));
        assert!(!ca.requires_confirmation("evalhandle"));
    }

    /// Protocol actions no documented category covers, grouped by feature. A
    /// policy entry matches these only by exact name. Keep this list in step
    /// with the daemon dispatch and the CLI parser; the test below fails when
    /// an action is in neither a category nor this list.
    const UNCATEGORIZED_ACTIONS: &[&str] = &[
        // Session lifecycle.
        "launch",
        "close",
        "inspect",
        "session_info",
        "bringtofront",
        "window_new",
        "tab_list",
        "tab_switch",
        "tab_close",
        // Observability, frames and page inspection.
        "console",
        "errors",
        "highlight",
        "clipboard",
        "a11y",
        "vitals",
        "frame",
        "mainframe",
        // Emulation and device settings.
        "viewport",
        "user_agent",
        // The daemon dispatch accepts both spellings of the user agent action.
        "useragent",
        "device",
        "device_list",
        "timezone",
        "locale",
        "geolocation",
        "permissions",
        "emulatemedia",
        "set_media",
        // Tracing, profiling, recording and streaming.
        "trace_start",
        "trace_stop",
        "profiler_start",
        "profiler_stop",
        "recording_start",
        "recording_stop",
        "recording_restart",
        "video_start",
        "video_stop",
        "screencast_start",
        "screencast_stop",
        "stream_enable",
        "stream_disable",
        "stream_status",
        // React instrumentation.
        "react_inspect",
        "react_tree",
        "react_suspense",
        "react_renders_start",
        "react_renders_stop",
        // WebMCP.
        "webmcp_list",
        "webmcp_invoke",
        "webmcp_result",
        "webmcp_cancel",
        // Saved flows.
        "flow_run",
        "flow_save",
        "flow_steps",
        "flow_list",
        // Credentials and authentication.
        "credentials",
        "credentials_set",
        "credentials_get",
        "credentials_delete",
        "credentials_list",
        "auth_save",
        "auth_login",
        "auth_list",
        "auth_show",
        "auth_delete",
        // Confirmation bookkeeping, batching and connection introspection.
        "confirm",
        "deny",
        "batch",
        "pause",
        "cdp_url",
    ];

    /// Actions the CLI parser emits as `"action": "<name>"`.
    fn cli_action_names() -> Vec<String> {
        let commands = include_str!("../commands.rs");
        let source = commands
            .split_once("#[cfg(test)]")
            .map_or(commands, |(code, _)| code);
        let mut names = Vec::new();
        let mut rest = source;
        while let Some((_, after)) = rest.split_once("\"action\"") {
            rest = after;
            let Some(after) = after.trim_start().strip_prefix(':') else {
                continue;
            };
            let Some(after) = after.trim_start().strip_prefix('"') else {
                continue;
            };
            if let Some((name, _)) = after.split_once('"') {
                names.push(name.to_string());
            }
        }
        names
    }

    /// Actions the daemon dispatch accepts.
    fn daemon_action_names() -> Vec<String> {
        let actions = include_str!("actions.rs");
        let dispatch = actions
            .split_once("async fn execute_command_inner")
            .and_then(|(_, rest)| rest.split_once("match action {"))
            .and_then(|(_, dispatch)| dispatch.split_once("_ => Err(format!(\"Not yet implemented"))
            .map(|(dispatch, _)| dispatch)
            .expect("the daemon action dispatch");
        let mut names = Vec::new();
        for line in dispatch.lines() {
            let Some((pattern, _)) = line.split_once("=>") else {
                continue;
            };
            // A pattern can name several actions: `"state_list" | "state_show"`.
            let mut parts = pattern.split('"');
            parts.next();
            while let Some(name) = parts.next() {
                // Skip the text between two quoted strings.
                parts.next();
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    names.push(name.to_string());
                }
            }
        }
        names
    }

    #[test]
    fn test_every_action_is_categorized_or_explicitly_uncategorized() {
        let categorized: Vec<&str> = ACTION_CATEGORIES
            .iter()
            .flat_map(|(_, actions)| actions.iter().copied())
            .collect();

        let mut sorted = categorized.clone();
        sorted.sort_unstable();
        let duplicates: Vec<&str> = sorted
            .windows(2)
            .filter(|pair| pair[0] == pair[1])
            .map(|pair| pair[0])
            .collect();
        assert!(
            duplicates.is_empty(),
            "actions mapped to more than one category: {duplicates:?}"
        );

        let mut names = cli_action_names();
        names.extend(daemon_action_names());
        names.sort_unstable();
        names.dedup();

        for name in &names {
            let in_category = categorized.contains(&name.as_str());
            let uncategorized = UNCATEGORIZED_ACTIONS.contains(&name.as_str());
            assert!(
                in_category ^ uncategorized,
                "'{name}' must be in a documented category or in UNCATEGORIZED_ACTIONS"
            );
        }
        for action in categorized.iter().chain(UNCATEGORIZED_ACTIONS) {
            assert!(
                names.iter().any(|name| name == action),
                "'{action}' is not an action the CLI or daemon accepts"
            );
        }
    }
}
