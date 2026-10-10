//! Identify which browser profile each tab belongs to.
//!
//! When agent-browser attaches to a running Chrome or Brave over CDP, the tabs
//! of every open profile show up in one target list. CDP only tags each
//! target with an opaque `browserContextId`: `Target.getBrowserContexts` does
//! not list the contexts of user profiles, and `Target.createTarget` and
//! `Storage.getCookies` reject their ids. A page can still open a tab in its
//! own profile, though, and `chrome://version` reports the profile path of
//! the tab that shows it. So a context's profile is read by opening a
//! throwaway tab from a page in that context, loading `chrome://version` in
//! it, reading the profile path, and closing the tab again.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::cdp::chrome::list_chrome_profiles;
use super::cdp::client::CdpClient;
use super::cdp::types::{AttachToTargetParams, AttachToTargetResult, GetTargetsResult};

/// Upper bound for identifying one context, including the throwaway tab.
const PROBE_DEADLINE: Duration = Duration::from_secs(8);
/// How long to wait for the throwaway tab to appear, and then for
/// `chrome://version` to render the profile path.
const STEP_DEADLINE: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A browser profile as reported by the browser itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIdentity {
    /// Display name from the browser's `Local State` (e.g. "Work"), or the
    /// directory name when `Local State` is not readable from this machine.
    pub name: String,
    /// Profile directory name (e.g. "Default", "Profile 1").
    pub directory: String,
    /// Full profile path as reported by `chrome://version`.
    pub path: String,
}

impl ProfileIdentity {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "directory": self.directory,
            "path": self.path,
        })
    }
}

/// Builds a [`ProfileIdentity`] from the profile path shown by
/// `chrome://version`, looking up the display name in the `Local State` file
/// next to the profile directory. For a remote browser that file does not
/// exist locally, so the directory name doubles as the display name.
pub fn identity_from_profile_path(path: &str) -> ProfileIdentity {
    let path = path.trim();
    let profile_dir = Path::new(path);
    let directory = profile_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let name = profile_dir
        .parent()
        .and_then(|user_data_dir| {
            list_chrome_profiles(user_data_dir)
                .into_iter()
                .find(|p| p.directory == directory)
        })
        .map(|p| p.name)
        .unwrap_or_else(|| directory.clone());
    ProfileIdentity {
        name,
        directory,
        path: path.to_string(),
    }
}

/// Maps every target id to its `browserContextId`. Returns `None` when the
/// connection cannot list targets (e.g. a CDP socket scoped to one page).
pub async fn target_contexts(client: &CdpClient) -> Option<HashMap<String, String>> {
    let result: GetTargetsResult = client
        .send_command_typed("Target.getTargets", &json!({}), None)
        .await
        .ok()?;
    Some(
        result
            .target_infos
            .into_iter()
            .filter_map(|t| t.browser_context_id.map(|ctx| (t.target_id, ctx)))
            .collect(),
    )
}

/// Identifies the profile of `context_id` by opening a throwaway tab from the
/// page attached as `opener_session_id` (which must live in that context).
///
/// Every throwaway tab id is added to `probe_targets` as soon as it is known,
/// so the daemon's event drain can ignore it instead of adopting it as a new
/// tab. The tab is closed before returning, whether or not the probe worked.
pub async fn probe_context_profile(
    client: &CdpClient,
    opener_session_id: &str,
    context_id: &str,
    probe_targets: &mut HashSet<String>,
) -> Result<ProfileIdentity, String> {
    match tokio::time::timeout(
        PROBE_DEADLINE,
        probe_inner(client, opener_session_id, context_id, probe_targets),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!(
            "Timed out identifying the profile of browser context {}",
            context_id
        )),
    }
}

async fn probe_inner(
    client: &CdpClient,
    opener_session_id: &str,
    context_id: &str,
    probe_targets: &mut HashSet<String>,
) -> Result<ProfileIdentity, String> {
    let before: HashSet<String> = target_contexts(client)
        .await
        .ok_or("Target.getTargets is not available on this connection")?
        .into_keys()
        .collect();

    // `noopener` keeps the throwaway tab from scripting (or being scripted
    // by) the user's page; `userGesture` gets it past the popup blocker.
    client
        .send_command(
            "Runtime.evaluate",
            Some(json!({
                "expression": "void window.open('about:blank', '_blank', 'noopener')",
                "userGesture": true,
            })),
            Some(opener_session_id),
        )
        .await?;

    let probe_target = wait_for_new_page(client, context_id, &before).await?;
    probe_targets.insert(probe_target.clone());

    let result = read_profile_path(client, &probe_target).await;
    let _ = client
        .send_command(
            "Target.closeTarget",
            Some(json!({ "targetId": probe_target })),
            None,
        )
        .await;
    result.map(|path| identity_from_profile_path(&path))
}

async fn wait_for_new_page(
    client: &CdpClient,
    context_id: &str,
    before: &HashSet<String>,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    loop {
        let targets: GetTargetsResult = client
            .send_command_typed("Target.getTargets", &json!({}), None)
            .await?;
        if let Some(t) = targets.target_infos.into_iter().find(|t| {
            t.target_type == "page"
                && t.browser_context_id.as_deref() == Some(context_id)
                && !before.contains(&t.target_id)
        }) {
            return Ok(t.target_id);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("The page did not open a tab (popup blocked?)".to_string());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn read_profile_path(client: &CdpClient, target_id: &str) -> Result<String, String> {
    let attach: AttachToTargetResult = client
        .send_command_typed(
            "Target.attachToTarget",
            &AttachToTargetParams {
                target_id: target_id.to_string(),
                flatten: true,
            },
            None,
        )
        .await?;
    let sid = attach.session_id.as_str();
    let _ = client
        .send_command("Runtime.runIfWaitingForDebugger", None, Some(sid))
        .await;
    client
        .send_command(
            "Page.navigate",
            Some(json!({ "url": "chrome://version" })),
            Some(sid),
        )
        .await?;

    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    loop {
        let eval = client
            .send_command(
                "Runtime.evaluate",
                Some(json!({
                    "expression": "(document.querySelector('#profile_path') || {}).textContent || ''",
                    "returnByValue": true,
                })),
                Some(sid),
            )
            .await;
        if let Some(path) = eval
            .ok()
            .as_ref()
            .and_then(|v| v.pointer("/result/value"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(path.to_string());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("chrome://version did not report a profile path".to_string());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Orders candidate opener pages for a context: regular web pages first,
/// since internal pages and blank tabs are more likely to refuse
/// `window.open` or to be the user's freshly opened, still-loading tab.
pub fn opener_rank(url: &str) -> u8 {
    if url.starts_with("https://") || url.starts_with("http://") {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_uses_local_state_display_name() {
        let dir = std::env::temp_dir().join(format!("ab-profiles-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Profile 1")).unwrap();
        std::fs::write(
            dir.join("Local State"),
            r#"{"profile":{"info_cache":{"Default":{"name":"Personal"},"Profile 1":{"name":"Work"}}}}"#,
        )
        .unwrap();
        let path = dir.join("Profile 1");
        let identity = identity_from_profile_path(&format!(" {} \n", path.display()));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(identity.name, "Work");
        assert_eq!(identity.directory, "Profile 1");
        assert_eq!(identity.path, path.display().to_string());
    }

    #[test]
    fn identity_falls_back_to_directory_name() {
        let identity = identity_from_profile_path("/nonexistent/ab-test/User Data/Profile 7");
        assert_eq!(identity.name, "Profile 7");
        assert_eq!(identity.directory, "Profile 7");
    }

    #[test]
    fn opener_rank_prefers_web_pages() {
        assert!(opener_rank("https://example.com") < opener_rank("chrome://newtab/"));
        assert!(opener_rank("http://localhost:3000") < opener_rank("about:blank"));
    }
}
