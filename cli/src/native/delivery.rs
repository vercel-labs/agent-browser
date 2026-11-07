//! Delivery reporting for page-changing actions: what an action observably did, and whether repeating it is safe.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::cdp::client::CdpClient;
use super::element::{resolve_element_object_id, RefMap};

/// Actions whose results carry a `disposition` and an `effect`.
pub const DELIVERY_ACTIONS: &[&str] = &[
    "click", "dblclick", "fill", "type", "press", "select", "check", "uncheck",
];

const POLL: Duration = Duration::from_millis(50);
const QUIET: Duration = Duration::from_millis(150);
const FIRST_EFFECT: Duration = Duration::from_millis(600);
const SETTLE_MAX: Duration = Duration::from_millis(1500);
const LOAD_MAX: Duration = Duration::from_millis(5000);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

const ARM: &str = r#"(() => {
  const prev = window.__abDelivery;
  if (prev && prev.mo) prev.mo.disconnect();
  const o = { t: Math.random().toString(36).slice(2), n: 0 };
  o.mo = new MutationObserver((r) => { o.n += r.length; });
  o.mo.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
  addEventListener('beforeunload', () => { o.leaving = true; }, { once: true });
  window.__abDelivery = o;
  return [location.href, document.title, o.t];
})()"#;

const PROBE: &str = r#"(() => {
  const o = window.__abDelivery;
  return [location.href, document.title, document.readyState, o ? o.t : null, o ? o.n : 0, !!(o && o.leaving)];
})()"#;

const TARGET_STATE: &str = r#"function() {
  const s = {};
  if (this.type !== 'password' && 'value' in this) s.value = String(this.value);
  if (this.type === 'password') s.length = String(this.value || '').length;
  if ('checked' in this && (this.type === 'checkbox' || this.type === 'radio')) s.checked = this.checked;
  const aria = this.getAttribute && this.getAttribute('aria-checked');
  if (s.checked === undefined && aria !== null) s.checked = aria === 'true';
  if (this.tagName === 'SELECT') {
    s.selected = Array.from(this.selectedOptions).map((o) => o.value);
    s.selectedLabels = Array.from(this.selectedOptions).map((o) => o.label.trim());
  }
  return s;
}"#;

/// Page state captured right before an action dispatches.
pub struct Before {
    url: String,
    title: String,
    token: String,
}

/// Arm a mutation counter in the page and record its URL and title. Returns `None` when the page cannot be read, in which case no disposition is reported.
pub async fn arm(client: &CdpClient, session_id: &str) -> Option<Before> {
    let v = eval(client, session_id, ARM).await?;
    Some(Before {
        url: v.get(0)?.as_str()?.to_string(),
        title: v.get(1)?.as_str()?.to_string(),
        token: v.get(2)?.as_str()?.to_string(),
    })
}

/// Wait for the page to settle after an action, then add `disposition` and `effect` to a successful result. For a failed action, returns them for the error response only when the page changed afterwards, since the action may then have landed.
#[allow(clippy::too_many_arguments)]
pub async fn report(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    iframe_sessions: &HashMap<String, String>,
    action: &str,
    cmd: &Value,
    before: Before,
    result: &mut Result<Value, String>,
) -> Option<Value> {
    let opened_elsewhere = result.as_ref().is_ok_and(|data| {
        data.get("dialogOpened").and_then(Value::as_bool) == Some(true)
            || data.get("newTab").and_then(Value::as_bool) == Some(true)
    });

    let mut effect = serde_json::Map::new();
    if !opened_elsewhere {
        let first_effect = if result.is_ok() && matches!(action, "click" | "dblclick" | "press") {
            FIRST_EFFECT
        } else {
            QUIET
        };
        settle(client, session_id, &before, first_effect, &mut effect).await;
    }

    let mut verified =
        opened_elsewhere || effect.contains_key("navigated") || effect.contains_key("url");
    let mut mismatch = false;
    if result.is_ok() && !effect.contains_key("navigated") {
        if let Some(state) =
            target_state(client, session_id, ref_map, iframe_sessions, action, cmd).await
        {
            match expected_matches(action, cmd, &state) {
                Some(true) => verified = true,
                Some(false) => mismatch = true,
                None => {}
            }
            effect.insert("postState".into(), state);
        }
    }
    let mutations = effect.get("mutations").and_then(Value::as_u64).unwrap_or(0);
    if mutations > 0 && !mismatch {
        verified = true;
    }

    let changed = verified
        || mutations > 0
        || effect.contains_key("unreadable")
        || effect.contains_key("navigationPending");
    let disposition = match result {
        Ok(_) => {
            let mut d = json!({
                "delivery": if verified { "delivered_verified" } else { "delivered_unverified" },
                "retry": "unsafe",
            });
            if mismatch {
                d["reason"] = json!("postcondition_mismatch");
            }
            d
        }
        Err(_) if changed => json!({ "delivery": "delivered_unverified", "retry": "unsafe" }),
        Err(_) => return None,
    };

    let effect = Value::Object(effect);
    if let Ok(Value::Object(data)) = result {
        data.insert("disposition".into(), disposition);
        data.insert("effect".into(), effect);
        return None;
    }
    Some(json!({ "disposition": disposition, "effect": effect }))
}

async fn settle(
    client: &CdpClient,
    session_id: &str,
    before: &Before,
    first_effect: Duration,
    effect: &mut serde_json::Map<String, Value>,
) {
    let start = Instant::now();
    let mut last_count = 0u64;
    let mut last_change = start;
    let mut navigated = false;
    let mut leaving = false;
    let mut unreadable;
    let mut probe = None;
    loop {
        tokio::time::sleep(POLL).await;
        let now = Instant::now();
        match eval(client, session_id, PROBE).await {
            Some(v) => {
                let same_doc = v.get(3).and_then(Value::as_str) == Some(before.token.as_str());
                let count = v.get(4).and_then(Value::as_u64).unwrap_or(0);
                let ready = v.get(2).and_then(Value::as_str) == Some("complete");
                leaving = same_doc && v.get(5).and_then(Value::as_bool) == Some(true);
                if !same_doc {
                    navigated = true;
                }
                if same_doc && count != last_count {
                    last_count = count;
                    last_change = now;
                }
                probe = Some(v);
                unreadable = false;
                if navigated && ready {
                    break;
                }
                let waited = if last_count == 0 { first_effect } else { QUIET };
                if !navigated && !leaving && now.duration_since(last_change) >= waited {
                    break;
                }
            }
            None => unreadable = true,
        }
        let cap = if navigated || leaving || unreadable {
            LOAD_MAX
        } else {
            SETTLE_MAX
        };
        if now.duration_since(start) >= cap {
            break;
        }
    }

    let url = probe
        .as_ref()
        .and_then(|v| v.get(0))
        .and_then(Value::as_str);
    let title = probe
        .as_ref()
        .and_then(|v| v.get(1))
        .and_then(Value::as_str);
    if navigated {
        effect.insert("navigated".into(), json!(true));
        if let Some(ready) = probe
            .as_ref()
            .and_then(|v| v.get(2))
            .and_then(Value::as_str)
        {
            effect.insert("readyState".into(), json!(ready));
        }
    }
    if let Some(url) = url.filter(|u| *u != before.url) {
        effect.insert("url".into(), json!(url));
    }
    if let Some(title) = title.filter(|t| *t != before.title) {
        effect.insert("title".into(), json!(title));
    }
    if leaving {
        effect.insert("navigationPending".into(), json!(true));
    }
    if unreadable {
        effect.insert("unreadable".into(), json!(true));
    } else if !navigated {
        effect.insert("mutations".into(), json!(last_count));
    }
}

async fn target_state(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    iframe_sessions: &HashMap<String, String>,
    action: &str,
    cmd: &Value,
) -> Option<Value> {
    if !matches!(action, "fill" | "type" | "select" | "check" | "uncheck") {
        return None;
    }
    let selector = cmd.get("selector")?.as_str()?;
    let (object_id, effective) =
        resolve_element_object_id(client, session_id, ref_map, selector, iframe_sessions)
            .await
            .ok()?;
    let call = client.send_command(
        "Runtime.callFunctionOn",
        Some(json!({ "objectId": object_id, "functionDeclaration": TARGET_STATE, "returnByValue": true })),
        Some(&effective),
    );
    let v = tokio::time::timeout(PROBE_TIMEOUT, call).await.ok()?.ok()?;
    v.get("result")?
        .get("value")
        .cloned()
        .filter(|s| s.as_object().is_some_and(|m| !m.is_empty()))
}

fn expected_matches(action: &str, cmd: &Value, state: &Value) -> Option<bool> {
    match action {
        "fill" => {
            let want = cmd.get("value")?.as_str()?;
            match state.get("value").and_then(Value::as_str) {
                Some(v) => Some(v == want),
                None => Some(state.get("length")?.as_u64()? == want.chars().count() as u64),
            }
        }
        "type" => {
            let typed = cmd.get("text")?.as_str()?;
            Some(state.get("value")?.as_str()?.contains(typed))
        }
        "check" => Some(state.get("checked")?.as_bool()?),
        "uncheck" => Some(!state.get("checked")?.as_bool()?),
        "select" => {
            let mut got: Vec<&str> = Vec::new();
            for key in ["selected", "selectedLabels"] {
                got.extend(state.get(key)?.as_array()?.iter().filter_map(Value::as_str));
            }
            let wanted: Vec<&str> = match cmd.get("values").or_else(|| cmd.get("value"))? {
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                Value::String(s) => vec![s.as_str()],
                _ => return None,
            };
            Some(!wanted.is_empty() && wanted.iter().all(|w| got.contains(&w.trim())))
        }
        _ => None,
    }
}

async fn eval(client: &CdpClient, session_id: &str, expression: &str) -> Option<Value> {
    let call = client.send_command(
        "Runtime.evaluate",
        Some(json!({ "expression": expression, "returnByValue": true })),
        Some(session_id),
    );
    let v = tokio::time::timeout(PROBE_TIMEOUT, call).await.ok()?.ok()?;
    if v.get("exceptionDetails").is_some() {
        return None;
    }
    v.get("result")?.get("value").cloned()
}
