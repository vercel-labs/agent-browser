//! The navigation hint: after a page loads, the saved flows for its site.

use super::*;

/// Whether a response to `action` may carry the hint.
pub fn wants_hint(action: &str, resp: &Value) -> bool {
    matches!(
        action,
        "navigate" | "back" | "forward" | "reload" | "tab_switch"
    ) && resp.get("success") == Some(&json!(true))
}

/// List the saved flows for the page's site, once per site, so the agent can
/// call them instead of driving the page. Flows saved in this session are
/// not listed.
pub fn attach_availability(
    resp: &mut Value,
    tab_url: Option<&str>,
    hinted: &mut Option<String>,
    saved_here: &[(String, String)],
) {
    let Some(host) = tab_url
        .and_then(|u| url::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(String::from))
    else {
        return;
    };
    let site = base_domain(&host);
    if hinted.as_deref() == Some(site.as_str()) {
        return;
    }
    *hinted = Some(site);
    let mut flows = list_flows(Some(&host));
    flows.retain(|f| {
        !saved_here
            .iter()
            .any(|(o, n)| f["origin"] == o.as_str() && f["name"] == n.as_str())
    });
    if flows.is_empty() {
        return;
    }
    if let Some(data) = resp
        .as_object_mut()
        .map(|r| r.entry("data").or_insert_with(|| json!({})))
        .and_then(|d| d.as_object_mut())
    {
        data.insert("flows".to_string(), json!(flows));
    }
}
