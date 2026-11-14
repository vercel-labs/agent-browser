//! Firefox permission grants: the names the session granted and the origins that hold them.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use serde_json::{json, Value};

use super::client::BidiClient;
use super::daemon::session;
use crate::native::actions::DaemonState;

const UNREACHABLE_ORIGIN: &str = "https://agent-browser.invalid";

/// Permissions the session granted and the origins that hold them.
#[derive(Debug, Default)]
pub struct Grants {
    names: BTreeSet<String>,
    held: HashSet<(String, String)>,
}

impl Grants {
    /// Records the permission names Firefox accepts and grants them for every origin in `urls`.
    pub async fn grant(
        &mut self,
        bidi: &BidiClient,
        names: &[String],
        urls: &[String],
    ) -> Result<(), String> {
        let mut fresh = BTreeSet::new();
        for name in names {
            if self.names.contains(name) || !fresh.insert(name.clone()) {
                continue;
            }
            bidi.send(
                "permissions.setPermission",
                set_permission(name, "prompt", UNREACHABLE_ORIGIN),
            )
            .await?;
        }
        self.names.extend(fresh);
        for url in urls {
            self.cover(bidi, url).await?;
        }
        Ok(())
    }

    /// Grants every recorded permission to the origin of `url` unless that origin already holds it.
    pub async fn cover(&mut self, bidi: &BidiClient, url: &str) -> Result<(), String> {
        let Some(origin) = grantable_origin(url) else {
            return Ok(());
        };
        let names: Vec<String> = self.names.iter().cloned().collect();
        for name in names {
            let pair = (origin.clone(), name);
            if self.held.contains(&pair) {
                continue;
            }
            bidi.send(
                "permissions.setPermission",
                set_permission(&pair.1, "granted", &origin),
            )
            .await?;
            self.held.insert(pair);
        }
        Ok(())
    }

    /// Takes back every grant and forgets the granted names, so a browser the session does not
    /// own keeps none of them.
    pub async fn revoke(&mut self, bidi: &BidiClient, timeout: Duration) {
        for (origin, name) in self.held.drain() {
            let _ = bidi
                .send_timeout(
                    "permissions.setPermission",
                    set_permission(&name, "prompt", &origin),
                    timeout,
                )
                .await;
        }
        self.names.clear();
    }
}

/// Grants the requested permissions to the open tabs and to every origin a tab loads later.
/// Requests are intercepted from here on, so a document request waits until its origin holds them.
pub async fn grant(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let names: Vec<String> = cmd
        .get("permissions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        return Ok(json!({ "granted": names }));
    }
    let ff = session(state)?;
    let tree = ff
        .bidi
        .send("browsingContext.getTree", json!({ "maxDepth": 0 }))
        .await?;
    let urls: Vec<String> = tree["contexts"]
        .as_array()
        .map(|contexts| {
            contexts
                .iter()
                .filter_map(|context| context["url"].as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    super::network::ensure_intercept(ff).await?;
    let mut grants = ff.permissions.lock().await;
    grants.grant(&ff.bidi, &names, &urls).await?;
    Ok(json!({ "granted": names }))
}

/// The origin that can hold a grant for `url`, or `None` when no page of it can hold one.
fn grantable_origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.scheme() {
        "http" | "https" => Some(parsed.origin().ascii_serialization()),
        "file" => Some("file://".to_string()),
        _ => None,
    }
}

/// The URL of the page or frame document a `network.beforeRequestSent` event announces, when it
/// is one. A frame of another origin holds its own grants, so it is covered like a page.
pub(crate) fn document_request_url(event: &Value) -> Option<&str> {
    if event["method"] != "network.beforeRequestSent" {
        return None;
    }
    let request = &event["params"]["request"];
    if !matches!(
        request["destination"].as_str(),
        Some("document" | "iframe" | "frame")
    ) {
        return None;
    }
    request["url"].as_str()
}

fn set_permission(name: &str, state: &str, origin: &str) -> Value {
    json!({
        "descriptor": { "name": name },
        "state": state,
        "origin": origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::bidi::network::spawn_dispatcher;
    use crate::native::bidi::session::tests::{
        fake_session, fake_session_with_rdp, fake_session_with_replies, FakeReply,
    };
    use std::time::Duration;

    fn set_permission_calls(messages: &[Value]) -> Vec<&Value> {
        messages
            .iter()
            .filter(|m| m["method"] == "permissions.setPermission")
            .collect()
    }

    fn state_with(ff: crate::native::bidi::session::FirefoxSession) -> DaemonState {
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
    }

    #[tokio::test]
    async fn a_blank_tab_records_the_grant_without_granting_an_origin() {
        let (ff, _, calls) = fake_session_with_replies(|v: &Value| {
            if v["method"] == "browsingContext.getTree" {
                return FakeReply::Result(
                    json!({"contexts": [{"context": "ctx-active", "url": "about:blank"}]}),
                );
            }
            if v["params"]["state"] == "granted" {
                return FakeReply::Error {
                    error: "invalid argument",
                    message: "about:blank holds no origin",
                };
            }
            FakeReply::Result(json!({}))
        })
        .await;
        let mut state = state_with(ff);

        let result = grant(&json!({"permissions": ["geolocation"]}), &mut state)
            .await
            .unwrap();

        assert_eq!(result, json!({"granted": ["geolocation"]}));
        assert_eq!(
            calls.lock().unwrap().clone(),
            vec![
                "browsingContext.getTree",
                "network.addIntercept",
                "permissions.setPermission"
            ]
        );
    }

    #[tokio::test]
    async fn the_open_tab_origin_receives_every_granted_name() {
        let (ff, _, _, messages, _) = fake_session_with_rdp().await;
        let mut state = state_with(ff);

        grant(
            &json!({"permissions": ["geolocation", "notifications"]}),
            &mut state,
        )
        .await
        .unwrap();

        let messages = messages.lock().unwrap().clone();
        let granted: Vec<&Value> = set_permission_calls(&messages)
            .into_iter()
            .filter(|m| m["params"]["state"] == "granted")
            .collect();
        assert_eq!(granted.len(), 2);
        for (message, name) in granted.iter().zip(["geolocation", "notifications"]) {
            assert_eq!(message["params"]["descriptor"]["name"], name);
            assert_eq!(message["params"]["origin"], "https://example.com");
        }
    }

    #[tokio::test]
    async fn a_name_firefox_rejects_leaves_the_session_without_grants() {
        let sent: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let log = sent.clone();
        let (ff, _, calls) = fake_session_with_replies(move |v: &Value| {
            log.lock().unwrap().push(v.clone());
            if v["method"] != "permissions.setPermission" {
                return FakeReply::Result(json!({}));
            }
            if v["params"]["descriptor"]["name"] == "geolocation" {
                FakeReply::Result(json!({}))
            } else {
                FakeReply::Error {
                    error: "invalid argument",
                    message: "Unsupported permission",
                }
            }
        })
        .await;
        let mut state = state_with(ff);

        let error = grant(
            &json!({"permissions": ["geolocation", "clipboard-read"]}),
            &mut state,
        )
        .await
        .unwrap_err();
        assert!(error.contains("Unsupported permission"), "{error}");

        let before = calls.lock().unwrap().len();
        let bidi = state.firefox.as_ref().unwrap().bidi.clone();
        let mut grants = state.firefox.as_ref().unwrap().permissions.lock().await;
        grants
            .cover(&bidi, "https://new.example/doc")
            .await
            .unwrap();
        assert_eq!(calls.lock().unwrap().len(), before);

        let sent = sent.lock().unwrap();
        assert_eq!(set_permission_calls(&sent).len(), 2);
        assert!(set_permission_calls(&sent)
            .iter()
            .all(|message| message["params"]["state"] == "prompt"));
    }

    fn document_event(id: &str, url: &str, destination: &str) -> Value {
        json!({
            "type": "event",
            "method": "network.beforeRequestSent",
            "params": {
                "isBlocked": true,
                "context": "ctx-active",
                "request": {
                    "request": id,
                    "url": url,
                    "method": "GET",
                    "headers": [],
                    "destination": destination,
                },
            },
        })
    }

    #[tokio::test]
    async fn the_dispatcher_grants_an_origin_before_it_releases_the_document_request() {
        let (ff, tx, calls) = fake_session().await;
        let bidi = ff.bidi.clone();
        let mut state = state_with(ff);
        grant(&json!({"permissions": ["geolocation"]}), &mut state)
            .await
            .unwrap();

        let ff = state.firefox.as_ref().unwrap();
        let _dispatcher = spawn_dispatcher(
            bidi.clone(),
            crate::native::bidi::network::NetContext::default(),
            bidi.subscribe_buffered(),
            ff.emulation.clone(),
            ff.permissions.clone(),
        );
        calls.lock().unwrap().clear();

        tx.send(document_event("d1", "https://new.example/page", "document"))
            .unwrap();
        tx.send(document_event("d2", "https://new.example/frame", "iframe"))
            .unwrap();
        tx.send(document_event(
            "d3",
            "https://new.example/asset.js",
            "script",
        ))
        .unwrap();
        tx.send(document_event(
            "d4",
            "https://new.example/again",
            "document",
        ))
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let seen = calls.lock().unwrap().clone();
        assert_eq!(seen[0], "permissions.setPermission", "{seen:?}");
        assert_eq!(
            seen.iter()
                .filter(|method| *method == "permissions.setPermission")
                .count(),
            1,
            "{seen:?}"
        );
        assert_eq!(
            seen.iter()
                .filter(|method| *method == "network.continueRequest")
                .count(),
            4,
            "{seen:?}"
        );
    }

    #[test]
    fn grantable_origin_covers_web_and_file_pages_only() {
        assert_eq!(
            grantable_origin("https://example.com/page").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            grantable_origin("http://example.com:8080/deep/path?q=1").as_deref(),
            Some("http://example.com:8080")
        );
        assert_eq!(
            grantable_origin("file:///tmp/a.html").as_deref(),
            Some("file://")
        );
        assert_eq!(grantable_origin("about:blank"), None);
        assert_eq!(grantable_origin("data:text/html,x"), None);
        assert_eq!(grantable_origin("not a url"), None);
    }
}
