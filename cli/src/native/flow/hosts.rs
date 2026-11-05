//! Which hosts a flow may reach: the one place a URL is checked.

use super::net::{check_literal, Reach};
use super::*;

/// Registrable part of a host, close enough for first-party checks:
/// `api.example.com.ar` -> `example.com.ar`, `www.example.org` -> `example.org`.
pub fn base_domain(host: &str) -> String {
    let host = host.trim_end_matches('.');
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return host.to_string();
    }
    // The public suffix list keeps `a.vercel.app` and `b.vercel.app` apart.
    psl::domain_str(&host.to_ascii_lowercase())
        .map(String::from)
        .unwrap_or_else(|| host.to_ascii_lowercase())
}

pub(crate) fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(String::from)
}

/// A flow origin becomes a directory name, so it must be a plain hostname.
pub fn validate_origin(origin: &str) -> Result<(), String> {
    let ok = !origin.is_empty()
        && origin.len() <= 253
        && !origin.starts_with('.')
        && !origin.ends_with('.')
        && !origin.contains("..")
        && origin
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid flow origin '{}': expected a hostname",
            origin
        ))
    }
}

fn host_matches(host: &str, allowed: &str) -> bool {
    if allowed.parse::<std::net::IpAddr>().is_ok() {
        return host == allowed;
    }
    host == allowed || host.ends_with(&format!(".{}", allowed))
}

/// The hosts a flow may reach: the base domain of its start URL, which a
/// flow file can only narrow, never widen.
#[derive(Clone, Debug)]
pub struct HostPolicy {
    base: Vec<String>,
    narrowed: Vec<String>,
    reach: Reach,
}

impl HostPolicy {
    /// The policy a flow recorded from `start_url` gets.
    pub fn from_start(start_url: &str) -> HostPolicy {
        HostPolicy {
            base: host_of(start_url)
                .map(|h| vec![base_domain(&h)])
                .unwrap_or_default(),
            narrowed: Vec::new(),
            reach: Reach::of_start(start_url),
        }
    }

    /// The policy of a saved flow.
    pub fn of(flow: &Flow) -> HostPolicy {
        HostPolicy {
            narrowed: flow.allowed_hosts.clone(),
            ..HostPolicy::from_start(&flow.start_url)
        }
    }

    /// The base domains, as `allowedHosts` records them.
    pub fn hosts(&self) -> &[String] {
        &self.base
    }

    pub fn allows_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.base.iter().any(|a| host_matches(&host, a))
            && (self.narrowed.is_empty() || self.narrowed.iter().any(|a| host_matches(&host, a)))
    }

    pub fn allows_url(&self, url: &str) -> bool {
        host_of(url).is_some_and(|h| self.allows_host(&h))
    }

    /// Reject a URL that is not http(s) or whose host is outside the policy.
    pub fn check(&self, url: &str) -> Result<(), String> {
        let parsed = url::Url::parse(url).map_err(|_| format!("'{}' is not a valid URL", url))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!("'{}' is not an http(s) URL", url));
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| format!("'{}' has no host", url))?;
        if self.allows_host(host) {
            // IP literals never reach the resolver, so they are checked here.
            return check_literal(url, self.reach);
        }
        Err(format!(
            "{} is outside the hosts this flow may reach ({})",
            host.to_ascii_lowercase(),
            if self.base.is_empty() {
                "none".to_string()
            } else {
                self.base.join(", ")
            }
        ))
    }
}
