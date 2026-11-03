//! The direct-HTTP tier: the saved request sent with the session's cookies.

use async_trait::async_trait;

use super::*;

/// A session route (`network route`) that matches the request answers it on
/// the HTTP tier too, so mocks apply to the whole session.
pub struct RoutedResponse {
    pub abort: bool,
    pub status: u16,
    pub body: String,
}

/// Tier 1: send the request directly, with cookies from the live session.
pub async fn run_http(
    client: &reqwest::Client,
    req: &PreparedRequest,
    cookie_header: Option<&str>,
    routed: Option<RoutedResponse>,
) -> TierResult {
    let started = Instant::now();
    if let Some(r) = routed {
        let outcome = if r.abort {
            Err(Failure::new(
                false,
                None,
                "request aborted by a session route".to_string(),
            ))
        } else {
            Ok((r.status, r.body))
        };
        return tier_result(req, Tier::Http, started, outcome);
    }
    let method = match reqwest::Method::from_bytes(req.method.as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            return tier_result(
                req,
                Tier::Http,
                started,
                Err(Failure::new(
                    true,
                    None,
                    format!("unsupported method {}", req.method),
                )),
            )
        }
    };
    let mut builder = client.request(method, &req.url);
    for (k, v) in &req.headers {
        builder = builder.header(k, v);
    }
    if let Some(cookie) = cookie_header.filter(|c| !c.is_empty()) {
        builder = builder.header("cookie", cookie);
    }
    // Browsers send Origin on every write; sites check it against CSRF.
    let has_origin = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("origin"));
    if is_write(&req.method, req.body.as_deref()) && !has_origin {
        if let Some(origin) = req
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("referer"))
            .and_then(|(_, v)| url::Url::parse(v).ok())
            .map(|u| u.origin().ascii_serialization())
        {
            builder = builder.header("origin", origin);
        }
    }
    if let Some(body) = &req.body {
        builder = builder.body(body.clone());
    }
    let mut retry_after = None;
    let outcome = match builder.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            if resp.status().is_redirection() {
                let to = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                return tier_result(
                    req,
                    Tier::Http,
                    started,
                    Err(Failure {
                        status: Some(status),
                        reason: format!("HTTP {}: the redirect to {} was not followed", status, to),
                        drift: false,
                        shape_changed: false,
                        kind: None,
                    }),
                );
            }
            resp.text().await.map(|t| (status, t)).map_err(|e| {
                Failure::new(true, Some(status), format!("could not read body: {}", e))
            })
        }
        // A write that left the machine may have been applied.
        Err(e) if req.write && !e.is_connect() => Err(unknown_outcome(format!(
            "the write was sent but no response came back: {}",
            e
        ))),
        Err(e) => Err(Failure::new(false, None, format!("request failed: {}", e))),
    };
    with_retry_after(tier_result(req, Tier::Http, started, outcome), retry_after)
}

/// The direct-HTTP client. Redirects are followed only to hosts the
/// session's domain filter allows, the same rule browser requests follow.
pub fn http_client(
    filter: Option<crate::native::network::DomainFilter>,
    flow: &Flow,
) -> Result<reqwest::Client, String> {
    let flow = flow.clone();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        let blocked = check_allowed_host(&flow, attempt.url().as_str())
            .err()
            .or_else(|| {
                filter
                    .as_ref()
                    .and_then(|f| f.check_url(attempt.url().as_str()).err())
            });
        match blocked {
            // The 3xx comes back as the response, so a write that was sent
            // counts as answered.
            Some(_) => attempt.stop(),
            // reqwest's default redirect limit.
            None if attempt.previous().len() >= 10 => attempt.stop(),
            None => attempt.follow(),
        }
    });
    reqwest::Client::builder()
        .redirect(policy)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))
}

pub fn cookie_header(cookies: &[crate::native::cookies::Cookie]) -> String {
    cookies
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The HTTP tier over a browser session's cookies and routes.
pub struct HttpSender<'a> {
    pub client: &'a reqwest::Client,
    pub host: &'a mut dyn Host,
}

#[allow(clippy::double_must_use)]
#[async_trait]
impl Sender for HttpSender<'_> {
    async fn send(&mut self, req: &PreparedRequest, _ctx: &SendCtx<'_>) -> TierResult {
        let cookie = self.host.cookie_header(&req.url).await;
        let routed = self.host.routed_response(&req.url).await;
        run_http(self.client, req, cookie.as_deref(), routed).await
    }
}
