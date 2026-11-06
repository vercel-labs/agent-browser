//! Sending a saved request directly, with the session's cookies.

use std::sync::Arc;

use super::net::{
    client_proxies, env_proxies, refusal_in, resolve_proxy_hosts, Reach, ReachResolver,
};
use super::*;

/// How long a request, or the lookup of a proxy's address, may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A session route (`network route`) that matches the request answers it,
/// so mocks apply to a run too.
pub struct RoutedResponse {
    pub abort: bool,
    pub status: u16,
    pub body: String,
}

/// Send the request directly, with cookies from the live session.
pub async fn run_http(
    clients: &HttpClients,
    req: &PreparedRequest,
    cookie_header: Option<&str>,
    routed: Option<RoutedResponse>,
) -> RequestResult {
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
        return request_result(req, started, outcome);
    }
    let method = match reqwest::Method::from_bytes(req.method.as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            return request_result(
                req,
                started,
                Err(Failure::new(
                    true,
                    None,
                    format!("unsupported method {}", req.method),
                )),
            )
        }
    };
    let mut builder = clients.for_request(req).request(method, &req.url);
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
                return request_result(
                    req,
                    started,
                    Err(Failure {
                        status: Some(status),
                        reason: format!("HTTP {}: the redirect to {} was not followed", status, to),
                        drift: false,
                        shape_changed: false,
                        stop: None,
                    }),
                );
            }
            resp.text().await.map(|t| (status, t)).map_err(|e| {
                Failure::new(true, Some(status), format!("could not read body: {}", e))
            })
        }
        Err(e) => match refusal_in(&e) {
            // The resolver refused the destination before any connection.
            Some(reason) => Err(Failure::new(false, None, reason)),
            // A write that left the machine may have been applied.
            None if req.write && !e.is_connect() => Err(Failure::unknown_outcome(format!(
                "the write was sent but no response came back: {}",
                e
            ))),
            None => Err(Failure::new(false, None, format!("request failed: {}", e))),
        },
    };
    with_retry_after(request_result(req, started, outcome), retry_after)
}

/// The clients of a run: reads follow same-site redirects, writes never
/// follow any.
pub struct HttpClients {
    reads: reqwest::Client,
    writes: reqwest::Client,
}

impl HttpClients {
    fn for_request(&self, req: &PreparedRequest) -> &reqwest::Client {
        if req.write || is_write(&req.method, req.body.as_deref()) {
            &self.writes
        } else {
            &self.reads
        }
    }
}

pub async fn http_client(
    filter: Option<crate::native::network::DomainFilter>,
    flow: &Flow,
) -> Result<HttpClients, String> {
    let env = |k: &str| std::env::var(k).ok();
    let env_proxies = env_proxies(env)?;
    let addresses = resolve_proxy_hosts(&env_proxies, REQUEST_TIMEOUT).await;
    let (proxies, trusted) =
        client_proxies(
            env,
            reqwest::NoProxy::from_env(),
            |host, port| match addresses.get(&(host.to_string(), port)) {
                Some(found) => found.clone(),
                None => Err(format!("could not resolve proxy {host}")),
            },
        )?;
    let mut resolver = ReachResolver::system(Reach::of_start(&flow.start_url));
    resolver.trusted = trusted;
    http_client_with(filter, flow, Arc::new(resolver), proxies)
}

/// `http_client` with the resolver and proxies given. The environment's
/// proxies are read by `http_client` only, so a test's client has none
/// unless it passes them.
pub(crate) fn http_client_with<R: reqwest::dns::Resolve + 'static>(
    filter: Option<crate::native::network::DomainFilter>,
    flow: &Flow,
    resolver: Arc<R>,
    proxies: Vec<reqwest::Proxy>,
) -> Result<HttpClients, String> {
    let build = |policy| {
        let mut builder = reqwest::Client::builder().no_proxy();
        for p in &proxies {
            builder = builder.proxy(p.clone());
        }
        builder
            .redirect(policy)
            .dns_resolver(resolver.clone())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))
    };
    Ok(HttpClients {
        reads: build(read_redirects(filter, flow))?,
        // The 3xx comes back as the answer, so a write that was sent is
        // not sent again by a 307 or 308.
        writes: build(reqwest::redirect::Policy::none())?,
    })
}

/// Reads follow redirects only to hosts the session's domain filter allows,
/// the same rule browser requests follow.
fn read_redirects(
    filter: Option<crate::native::network::DomainFilter>,
    flow: &Flow,
) -> reqwest::redirect::Policy {
    let hosts = HostPolicy::of(flow);
    reqwest::redirect::Policy::custom(move |attempt| {
        let blocked = hosts.check(attempt.url().as_str()).err().or_else(|| {
            filter
                .as_ref()
                .and_then(|f| f.check_url(attempt.url().as_str()).err())
        });
        match blocked {
            Some(_) => attempt.stop(),
            // reqwest's default redirect limit.
            None if attempt.previous().len() >= 10 => attempt.stop(),
            None => attempt.follow(),
        }
    })
}

pub fn cookie_header(cookies: &[crate::native::cookies::Cookie]) -> String {
    cookies
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}
