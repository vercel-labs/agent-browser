//! The in-page tier: the same request sent by the active tab's own fetch.

use async_trait::async_trait;

use super::*;

/// Tier 2: the page sends the request itself, so the browser adds its own
/// cookies, client hints and any headers the site's scripts would not.
pub fn page_fetch_script(req: &PreparedRequest) -> String {
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
    // A write must not follow a redirect to another site; the 3xx counts as
    // the site's answer.
    if req.write {
        init["redirect"] = json!("manual");
    }
    // Cross-origin APIs differ on credentials: some need cookies, some reject
    // a credentialed request outright. Try with, then without.
    // A write is never sent a second time: a thrown fetch may still have
    // reached the server, so it is reported as an unknown outcome instead.
    let send = if req.write {
        "const r = await fetch(u, { ...i, credentials: 'include' });"
    } else {
        "let r; try { r = await fetch(u, { ...i, credentials: 'include' }); } \
         catch (e) { r = await fetch(u, i); }"
    };
    format!(
        "(async () => {{ const u = {url}, i = {init}; \
         {send} \
         return {{ status: r.type === 'opaqueredirect' ? 302 : r.status, retryAfter: r.headers.get('retry-after'), text: await r.text() }}; }})()",
        url = json!(req.url),
        init = init,
        send = send
    )
}

pub fn page_result(
    req: &PreparedRequest,
    started: Instant,
    value: Result<Value, String>,
) -> TierResult {
    let retry_after = value
        .as_ref()
        .ok()
        .and_then(|v| v["retryAfter"].as_str())
        .map(String::from);
    let outcome = match value {
        Ok(v) => match (v["status"].as_u64(), v["text"].as_str()) {
            (Some(status), Some(text)) => Ok((status as u16, text.to_string())),
            _ => Err(Failure::new(
                true,
                None,
                "in-page fetch returned no response".to_string(),
            )),
        },
        Err(e) if req.write => Err(Failure::unknown_outcome(format!(
            "the in-page write may have been sent: {}",
            e
        ))),
        Err(e) => Err(Failure::new(
            true,
            None,
            format!("in-page fetch failed: {}", e),
        )),
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
pub struct PageSender<'a> {
    pub flow: &'a Flow,
    pub host: &'a mut dyn Host,
}

#[allow(clippy::double_must_use)]
#[async_trait]
impl Sender for PageSender<'_> {
    async fn send(&mut self, req: &PreparedRequest, _ctx: &SendCtx<'_>) -> TierResult {
        let started = Instant::now();
        let tab = self.host.tab_url().await;
        if let Err(reason) = tab_on_site(self.flow, tab.as_deref()) {
            return TierResult::fail(req, Tier::Page, started, Failure::drift(reason));
        }
        let value = self.host.eval(&page_fetch_script(req)).await;
        page_result(req, started, value)
    }
}
