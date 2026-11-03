//! Warns when the connected Chrome is behind stable and newer Stable releases fixed CVEs.

use std::collections::BTreeSet;
use std::time::Duration;

type Version = [u64; 4];

const FEED_URL: &str = "https://chromereleases.googleblog.com/feeds/posts/default?alt=json&q=Stable+Channel+Update+for+Desktop&max-results=30";
const MAX_BODY: usize = 4 * 1024 * 1024;

/// Returns a warning when `product` is a Chrome build older than the current stable release
/// and the Stable releases after it fixed CVEs; `None` means current, safe, unknown, or not Chrome.
pub async fn outdated_warning(product: &str) -> Option<String> {
    if cfg!(test) {
        return None;
    }
    let lower = product.to_ascii_lowercase();
    if !["chrome/", "headlesschrome/", "chromium/"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return None;
    }
    let installed = first_version(product)?;
    let feed = fetch_feed().await?;
    warning_from_feed(installed, &feed)
}

async fn fetch_feed() -> Option<String> {
    let client =
        crate::tls::apply_to_reqwest(reqwest::Client::builder(), &crate::tls::process_options())
            .ok()?
            .https_only(true)
            .timeout(Duration::from_secs(3))
            .user_agent(format!("agent-browser/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .ok()?;
    let mut resp = client.get(FEED_URL).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return None;
        }
    }
    String::from_utf8(body).ok()
}

fn first_version(text: &str) -> Option<Version> {
    text.split(|c: char| !c.is_ascii_digit() && c != '.')
        .find_map(|tok| {
            let nums: Vec<u64> = tok
                .trim_matches('.')
                .split('.')
                .map(|n| n.parse().ok())
                .collect::<Option<_>>()?;
            nums.get(..4)?.try_into().ok()
        })
}

fn visible_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
}

fn cve_ids(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .filter(|t| {
            let mut p = t.split('-');
            let digits = |s: Option<&str>, n: usize| {
                s.is_some_and(|s| s.len() >= n && s.bytes().all(|b| b.is_ascii_digit()))
            };
            p.next() == Some("CVE")
                && digits(p.next(), 4)
                && digits(p.next(), 4)
                && p.next().is_none()
        })
        .map(String::from)
        .collect()
}

fn warning_from_feed(installed: Version, feed: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(feed).ok()?;
    let posts: Vec<(Version, Vec<String>)> = json["feed"]["entry"]
        .as_array()?
        .iter()
        .filter_map(|e| {
            let title = e["title"]["$t"].as_str()?.to_ascii_lowercase();
            if !title
                .split_whitespace()
                .eq(["stable", "channel", "update", "for", "desktop"])
            {
                return None;
            }
            let text = visible_text(e["content"]["$t"].as_str()?);
            Some((first_version(&text)?, cve_ids(&text)))
        })
        .collect();
    let latest = posts.iter().map(|p| p.0).max()?;
    if installed >= latest {
        return None;
    }
    let cves: BTreeSet<&String> = posts
        .iter()
        .filter(|p| p.0 > installed)
        .flat_map(|p| &p.1)
        .collect();
    if cves.is_empty() {
        return None;
    }
    let fmt = |v: Version| v.map(|n| n.to_string()).join(".");
    let listed: Vec<&str> = cves.iter().take(3).map(|s| s.as_str()).collect();
    let more = (cves.len() > 3).then(|| format!(", +{} more", cves.len() - 3));
    Some(format!(
        "Chrome {} is behind the current stable release {}, which fixes {} CVEs ({}{}). Update the browser; for agent-browser's Chrome, run `agent-browser install`.",
        fmt(installed),
        fmt(latest),
        cves.len(),
        listed.join(", "),
        more.unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T: &str = "Stable Channel Update for Desktop";
    const OLD: Version = [150, 0, 7000, 1];

    fn feed(posts: &[(&str, &str)]) -> String {
        let entries: Vec<_> = posts
            .iter()
            .map(|(t, c)| json!({"title": {"$t": t}, "content": {"$t": c}}))
            .collect();
        json!({"feed": {"entry": entries}}).to_string()
    }

    #[test]
    fn current_or_newer_is_silent() {
        let f = feed(&[(T, "<p>156.0.8078.80 CVE-2026-1111</p>")]);
        assert!(warning_from_feed([156, 0, 8078, 80], &f).is_none());
        assert!(warning_from_feed([157, 0, 1, 1], &f).is_none());
    }

    #[test]
    fn outdated_with_cves_warns() {
        let f = feed(&[
            (
                T,
                "<p>The Stable channel has been updated to <span>156.0.8078.79/.80</span> for Windows/Mac and 156.0.8078.79 for Linux.</p> [$N][1] High CVE-2026-1111: Use after free. [$N][2] CVE-2026-2222: Heap overflow.",
            ),
            (
                "Stable  channel update for Desktop",
                "155.0.1.1 CVE-2026-3333 CVE-2026-4444 CVE-2026-1111",
            ),
        ]);
        let w = warning_from_feed(OLD, &f).unwrap();
        assert!(w.contains("fixes 4 CVEs"), "{w}");
        assert!(
            w.contains("CVE-2026-1111, CVE-2026-2222, CVE-2026-3333, +1 more"),
            "{w}"
        );
        assert!(w.contains("stable release 156.0.8078.79,"), "{w}");
    }

    #[test]
    fn behind_without_cves_is_silent() {
        assert!(warning_from_feed(OLD, &feed(&[(T, "156.0.8078.80 bug fixes")])).is_none());
    }

    #[test]
    fn other_channels_are_ignored() {
        let f = feed(&[
            (
                "Early Stable Update for Desktop",
                "156.0.8078.80 CVE-2026-1111",
            ),
            (
                "Extended Stable Channel Update for Desktop",
                "156.0.8078.80 CVE-2026-2222",
            ),
        ]);
        assert!(warning_from_feed(OLD, &f).is_none());
    }

    #[test]
    fn version_in_html_attribute_is_ignored() {
        let f = feed(&[(T, "<a title=\"999.0.0.1\">149.0.1.1</a> CVE-2026-1111")]);
        assert!(warning_from_feed(OLD, &f).is_none());
    }
}
