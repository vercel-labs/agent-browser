//! The in-page tier: the same request sent by the active tab's own fetch.

use super::*;

const PAGE_FETCH_WRITE: &str =
    r#"const r = await fetch(u, { ...i, credentials: 'include', signal: controller.signal });"#;
const PAGE_FETCH_READ: &str = r#"let r;
try {
    r = await fetch(u, { ...i, credentials: 'include', signal: controller.signal });
} catch (e) {
    if (controller.signal.aborted) throw e;
    r = await fetch(u, { ...i, signal: controller.signal });
}"#;

/// Tier 2: the page sends the request itself, so the browser adds its own
/// cookies, client hints and any headers the site's scripts would not.
pub fn page_fetch_script(req: &PreparedRequest, timeout: Duration) -> String {
    // Headers the browser sets itself. Sending them as app headers turns the
    // request into a CORS preflight the site never allowed.
    let headers: Map<String, Value> = req
        .headers
        .iter()
        .filter(|(k, _)| classify_header(k) != HeaderClass::BrowserSet)
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let mut init = json!({
        "method": req.method,
        "headers": headers,
        "body": req.body,
    });
    // No request follows a redirect: the browser would send the next hop
    // before the site boundary could be checked. A write's 3xx counts as the
    // site's answer; a read moves on to the next tier.
    init["redirect"] = json!("manual");
    // Cross-origin APIs differ on credentials: some need cookies, some reject
    // a credentialed request outright. Try with, then without.
    // A write is never sent a second time: a thrown fetch may still have
    // reached the server, so it is reported as an unknown outcome instead.
    let send = if req.write {
        PAGE_FETCH_WRITE
    } else {
        PAGE_FETCH_READ
    };
    // Dropping the CDP evaluation future does not cancel fetch in the page.
    // Keep its timer alive through response-body consumption as well.
    format!(
        r#"(async () => {{
    const u = {url}, i = {init};
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(new Error("the run's time is up")), {timeout_ms});
    try {{
        {send}
        return {{ status: r.type === 'opaqueredirect' ? 302 : r.status, redirected: r.type === 'opaqueredirect', url: r.url, retryAfter: r.headers.get('retry-after'), text: await r.text() }};
    }} finally {{
        clearTimeout(timer);
    }}
}})()"#,
        url = json!(req.url),
        init = init,
        send = send,
        // Browsers store timer delays as signed 32-bit milliseconds.
        timeout_ms = timeout.as_millis().min(i32::MAX as u128),
    )
}

pub fn page_result(
    policy: &HostPolicy,
    req: &PreparedRequest,
    started: Instant,
    value: Result<Value, String>,
) -> TierResult {
    let retry_after = value
        .as_ref()
        .ok()
        .and_then(|v| v["retryAfter"].as_str())
        .map(String::from);
    // A read follows redirects; the page it ended on must be on the site, as
    // in the HTTP tier. An opaque redirect has no URL.
    let landed = value
        .as_ref()
        .ok()
        .and_then(|v| v["url"].as_str())
        .filter(|u| !u.is_empty())
        .and_then(|u| policy.check(u).err().map(|_| u.to_string()));
    let outcome = match value {
        // The page tier does not follow redirects, so the hop was never
        // sent; the next tier can still answer the read.
        Ok(v) if !req.write && v["redirected"] == json!(true) => Err(Failure::skipped(
            "the in-page read was answered with a redirect, which the page tier does not follow"
                .to_string(),
        )),
        Ok(v) if landed.is_some() => {
            let to = landed.unwrap_or_default();
            if req.write {
                Err(Failure::unknown_outcome(format!(
                    "the in-page write was sent and ended at {}, outside the flow's site",
                    to
                )))
            } else {
                Err(Failure::new(
                    false,
                    v["status"].as_u64().map(|s| s as u16),
                    format!("the redirect to {} was not followed", to),
                ))
            }
        }
        Ok(v) => match (v["status"].as_u64(), v["text"].as_str()) {
            (Some(status), Some(text)) => Ok((status as u16, text.to_string())),
            _ if req.write => Err(Failure::unknown_outcome(
                "the in-page write returned no response and may have been sent".to_string(),
            )),
            _ => Err(Failure::transport(
                "in-page fetch returned no response".to_string(),
            )),
        },
        Err(e) if req.write => Err(Failure::unknown_outcome(format!(
            "the in-page write may have been sent: {}",
            e
        ))),
        Err(e) => Err(Failure::transport(format!("in-page fetch failed: {}", e))),
    };
    with_retry_after(tier_result(req, Tier::Page, started, outcome), retry_after)
}

/// The in-page tier needs a tab that is already on the flow's site.
pub fn tab_on_site(flow: &Flow, current: Option<&str>) -> Result<(), String> {
    let policy = HostPolicy::of(flow);
    let site = policy.hosts().join(", ");
    match current {
        Some(url) if policy.check(url).is_ok() => Ok(()),
        Some(url) => Err(format!(
            "the active tab ({}) is not on the flow's site ({}); open the site to use the in-page request",
            url, site
        )),
        None => Err(format!(
            "no browser is open; open the flow's site ({}) to use the in-page request",
            site
        )),
    }
}

/// The in-page tier over the active tab.
pub async fn send(
    flow: &Flow,
    browser: &mut dyn Browser,
    req: &PreparedRequest,
    deadline: Instant,
) -> TierResult {
    let started = Instant::now();
    let left = deadline.saturating_duration_since(Instant::now());
    let Ok(tab) = tokio::time::timeout(left, browser.tab_url()).await else {
        return TierResult::fail(
            req,
            Tier::Page,
            started,
            Failure::unavailable("the run's time is up".to_string()),
        );
    };
    if let Err(reason) = tab_on_site(flow, tab.as_deref()) {
        return TierResult::fail(req, Tier::Page, started, Failure::skipped(reason));
    }
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return TierResult::fail(
            req,
            Tier::Page,
            started,
            Failure::unavailable("the run's time is up".to_string()),
        );
    }
    let value = match tokio::time::timeout(left, browser.eval(&page_fetch_script(req, left))).await
    {
        Ok(value) => value,
        Err(_) => Err("the run's time is up".to_string()),
    };
    page_result(&HostPolicy::of(flow), req, started, value)
}
