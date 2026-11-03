//! Pre-flight security check for the browser the daemon is about to drive.
//!
//! The daemon asks this module about the build it just connected to, before
//! any page work happens. The check is split in two halves so every browser
//! family plugs into the same hook:
//!
//! * [`family_for`] decides which published advisory feed speaks for the
//!   connected browser, from the engine name and the CDP product string.
//! * [`Family::fetch`] turns that feed into a latest stable version and the
//!   CVE IDs each release after it documented.
//!
//! Families with no machine-readable advisory source (Lightpanda, Obscura,
//! Safari, remote providers, user-attached browsers) resolve to `None` and
//! are never blocked. Adding one is a new `Family` variant plus its fetch
//! implementation; the daemon hook does not change.
//!
//! Results are cached for a day, so a warm cache costs no network and the
//! check adds no latency to a launch that is already fine. The check fails
//! open: no network and no cache means no verdict, and the launch proceeds.
//! Only a positive "this build is behind stable" verdict stops a launch, and
//! `--allow-outdated-browser` (or `AGENT_BROWSER_ALLOW_OUTDATED_BROWSER=1`)
//! turns even that off.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Set this to any truthy value to let an outdated browser launch anyway.
pub const ALLOW_ENV: &str = "AGENT_BROWSER_ALLOW_OUTDATED_BROWSER";

/// Chrome for Testing publishes the current stable version of every channel.
const CHROME_STABLE_URL: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json";
/// The Chrome Releases blog documents the CVE IDs each stable desktop update
/// fixed. No other Chrome feed exposes that mapping.
const CHROME_RELEASES_URL: &str = "https://chromereleases.googleblog.com/feeds/posts/default?alt=json&q=Stable+Channel+Update+for+Desktop&max-results=30";
/// Mozilla publishes current Firefox versions here. It has no matching
/// machine-readable CVE feed, so Firefox gets a version verdict only.
const FIREFOX_VERSIONS_URL: &str = "https://product-details.mozilla.org/1.0/firefox_versions.json";

const CACHE_FILE: &str = "security-advisories.json";
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const FETCH_TIMEOUT: Duration = Duration::from_secs(4);
/// Both feeds are a few hundred kilobytes. Anything larger is not the feed.
const MAX_FEED_BYTES: usize = 4 * 1024 * 1024;
/// A cache entry older than this is not trusted at all, even offline.
const CACHE_MAX_AGE_SECS: u64 = 30 * 24 * 60 * 60;
/// How many CVE IDs the warning spells out before summarising the rest.
const LISTED_CVES: usize = 8;

/// A browser family whose releases we can audit against published advisories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Chrome and Chromium. Also covers Chromium-based provider browsers,
    /// which report a `Chrome/` product string and track stable builds.
    Chrome,
    /// Firefox, whose release train Mozilla publishes but whose advisories
    /// are not available as structured data.
    Firefox,
}

impl Family {
    /// Stable key for the on-disk cache entry.
    fn id(self) -> &'static str {
        match self {
            Family::Chrome => "chrome",
            Family::Firefox => "firefox",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Family::Chrome => "Chrome",
            Family::Firefox => "Firefox",
        }
    }

    /// How the user updates this browser.
    fn update_hint(self) -> &'static str {
        match self {
            Family::Chrome => "Update with `agent-browser install`",
            Family::Firefox => "Update Firefox, then relaunch the session",
        }
    }
}

/// Pick the advisory source that speaks for the connected browser.
///
/// `engine` is the launch engine (`chrome`, `lightpanda`, `obscura`); a
/// browser reporting its own product string wins, because that is what is
/// actually running. `None` means no published feed covers this browser, so
/// there is nothing to warn about.
pub fn family_for(engine: &str, product: &str) -> Option<Family> {
    // The product string is what is actually running, so it wins over the
    // engine the daemon remembers: an attached Chrome after a Lightpanda
    // session still reports `Chrome/`, and skipping it would be a bypass.
    let product = product.to_ascii_lowercase();
    if product.contains("firefox") {
        return Some(Family::Firefox);
    }
    if product.contains("chrome") || product.contains("chromium") {
        return Some(Family::Chrome);
    }
    // Lightpanda and Obscura ship their own binaries, so auditing them
    // against Chrome's release train would be wrong.
    match engine {
        "chrome" | "" => Some(Family::Chrome),
        "firefox" => Some(Family::Firefox),
        _ => None,
    }
}

/// A `major.minor.build.patch` browser version. Ordering is numeric per part,
/// so `155.0.8059.85 < 156.0.8078.80`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    parts: [u64; 4],
}

impl Version {
    /// Parse a version out of a product string such as
    /// `Chrome/156.0.8078.80`, or a bare `156.0.8078.80` or `157.0`.
    pub fn parse(text: &str) -> Option<Self> {
        parse_dotted(text, 2)
    }

    /// Parse a full four-part build number, as used for release versions.
    pub fn parse_build(text: &str) -> Option<Self> {
        parse_dotted(text, 4)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.parts[0], self.parts[1], self.parts[2], self.parts[3]
        )
    }
}

fn parse_dotted(text: &str, min_parts: usize) -> Option<Version> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let mut parts = [0u64; 4];
    let mut count = 0;
    for piece in rest[..end].split('.') {
        if count == parts.len() {
            break;
        }
        if piece.is_empty() {
            return None;
        }
        parts[count] = piece.parse().ok()?;
        count += 1;
    }
    if count < min_parts {
        return None;
    }
    Some(Version { parts })
}

/// One documented security fix, and the release that carried it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CveFix {
    id: String,
    fixed_in: String,
}

/// What the guard knows about one family after a (possibly cached) refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Advisories {
    fetched_at: u64,
    latest_stable: String,
    #[serde(default)]
    cves: Vec<CveFix>,
}

impl Advisories {
    fn latest(&self) -> Option<Version> {
        Version::parse_build(&self.latest_stable).or_else(|| Version::parse(&self.latest_stable))
    }
}

/// Per-family cache, so switching engines never reuses another browser's data.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    families: HashMap<String, Advisories>,
}

/// True when the caller has opted out of the check.
pub fn override_requested() -> bool {
    std::env::var(ALLOW_ENV)
        .ok()
        .is_some_and(|val| !matches!(val.to_lowercase().as_str(), "0" | "false" | "no" | ""))
}

/// Refuse a browser build that is behind stable, unless `allowed`.
///
/// Anything the caller cannot resolve a verdict for passes, so an offline
/// machine or an unsupported engine never turns into a launch failure.
pub async fn enforce(engine: &str, product: &str, allowed: bool) -> Result<(), String> {
    if allowed || override_requested() {
        return Ok(());
    }
    // Unit and e2e tests drive real browsers without a network budget; the
    // verdict logic is exercised through `decide` below instead.
    if cfg!(test) {
        return Ok(());
    }
    let Some(family) = family_for(engine, product) else {
        return Ok(());
    };
    let Some(installed) = Version::parse(product) else {
        return Ok(());
    };
    let Some(advisories) = load(family).await else {
        return Ok(());
    };
    decide(family, installed, &advisories)
}

/// The verdict itself, with no I/O, so it can be tested against fixed data.
///
/// `Ok` covers both "this build is current" and "the published data does not
/// cover this build", which is why callers must not treat it as a statement
/// about the browser's security.
fn decide(family: Family, installed: Version, advisories: &Advisories) -> Result<(), String> {
    let Some(latest) = advisories.latest() else {
        return Ok(());
    };
    if installed >= latest {
        return Ok(());
    }
    let missing = later_fixes(advisories, installed);
    Err(outdated_warning(family, installed, latest, &missing))
}

fn outdated_warning(
    family: Family,
    installed: Version,
    latest: Version,
    missing: &[CveFix],
) -> String {
    let mut message = format!(
        "Refusing to drive {} {}: the current stable build is {}.",
        family.label(),
        installed,
        latest
    );
    if missing.is_empty() {
        message.push_str(
            "\n\nStable updates fix security bugs that this build still has; no published fix \
             could be tied to a build number.",
        );
    } else {
        let ids: Vec<&str> = missing.iter().map(|fix| fix.id.as_str()).collect();
        let shown = ids.len().min(LISTED_CVES);
        message.push_str(&format!(
            "\n\n{} documented security fix(es) shipped after this build: {}",
            ids.len(),
            ids[..shown].join(", ")
        ));
        if ids.len() > shown {
            message.push_str(&format!(" (+{} more)", ids.len() - shown));
        }
    }
    message.push_str(&format!(
        "\n\n{}. To launch this build anyway, pass `--allow-outdated-browser` or set {}=1.",
        family.update_hint(),
        ALLOW_ENV
    ));
    message
}

/// Documented fixes newer than the installed build, oldest first, one entry
/// per CVE at the earliest release that carried it.
fn later_fixes(advisories: &Advisories, installed: Version) -> Vec<CveFix> {
    let mut fixes: Vec<(Version, CveFix)> = advisories
        .cves
        .iter()
        .filter_map(|fix| {
            Version::parse_build(&fix.fixed_in)
                .filter(|fixed_in| *fixed_in > installed)
                .map(|fixed_in| (fixed_in, fix.clone()))
        })
        .collect();
    fixes.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    fixes.dedup_by(|a, b| a.1.id == b.1.id);
    fixes.into_iter().map(|(_, fix)| fix).collect()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn cache_path() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agent-browser")
        .join(CACHE_FILE)
}

async fn load(family: Family) -> Option<Advisories> {
    let path = cache_path();
    let cached = read_cache(&path).families.remove(family.id());
    if let Some(advisories) = cached.as_ref() {
        if !is_stale(advisories) {
            return cached;
        }
    }

    let client = http_client()?;
    match family.fetch(&client).await {
        Some(fresh) => {
            write_cache_entry(&path, family, &fresh);
            Some(fresh)
        }
        // A stale entry still names real CVEs, which beats no verdict at all,
        // but an expired or unparseable one is not a verdict source.
        None => cached.filter(|advisories| !is_expired(advisories)),
    }
}

fn is_stale(advisories: &Advisories) -> bool {
    let now = now_secs();
    // A timestamp from the future is not fresher, it is untrustworthy.
    if advisories.fetched_at > now {
        return true;
    }
    now - advisories.fetched_at > CACHE_TTL_SECS
}

/// Old enough that the entry should not be used as a fallback at all.
fn is_expired(advisories: &Advisories) -> bool {
    let now = now_secs();
    advisories.fetched_at > now || now - advisories.fetched_at > CACHE_MAX_AGE_SECS
}

fn read_cache(path: &PathBuf) -> Cache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_cache_entry(path: &PathBuf, family: Family, advisories: &Advisories) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut cache = read_cache(path);
    cache
        .families
        .insert(family.id().to_string(), advisories.clone());
    if let Ok(encoded) = serde_json::to_string_pretty(&cache) {
        let _ = std::fs::write(path, encoded);
    }
}

fn http_client() -> Option<reqwest::Client> {
    crate::tls::apply_to_reqwest(reqwest::Client::builder(), &crate::tls::process_options())
        .ok()?
        .user_agent(format!("agent-browser/{}", env!("CARGO_PKG_VERSION")))
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(Duration::from_secs(2))
        // The feeds are served directly; a redirect chain is either a proxy
        // artifact or an attempt to point the daemon somewhere else.
        .redirect(reqwest::redirect::Policy::limited(1))
        .build()
        .ok()
}

async fn get_text(client: &reqwest::Client, url: &str) -> Option<String> {
    if !url.starts_with("https://") {
        return None;
    }
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_FEED_BYTES as u64)
    {
        return None;
    }
    // Read with a hard cap so a hostile or broken response cannot grow the
    // daemon's heap.
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > MAX_FEED_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).ok()
}

impl Family {
    /// Refresh this family's view of stable plus the fixes documented since.
    async fn fetch(self, client: &reqwest::Client) -> Option<Advisories> {
        match self {
            Family::Chrome => fetch_chrome(client).await,
            Family::Firefox => fetch_firefox(client).await,
        }
    }
}

async fn fetch_chrome(client: &reqwest::Client) -> Option<Advisories> {
    let stable = get_text(client, CHROME_STABLE_URL)
        .await
        .and_then(|body| chrome_stable_version(&body));
    let cves = get_text(client, CHROME_RELEASES_URL)
        .await
        .map(|body| cve_fixes_from_feed(&body))
        .unwrap_or_default();

    let latest = stable.or_else(|| {
        cves.iter()
            .filter_map(|fix| Version::parse_build(&fix.fixed_in))
            .max()
    })?;
    if latest <= Version::parse("1.0")? {
        return None;
    }
    Some(Advisories {
        fetched_at: now_secs(),
        latest_stable: latest.to_string(),
        cves,
    })
}

async fn fetch_firefox(client: &reqwest::Client) -> Option<Advisories> {
    let body = get_text(client, FIREFOX_VERSIONS_URL).await?;
    let latest = firefox_stable_version(&body)?;
    Some(Advisories {
        fetched_at: now_secs(),
        latest_stable: latest.to_string(),
        cves: Vec::new(),
    })
}

fn chrome_stable_version(body: &str) -> Option<Version> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let version = json
        .get("channels")?
        .get("Stable")?
        .get("version")?
        .as_str()?;
    Version::parse_build(version)
}

fn firefox_stable_version(body: &str) -> Option<Version> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let version = json.get("LATEST_FIREFOX_VERSION")?.as_str()?;
    let parsed = Version::parse(version)?;
    // A release version is a real, dispatched build, never `0.0` filler.
    (parsed > Version::parse("1.0")?).then_some(parsed)
}

/// Pull the CVE IDs each stable desktop release fixed out of the blog feed.
fn cve_fixes_from_feed(body: &str) -> Vec<CveFix> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(entries) = json
        .get("feed")
        .and_then(|feed| feed.get("entry"))
        .and_then(|entries| entries.as_array())
    else {
        return Vec::new();
    };

    let mut fixes: Vec<CveFix> = Vec::new();
    for entry in entries {
        let title = entry
            .get("title")
            .and_then(|title| title.get("$t"))
            .and_then(|title| title.as_str())
            .unwrap_or_default();
        if !is_stable_desktop_post(title) {
            continue;
        }
        let content = entry
            .get("content")
            .and_then(|content| content.get("$t"))
            .and_then(|content| content.as_str())
            .unwrap_or_default();
        // A post lists one build per platform (for example Windows .73 and
        // mac/Linux .80). Credit the earliest one: a build at .73 already has
        // those fixes, and overstating the fixed-in version invents a gap.
        let Some(fixed_in) = earliest_build_in(content) else {
            continue;
        };
        for id in cve_ids_in(content) {
            fixes.push(CveFix {
                id,
                fixed_in: fixed_in.to_string(),
            });
        }
    }

    // The same CVE can appear in a follow-up post; keep the earliest release
    // that carried it, compared as versions rather than as strings so `.98`
    // stays older than `.100`.
    fixes.sort_by(|a, b| {
        a.id.cmp(&b.id).then_with(|| {
            let a_v = Version::parse_build(&a.fixed_in);
            let b_v = Version::parse_build(&b.fixed_in);
            a_v.cmp(&b_v)
        })
    });
    fixes.dedup_by(|a, b| a.id == b.id);
    fixes
}

fn is_stable_desktop_post(title: &str) -> bool {
    title.contains("Stable") && title.contains("Desktop")
}

fn earliest_build_in(text: &str) -> Option<Version> {
    let bytes = text.as_bytes();
    let mut earliest: Option<Version> = None;
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        if let Some(version) = Version::parse_build(&text[start..i]) {
            earliest = Some(match earliest {
                Some(current) if current <= version => current,
                _ => version,
            });
        }
    }
    earliest
}

fn cve_ids_in(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some(position) = rest.find("CVE-") {
        let tail = &rest[position + 4..];
        let year = tail.bytes().take_while(u8::is_ascii_digit).count();
        if year == 4 && tail.as_bytes().get(4) == Some(&b'-') {
            let number_part = &tail[5..];
            let number = number_part.bytes().take_while(u8::is_ascii_digit).count();
            if number >= 4 {
                ids.push(format!("CVE-{}-{}", &tail[..4], &number_part[..number]));
                rest = &number_part[number..];
                continue;
            }
        }
        rest = tail;
    }
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advisories(latest: &str, cves: &[(&str, &str)]) -> Advisories {
        Advisories {
            fetched_at: 0,
            latest_stable: latest.to_string(),
            cves: cves
                .iter()
                .map(|(id, fixed_in)| CveFix {
                    id: id.to_string(),
                    fixed_in: fixed_in.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn family_is_picked_from_the_product_string() {
        assert_eq!(
            family_for("chrome", "HeadlessChrome/156.0.8078.80"),
            Some(Family::Chrome)
        );
        assert_eq!(
            family_for("chrome", "Chrome/156.0.8078.80"),
            Some(Family::Chrome)
        );
        assert_eq!(
            family_for("firefox", "Firefox/157.0"),
            Some(Family::Firefox)
        );
        // An odd product string still resolves through the launch engine.
        assert_eq!(family_for("chrome", "SomeOther/1.0"), Some(Family::Chrome));
    }

    #[test]
    fn browsers_without_a_feed_have_no_family() {
        assert_eq!(family_for("lightpanda", "Lightpanda/1.0"), None);
        assert_eq!(family_for("obscura", "Obscura/0.2"), None);
        assert_eq!(family_for("chrome", ""), Some(Family::Chrome));
    }

    #[test]
    fn version_parses_product_strings_and_orders_numerically() {
        let installed = Version::parse("HeadlessChrome/155.0.8059.85").unwrap();
        let latest = Version::parse("156.0.8078.80").unwrap();
        assert_eq!(installed.to_string(), "155.0.8059.85");
        assert!(installed < latest);
        // Numeric, not lexicographic: 9 is older than 10.
        assert!(Version::parse("155.0.8059.9").unwrap() < Version::parse("155.0.8059.10").unwrap());
        // Firefox ships three-part versions.
        assert!(Version::parse("156.0.1").unwrap() < Version::parse("157.0").unwrap());
    }

    #[test]
    fn version_rejects_text_without_a_build_number() {
        assert!(Version::parse("Chrome").is_none());
        assert!(Version::parse("156").is_none());
        assert!(Version::parse_build("156.0.8078").is_none());
    }

    #[test]
    fn current_stable_is_not_blocked() {
        let advisories = advisories("156.0.8078.80", &[("CVE-2026-0001", "156.0.8078.74")]);
        let latest = advisories.latest().unwrap();
        let installed = Version::parse("Chrome/156.0.8078.80").unwrap();
        assert!(installed >= latest);
        assert!(later_fixes(&advisories, installed).is_empty());
    }

    #[test]
    fn only_fixes_newer_than_the_installed_build_count() {
        let advisories = advisories(
            "156.0.8078.80",
            &[
                ("CVE-2026-0001", "155.0.8059.80"),
                ("CVE-2026-0002", "156.0.8078.48"),
                ("CVE-2026-0003", "156.0.8078.74"),
            ],
        );
        let installed = Version::parse("Chrome/155.0.8059.85").unwrap();
        let missing: Vec<String> = later_fixes(&advisories, installed)
            .into_iter()
            .map(|fix| fix.id)
            .collect();
        assert_eq!(missing, vec!["CVE-2026-0002", "CVE-2026-0003"]);
    }

    #[test]
    fn warning_lists_cves_and_the_override() {
        let installed = Version::parse("155.0.8059.85").unwrap();
        let latest = Version::parse("156.0.8078.80").unwrap();
        let missing = vec![CveFix {
            id: "CVE-2026-0002".to_string(),
            fixed_in: "156.0.8078.48".to_string(),
        }];
        let message = outdated_warning(Family::Chrome, installed, latest, &missing);
        assert!(message.contains("Chrome 155.0.8059.85"));
        assert!(message.contains("156.0.8078.80"));
        assert!(message.contains("CVE-2026-0002"));
        assert!(message.contains("agent-browser install"));
        assert!(message.contains("--allow-outdated-browser"));
        assert!(message.contains(ALLOW_ENV));
    }

    #[test]
    fn firefox_warning_uses_its_own_update_hint() {
        let installed = Version::parse("153.0").unwrap();
        let latest = Version::parse("157.0").unwrap();
        let message = outdated_warning(Family::Firefox, installed, latest, &[]);
        assert!(message.contains("Firefox 153.0.0.0"));
        assert!(message.contains("Update Firefox"));
        assert!(!message.contains("agent-browser install"));
    }

    #[test]
    fn cve_ids_are_extracted_without_duplicates() {
        let ids = cve_ids_in("Fixes [1] CVE-2026-1234 and CVE-2026-1234, plus CVE-2026-5678");
        assert_eq!(ids, vec!["CVE-2026-1234", "CVE-2026-5678"]);
        assert!(cve_ids_in("no advisories here").is_empty());
        assert!(cve_ids_in("CVE-2026-").is_empty());
    }

    #[test]
    fn feed_keeps_stable_desktop_posts_only() {
        let feed = r#"{"feed":{"entry":[
            {"title":{"$t":"Stable Channel Update for Desktop"},
             "content":{"$t":"The Stable channel has been updated to 156.0.8078.80 for Windows, Mac and Linux. CVE-2026-1111 and CVE-2026-2222 were fixed."}},
            {"title":{"$t":"Stable Channel Update for Android"},
             "content":{"$t":"Chrome for Android updated to 156.0.8078.80. CVE-2026-9999 was fixed."}},
            {"title":{"$t":"Dev Channel Update for Desktop"},
             "content":{"$t":"Chrome Dev updated to 159.0.8130.0. CVE-2026-3333 was fixed."}}
        ]}}"#;
        let fixes = cve_fixes_from_feed(feed);
        let ids: Vec<&str> = fixes.iter().map(|fix| fix.id.as_str()).collect();
        assert_eq!(ids, vec!["CVE-2026-1111", "CVE-2026-2222"]);
        assert!(fixes.iter().all(|fix| fix.fixed_in == "156.0.8078.80"));
    }

    #[test]
    fn feed_credits_the_earliest_build_a_post_mentions() {
        // Windows shipped the fix in .73 and mac/Linux in .80; a Windows build
        // at .73 must not be reported as missing it.
        let feed = r#"{"feed":{"entry":[
            {"title":{"$t":"Stable Channel Update for Desktop"},
             "content":{"$t":"The Stable channel has been updated to 156.0.8078.73 for Windows and 156.0.8078.80 for Mac and Linux. CVE-2026-4444 was fixed."}}
        ]}}"#;
        let fixes = cve_fixes_from_feed(feed);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].fixed_in, "156.0.8078.73");
    }

    #[test]
    fn repeated_cves_keep_the_numerically_earliest_release() {
        let feed = r#"{"feed":{"entry":[
            {"title":{"$t":"Stable Channel Update for Desktop"},
             "content":{"$t":"updated to 156.0.8078.100 for Windows. CVE-2026-5555 was fixed."}},
            {"title":{"$t":"Stable Channel Update for Desktop"},
             "content":{"$t":"updated to 156.0.8078.98 for Windows. CVE-2026-5555 was fixed."}}
        ]}}"#;
        let fixes = cve_fixes_from_feed(feed);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].fixed_in, "156.0.8078.98");
    }

    #[test]
    fn decide_blocks_only_builds_behind_stable() {
        let advisories = advisories("156.0.8078.80", &[("CVE-2026-0001", "156.0.8078.74")]);
        assert!(decide(
            Family::Chrome,
            Version::parse("Chrome/156.0.8078.80").unwrap(),
            &advisories
        )
        .is_ok());
        let blocked = decide(
            Family::Chrome,
            Version::parse("Chrome/155.0.8059.85").unwrap(),
            &advisories,
        )
        .unwrap_err();
        assert!(blocked.contains("CVE-2026-0001"));
    }

    #[test]
    fn future_and_ancient_cache_timestamps_are_not_trusted() {
        let now = now_secs();
        let mut future = advisories("156.0.8078.80", &[]);
        future.fetched_at = now + 600;
        assert!(is_stale(&future));
        assert!(is_expired(&future));

        let mut ancient = advisories("156.0.8078.80", &[]);
        ancient.fetched_at = now - CACHE_MAX_AGE_SECS - 1;
        assert!(is_stale(&ancient));
        assert!(is_expired(&ancient));

        let mut normal = advisories("156.0.8078.80", &[]);
        normal.fetched_at = now - 60;
        assert!(!is_stale(&normal));
        assert!(!is_expired(&normal));
    }

    #[test]
    fn implausible_stable_versions_are_rejected() {
        assert!(chrome_stable_version(r#"{"channels":{"Stable":{"version":"0.0"}}}"#).is_none());
        assert!(chrome_stable_version(r#"{"channels":{"Stable":{"version":"156"}}}"#).is_none());
        assert!(firefox_stable_version(r#"{"LATEST_FIREFOX_VERSION":"0.1"}"#).is_none());
    }

    #[test]
    fn feed_that_is_not_json_yields_no_fixes() {
        assert!(cve_fixes_from_feed("<html>rate limited</html>").is_empty());
        assert!(cve_fixes_from_feed("{}").is_empty());
    }

    #[test]
    fn stable_versions_are_read_from_both_feeds() {
        let chrome = r#"{"channels":{"Stable":{"version":"156.0.8078.80"},"Beta":{"version":"158.0.8127.2"}}}"#;
        assert_eq!(
            chrome_stable_version(chrome).unwrap().to_string(),
            "156.0.8078.80"
        );
        assert!(chrome_stable_version("{}").is_none());

        let firefox = r#"{"LATEST_FIREFOX_VERSION":"157.0","FIREFOX_ESR":"153.6.0esr"}"#;
        assert_eq!(
            firefox_stable_version(firefox).unwrap().to_string(),
            "157.0.0.0"
        );
        assert!(firefox_stable_version("<html>").is_none());
    }

    #[test]
    fn cache_keeps_families_separate() {
        let dir = std::env::temp_dir().join(format!("agent-browser-guard-{}", std::process::id()));
        let path = dir.join(CACHE_FILE);
        write_cache_entry(
            &path,
            Family::Chrome,
            &advisories("156.0.8078.80", &[("CVE-2026-0001", "156.0.8078.74")]),
        );
        write_cache_entry(&path, Family::Firefox, &advisories("157.0", &[]));

        let cache = read_cache(&path);
        assert_eq!(
            cache.families["chrome"].latest_stable,
            "156.0.8078.80".to_string()
        );
        assert_eq!(cache.families["firefox"].latest_stable, "157.0".to_string());
        assert_eq!(cache.families["chrome"].cves.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
