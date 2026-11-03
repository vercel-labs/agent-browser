//! Pre-flight security check for the browser the daemon is about to drive.
//!
//! The daemon asks this module about the build it just connected to, before
//! any page work happens. The check is split in two halves so every browser
//! family plugs into the same hook:
//!
//! * [`family_for`] selects an advisory feed from the CDP product string.
//! * [`Family::fetch`] turns that feed into a latest stable version and the
//!   CVE IDs each release after it documented.
//!
//! Unsupported product strings are skipped. Provider sessions are exempt; locally
//! launched and attached browsers with recognized products use the same check.
//! Adding a family requires a new variant and its feed implementation.
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

const CHROME_RELEASES_URL: &str = "https://chromereleases.googleblog.com/feeds/posts/default?alt=json&q=Stable+Channel+Update+for+Desktop&max-results=30";
/// Mozilla publishes current Firefox versions here. It has no matching
/// machine-readable CVE feed, so Firefox gets a version verdict only.
const FIREFOX_VERSIONS_URL: &str = "https://product-details.mozilla.org/1.0/firefox_versions.json";

const CACHE_FILE: &str = "security-advisories.json";
/// Bump this when stored entries stop being valid for the trust that wrote them.
const CACHE_KEY_VERSION: &str = "v2";
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const FETCH_TIMEOUT: Duration = Duration::from_secs(4);
/// Both feeds are a few hundred kilobytes. Anything larger is not the feed.
const MAX_FEED_BYTES: usize = 4 * 1024 * 1024;
/// A cache entry older than this is not trusted at all, even offline.
const CACHE_MAX_AGE_SECS: u64 = 30 * 24 * 60 * 60;
/// How many CVE IDs the warning spells out before summarising the rest.
const LISTED_CVES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChromePlatform {
    Linux,
    Mac,
    Windows,
}

impl ChromePlatform {
    fn from_user_agent(user_agent: &str) -> Option<Self> {
        let user_agent = user_agent.to_ascii_lowercase();
        if user_agent.contains("android")
            || user_agent.contains("iphone")
            || user_agent.contains("ipad")
        {
            return None;
        }
        if user_agent.contains("windows") {
            Some(Self::Windows)
        } else if user_agent.contains("macintosh") || user_agent.contains("mac os") {
            Some(Self::Mac)
        } else if user_agent.contains("linux") {
            Some(Self::Linux)
        } else {
            None
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Mac => "mac",
            Self::Windows => "windows",
        }
    }

    fn version_history_platforms(self) -> &'static [&'static str] {
        match self {
            Self::Linux => &["linux"],
            Self::Mac => &["mac", "mac_arm64"],
            Self::Windows => &["win", "win64"],
        }
    }
}

const UNKNOWN_CHROME_PLATFORM_WARNING: &str =
    "Could not determine the connected Chrome platform from Browser.getVersion userAgent; no current security verdict is available.";

fn chrome_platform_from_user_agent(user_agent: &str) -> Result<ChromePlatform, &'static str> {
    ChromePlatform::from_user_agent(user_agent).ok_or(UNKNOWN_CHROME_PLATFORM_WARNING)
}

/// A browser family whose releases we can audit against published advisories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Recognized Chrome and Chromium product strings.
    Chrome,
    /// Firefox, whose release train Mozilla publishes but whose advisories
    /// are not available as structured data.
    Firefox,
}

impl Family {
    fn cache_key(self, platform: Option<ChromePlatform>) -> Option<String> {
        match self {
            Family::Chrome => Some(format!("{CACHE_KEY_VERSION}-chrome-{}", platform?.id())),
            Family::Firefox => Some(format!("{CACHE_KEY_VERSION}-firefox")),
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
            Family::Chrome => {
                "Update the browser; for agent-browser's Chrome, run `agent-browser install`"
            }
            Family::Firefox => "Update Firefox, then relaunch the session",
        }
    }
}

/// Pick the advisory source from the connected browser's CDP product string.
/// The engine name is not enough to classify an unrecognized remote product.
pub fn family_for(_engine: &str, product: &str) -> Option<Family> {
    let product = product.to_ascii_lowercase();
    if product.starts_with("firefox/") {
        return Some(Family::Firefox);
    }
    if ["chrome/", "headlesschrome/", "chromium/"]
        .iter()
        .any(|prefix| product.starts_with(prefix))
    {
        return Some(Family::Chrome);
    }
    None
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

/// One CVE named in a Stable post and the earliest build that post associates with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CveFix {
    id: String,
    fixed_in: String,
}

/// What the guard knows about one family and platform after a (possibly cached) refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Advisories {
    fetched_at: u64,
    latest_stable: String,
    #[serde(default)]
    chrome_desktop_release: bool,
    #[serde(default)]
    chrome_platform: Option<String>,
    #[serde(default)]
    cves: Vec<CveFix>,
}

impl Advisories {
    fn latest(&self) -> Option<Version> {
        Version::parse_build(&self.latest_stable).or_else(|| Version::parse(&self.latest_stable))
    }
}

/// Per-family and per-platform cache of default-trust verdicts, so one browser
/// target never reuses another's data and a custom trust never shares it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    families: HashMap<String, Advisories>,
}

/// A blocking verdict or a problem with the requested trust configuration.
#[derive(Debug, PartialEq, Eq)]
pub enum EnforcementError {
    Outdated(String),
    Trust(String),
}

impl EnforcementError {
    pub fn into_message(self) -> String {
        match self {
            Self::Outdated(message) | Self::Trust(message) => message,
        }
    }
}

/// Check a browser using request-local trust and return any fail-open warning.
pub async fn enforce_with_tls(
    engine: &str,
    product: &str,
    user_agent: &str,
    allowed: bool,
    trust: &crate::tls::TrustOptions,
) -> Result<Option<String>, EnforcementError> {
    if allowed || cfg!(test) {
        return Ok(None);
    }
    if engine == "chrome" && product.trim().is_empty() {
        return Ok(Some(
            "Could not determine the connected browser product; no current security verdict is available."
                .to_string(),
        ));
    }
    let Some(family) = family_for(engine, product) else {
        return Ok(None);
    };
    let platform = if family == Family::Chrome {
        match chrome_platform_from_user_agent(user_agent) {
            Ok(platform) => Some(platform),
            Err(warning) => return Ok(Some(warning.to_string())),
        }
    } else {
        None
    };
    let Some(installed) = Version::parse(product) else {
        return Ok(Some(
            "Could not determine the browser version; no current security verdict is available."
                .to_string(),
        ));
    };
    let (advisories, warning) = load(family, platform, trust).await?;
    if let Some(advisories) = advisories {
        decide(family, installed, &advisories).map_err(|mut message| {
            if let Some(warning) = &warning {
                message.push_str("\n\n");
                message.push_str(warning);
            }
            EnforcementError::Outdated(message)
        })?;
    }
    Ok(warning)
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
    let documented_later_cves = later_fixes(advisories, installed);
    Err(outdated_warning(
        family,
        installed,
        latest,
        &documented_later_cves,
        advisories.chrome_platform.as_deref(),
    ))
}

fn outdated_warning(
    family: Family,
    installed: Version,
    latest: Version,
    documented_later_cves: &[CveFix],
    chrome_platform: Option<&str>,
) -> String {
    let stable_label = if family == Family::Chrome {
        match chrome_platform {
            Some("linux") => "current stable Linux build",
            Some("mac") => "current stable macOS build",
            Some("windows") => "current stable Windows build",
            _ => "current stable build for the detected platform",
        }
    } else {
        "current stable build"
    };
    let mut message = format!(
        "Refusing to drive {} {}: the {} is {}.",
        family.label(),
        installed,
        stable_label,
        latest
    );
    if documented_later_cves.is_empty() {
        message.push_str(
            "\n\nNo CVEs were identified as documented in later Stable release notes. This does not establish that this build is free of known or unfixed vulnerabilities, and the advisory mapping may be incomplete.",
        );
    } else {
        let ids: Vec<&str> = documented_later_cves
            .iter()
            .map(|cve| cve.id.as_str())
            .collect();
        let shown = ids.len().min(LISTED_CVES);
        message.push_str(&format!(
            "\n\n{} CVE(s) are documented in later Stable release notes: {}",
            ids.len(),
            ids[..shown].join(", ")
        ));
        if ids.len() > shown {
            message.push_str(&format!(" (+{} more)", ids.len() - shown));
        }
    }
    if family == Family::Chrome {
        message.push_str(
            "\n\nStable-release CVE references do not prove that a fix is missing or that the browser is insecure. A CVE may have appeared in an earlier Early Stable build, and the release post does not map each CVE unambiguously to every platform build, so this advisory mapping may be incomplete. When the user agent does not identify macOS or Windows architecture, the lower stable version across both variants is used.",
        );
    }
    message.push_str(&format!(
        "\n\n{}. To launch this build anyway, pass `--allow-outdated-browser` or set {}=1.",
        family.update_hint(),
        ALLOW_ENV
    ));
    message
}

/// CVEs whose Stable post names a build later than the installed build, oldest
/// first, one entry per CVE at the earliest release note that named it.
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

async fn load(
    family: Family,
    platform: Option<ChromePlatform>,
    trust: &crate::tls::TrustOptions,
) -> Result<(Option<Advisories>, Option<String>), EnforcementError> {
    load_from(&cache_path(), family, platform, trust).await
}

/// Read the shared cache only for requests that used the default trust.
fn cached_advisories(
    path: &PathBuf,
    family: Family,
    platform: Option<ChromePlatform>,
    trust: &crate::tls::TrustOptions,
) -> Option<Advisories> {
    if !trust.is_default() {
        return None;
    }
    let cache_key = family.cache_key(platform)?;
    read_cache(path)
        .families
        .get(&cache_key)
        .cloned()
        .filter(|advisories| advisories.latest().is_some())
        .filter(|advisories| cache_entry_is_compatible(family, platform, advisories))
}

/// Store a refreshed verdict only for requests that used the default trust.
fn store_advisories(
    path: &PathBuf,
    family: Family,
    platform: Option<ChromePlatform>,
    trust: &crate::tls::TrustOptions,
    advisories: &Advisories,
) {
    if !trust.is_default() {
        return;
    }
    write_cache_entry(path, family, platform, advisories);
}

/// Resolve advisories for one request, valid only for that request's trust.
async fn load_from(
    path: &PathBuf,
    family: Family,
    platform: Option<ChromePlatform>,
    trust: &crate::tls::TrustOptions,
) -> Result<(Option<Advisories>, Option<String>), EnforcementError> {
    let client = match http_client(trust) {
        Ok(client) => Some(client),
        Err(ClientError::Trust(message)) => return Err(EnforcementError::Trust(message)),
        Err(ClientError::Unavailable) => None,
    };
    let cached = cached_advisories(path, family, platform, trust);
    if cached
        .as_ref()
        .is_some_and(|advisories| !is_stale(advisories))
    {
        return Ok((cached, None));
    }

    let Some(client) = client else {
        return Ok(select_advisories(family, cached, None));
    };
    let fresh = tokio::time::timeout(FETCH_TIMEOUT, family.fetch(&client, platform))
        .await
        .ok()
        .flatten();
    if let Some(advisories) = fresh.as_ref() {
        store_advisories(path, family, platform, trust, advisories);
    }
    Ok(select_advisories(family, cached, fresh))
}

fn cache_entry_is_compatible(
    family: Family,
    platform: Option<ChromePlatform>,
    advisories: &Advisories,
) -> bool {
    match family {
        Family::Chrome => {
            advisories.chrome_desktop_release
                && platform.is_some_and(|platform| {
                    advisories.chrome_platform.as_deref() == Some(platform.id())
                })
        }
        Family::Firefox => true,
    }
}

fn select_advisories(
    family: Family,
    cached: Option<Advisories>,
    fresh: Option<Advisories>,
) -> (Option<Advisories>, Option<String>) {
    if fresh.is_some() {
        return (fresh, None);
    }
    let cached = cached.filter(|advisories| !is_expired(advisories));
    let warning = match (family, cached.is_some()) {
        (Family::Chrome, true) => {
            "Could not obtain a trustworthy current Chrome stable desktop release for the detected platform; using an older platform-specific cached verdict."
        }
        (Family::Chrome, false) => {
            "Could not determine a trustworthy current Chrome stable desktop release for the detected platform; no current platform-specific security verdict is available."
        }
        (_, true) => "Could not refresh browser security advisories; using an older cached verdict.",
        (_, false) => {
            "Could not refresh browser security advisories; no current security verdict is available."
        }
    };
    (cached, Some(warning.to_string()))
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

fn write_cache_entry(
    path: &PathBuf,
    family: Family,
    platform: Option<ChromePlatform>,
    advisories: &Advisories,
) {
    let Some(cache_key) = family.cache_key(platform) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut cache = read_cache(path);
    cache.families.insert(cache_key, advisories.clone());
    if let Ok(encoded) = serde_json::to_string_pretty(&cache) {
        let _ = std::fs::write(path, encoded);
    }
}

#[derive(Debug)]
enum ClientError {
    Trust(String),
    Unavailable,
}

fn http_client(trust: &crate::tls::TrustOptions) -> Result<reqwest::Client, ClientError> {
    http_client_with_builder(reqwest::Client::builder(), trust)
}

fn http_client_with_builder(
    builder: reqwest::ClientBuilder,
    trust: &crate::tls::TrustOptions,
) -> Result<reqwest::Client, ClientError> {
    crate::tls::apply_to_reqwest(builder, trust)
        .map_err(ClientError::Trust)?
        .user_agent(format!("agent-browser/{}", env!("CARGO_PKG_VERSION")))
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(Duration::from_secs(2))
        .https_only(true)
        // The feeds are served directly; a redirect chain is either a proxy
        // artifact or an attempt to point the daemon somewhere else.
        .redirect(reqwest::redirect::Policy::limited(1))
        .build()
        .map_err(|_| ClientError::Unavailable)
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
    async fn fetch(
        self,
        client: &reqwest::Client,
        platform: Option<ChromePlatform>,
    ) -> Option<Advisories> {
        match self {
            Family::Chrome => fetch_chrome(client, platform).await,
            Family::Firefox => fetch_firefox(client).await,
        }
    }
}

async fn fetch_chrome(
    client: &reqwest::Client,
    platform: Option<ChromePlatform>,
) -> Option<Advisories> {
    let platform = platform?;
    let body = get_text(client, CHROME_RELEASES_URL).await?;
    let ceiling = chrome_desktop_stable_ceiling(&body, platform)?;
    let cves = cve_fixes_from_feed(&body);
    if ceiling <= Version::parse("1.0")? {
        return None;
    }

    let mut platform_versions = Vec::new();
    for api_platform in platform.version_history_platforms() {
        let url = chrome_version_history_url(api_platform, ceiling);
        let body = get_text(client, &url).await?;
        let version = version_history_latest_version(&body)?;
        if version > ceiling {
            return None;
        }
        platform_versions.push(version);
    }
    let latest = conservative_latest(&platform_versions)?;
    Some(Advisories {
        fetched_at: now_secs(),
        latest_stable: latest.to_string(),
        chrome_desktop_release: true,
        chrome_platform: Some(platform.id().to_string()),
        cves,
    })
}

async fn fetch_firefox(client: &reqwest::Client) -> Option<Advisories> {
    let body = get_text(client, FIREFOX_VERSIONS_URL).await?;
    let latest = firefox_stable_version(&body)?;
    Some(Advisories {
        fetched_at: now_secs(),
        latest_stable: latest.to_string(),
        chrome_desktop_release: false,
        chrome_platform: None,
        cves: Vec::new(),
    })
}

fn chrome_desktop_stable_ceiling(body: &str, platform: ChromePlatform) -> Option<Version> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let entries = json.get("feed")?.get("entry")?.as_array()?;
    let entry = entries.iter().find(|entry| {
        entry
            .get("title")
            .and_then(|title| title.get("$t"))
            .and_then(|title| title.as_str())
            .is_some_and(is_stable_desktop_post)
    })?;
    let content = entry
        .get("content")
        .and_then(|content| content.get("$t"))
        .and_then(|content| content.as_str())?;
    let text = visible_text(content);
    let sentence = release_sentence(&text)?;
    let groups = build_groups(sentence);
    let mut ceilings = Vec::new();

    for (index, (versions, start, end)) in groups.iter().enumerate() {
        let before = if index == 0 {
            platform_mentions(&sentence[..*start]).0
        } else {
            Vec::new()
        };
        let next_start = groups
            .get(index + 1)
            .map(|group| group.1)
            .unwrap_or(sentence.len());
        let after = platform_mentions(&sentence[*end..next_start]).0;
        if before.contains(&platform) || after.contains(&platform) {
            ceilings.push(versions.iter().copied().max()?);
        }
    }

    (ceilings.len() == 1).then(|| ceilings[0])
}

fn build_groups(text: &str) -> Vec<(Vec<Version>, usize, usize)> {
    let mut groups: Vec<(Vec<Version>, usize, usize)> = Vec::new();
    for (version, start, end) in build_mentions(text) {
        let split = groups
            .last()
            .is_some_and(|(_, _, previous_end)| platform_mentions(&text[*previous_end..start]).1);
        if split {
            groups.push((vec![version], start, end));
        } else if let Some((versions, _, previous_end)) = groups.last_mut() {
            versions.push(version);
            *previous_end = end;
        } else {
            groups.push((vec![version], start, end));
        }
    }
    groups
}

fn platform_mentions(text: &str) -> (Vec<ChromePlatform>, bool) {
    let words: Vec<String> = text
        .split(|character: char| !character.is_ascii_alphabetic())
        .map(str::to_ascii_lowercase)
        .collect();
    let mut platforms = Vec::new();
    let mut has_platform_label = false;
    for word in words {
        let platform = match word.as_str() {
            "linux" => Some(ChromePlatform::Linux),
            "mac" | "macos" | "macintosh" => Some(ChromePlatform::Mac),
            "windows" | "win" => Some(ChromePlatform::Windows),
            "android" | "chromeos" | "cros" => None,
            _ => continue,
        };
        has_platform_label = true;
        if let Some(platform) = platform {
            if !platforms.contains(&platform) {
                platforms.push(platform);
            }
        }
    }
    (platforms, has_platform_label)
}

fn chrome_version_history_url(platform: &str, ceiling: Version) -> String {
    format!(
        "https://versionhistory.googleapis.com/v1/chrome/platforms/{platform}/channels/stable/versions?filter=version%3C%3D{ceiling}&order_by=version%20desc&pageSize=1"
    )
}

fn version_history_latest_version(body: &str) -> Option<Version> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let version = json
        .get("versions")?
        .as_array()?
        .first()?
        .get("version")?
        .as_str()?;
    Version::parse_build(version)
}

fn conservative_latest(versions: &[Version]) -> Option<Version> {
    versions.iter().copied().min()
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
        let text = visible_text(content);
        let Some(fixed_in) = released_build(&text) else {
            continue;
        };
        for id in cve_ids_in(&text) {
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

/// The visible text of an HTML fragment, so tag attributes cannot supply versions.
fn visible_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    text.replace("&nbsp;", " ")
}

/// True only for the plain stable desktop post, not Early Stable, Extended
/// Stable, or a non-desktop channel.
fn is_stable_desktop_post(title: &str) -> bool {
    let normalized = title.split_whitespace().map(str::to_ascii_lowercase);
    normalized.eq(["stable", "channel", "update", "for", "desktop"])
}

/// Every full build number in a sentence, expanding the `156.0.8078.79/.80`
/// shorthand posts use for the platform builds of one release.
fn builds_in(text: &str) -> Vec<Version> {
    build_mentions(text)
        .into_iter()
        .map(|(version, _, _)| version)
        .collect()
}

fn build_mentions(text: &str) -> Vec<(Version, usize, usize)> {
    let bytes = text.as_bytes();
    let mut builds = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            if bytes[index] == b'.' && bytes.get(index + 1) == Some(&b'.') {
                break;
            }
            index += 1;
        }
        if text[index..].starts_with("..") {
            index += 2;
            continue;
        }
        let Some(version) = Version::parse_build(&text[start..index]) else {
            continue;
        };
        builds.push((version, start, index));
        if let Some((patch, shorthand_end)) = shorthand_patch(bytes, index) {
            let shorthand = Version {
                parts: [version.parts[0], version.parts[1], version.parts[2], patch],
            };
            builds.push((shorthand, index, shorthand_end));
            index = shorthand_end;
        }
    }
    builds
}

/// The patch number of a `/.80` or `/80` suffix, and the index just past it.
fn shorthand_patch(bytes: &[u8], start: usize) -> Option<(u64, usize)> {
    if bytes.get(start) != Some(&b'/') {
        return None;
    }
    let mut cursor = start + 1;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
    }
    let digits = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    if cursor == digits {
        return None;
    }
    let patch: u64 = std::str::from_utf8(&bytes[digits..cursor])
        .ok()?
        .parse()
        .ok()?;
    Some((patch, cursor))
}

fn release_sentence(text: &str) -> Option<&str> {
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if character != '.' {
            continue;
        }
        let ends_sentence = text[index + 1..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
        if !ends_sentence {
            continue;
        }
        let sentence = &text[start..index];
        if !builds_in(sentence).is_empty() {
            return Some(sentence);
        }
        start = index + 1;
    }
    let sentence = &text[start..];
    (!builds_in(sentence).is_empty()).then_some(sentence)
}

/// The earliest build a post names for its release, from the first sentence
/// that names one. `None` means the post never said, so nothing is guessed.
fn released_build(text: &str) -> Option<Version> {
    builds_in(release_sentence(text)?).into_iter().min()
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
            chrome_desktop_release: true,
            chrome_platform: Some("linux".to_string()),
            cves: cves
                .iter()
                .map(|(id, fixed_in)| CveFix {
                    id: id.to_string(),
                    fixed_in: fixed_in.to_string(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn advisory_client_verifies_peers_against_the_requested_ca() {
        use std::io::{Read, Write};
        use std::sync::Arc;

        let certs = rustls_pemfile::certs(
            &mut include_str!("../tests/fixtures/tls/server-a.pem").as_bytes(),
        )
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        let key = rustls_pemfile::private_key(
            &mut include_str!("../tests/fixtures/tls/server-a.key").as_bytes(),
        )
        .unwrap()
        .unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            let mut accepted = 0;
            while accepted < 3 && std::time::Instant::now() < deadline {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("accept failed: {error}"),
                };
                accepted += 1;
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut tls = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(config.clone()).unwrap(),
                    stream,
                );
                let mut request = [0; 1024];
                if tls.read(&mut request).is_ok() {
                    let _ = tls.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    );
                    let _ = tls.flush();
                }
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let ca_a = dir.path().join("ca-a.pem");
        let ca_b = dir.path().join("ca-b.pem");
        std::fs::write(&ca_a, include_str!("../tests/fixtures/tls/ca-a.pem")).unwrap();
        std::fs::write(&ca_b, include_str!("../tests/fixtures/tls/ca-b.pem")).unwrap();
        let make_client = |path: Option<&std::path::Path>| {
            let trust = crate::tls::TrustOptions {
                ca_cert: path.map(|path| path.to_string_lossy().into_owned()),
                ..Default::default()
            };
            http_client_with_builder(reqwest::Client::builder().no_proxy(), &trust).unwrap()
        };
        assert!(get_text(&make_client(None), &url).await.is_none());
        assert!(get_text(&make_client(Some(&ca_b)), &url).await.is_none());
        assert_eq!(
            get_text(&make_client(Some(&ca_a)), &url).await.as_deref(),
            Some("ok")
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn explicit_bad_ca_is_an_error_even_with_a_fresh_cached_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut cached = advisories("156.0.8078.80", &[]);
        cached.fetched_at = now_secs();
        write_cache_entry(&path, Family::Chrome, Some(ChromePlatform::Linux), &cached);
        assert_eq!(
            cached_advisories(
                &path,
                Family::Chrome,
                Some(ChromePlatform::Linux),
                &crate::tls::TrustOptions::default(),
            )
            .unwrap()
            .latest_stable,
            cached.latest_stable
        );

        let ca = dir.path().join("invalid.pem");
        std::fs::write(&ca, b"not a certificate").unwrap();
        let trust = crate::tls::TrustOptions {
            ca_cert: Some(ca.to_string_lossy().into_owned()),
            ..Default::default()
        };
        assert!(matches!(
            load_from(&path, Family::Chrome, Some(ChromePlatform::Linux), &trust,).await,
            Err(EnforcementError::Trust(_))
        ));
        let cached_for_custom =
            cached_advisories(&path, Family::Chrome, Some(ChromePlatform::Linux), &trust);
        assert!(cached_for_custom.is_none());
    }

    #[test]
    fn custom_trust_neither_reads_nor_writes_the_shared_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut fresh = advisories("156.0.8078.80", &[]);
        fresh.fetched_at = now_secs();
        write_cache_entry(&path, Family::Chrome, Some(ChromePlatform::Linux), &fresh);

        let custom = crate::tls::TrustOptions {
            ca_cert: Some("/tmp/custom-ca.pem".to_string()),
            ..Default::default()
        };
        let cached_for_custom =
            cached_advisories(&path, Family::Chrome, Some(ChromePlatform::Linux), &custom);
        assert!(cached_for_custom.is_none());
        store_advisories(
            &path,
            Family::Chrome,
            Some(ChromePlatform::Linux),
            &custom,
            &advisories("157.0.8103.26", &[]),
        );
        let cache = read_cache(&path);
        assert_eq!(cache.families.len(), 1);
        assert_eq!(
            cache.families["v2-chrome-linux"].latest_stable,
            "156.0.8078.80"
        );
    }

    #[test]
    fn offline_fallback_warns_whether_or_not_a_cached_verdict_is_usable() {
        let mut cached = advisories("156.0.8078.80", &[]);
        cached.fetched_at = now_secs().saturating_sub(CACHE_TTL_SECS + 1);
        let (result, warning) = select_advisories(Family::Chrome, Some(cached.clone()), None);
        assert!(result.is_some());
        let warning = warning.unwrap();
        assert!(warning.contains("cached verdict"));
        assert!(warning.contains("Chrome stable desktop release"));

        cached.fetched_at = now_secs().saturating_add(1);
        let (result, warning) = select_advisories(Family::Chrome, Some(cached), None);
        assert!(result.is_none());
        let warning = warning.unwrap();
        assert!(warning.contains("no current platform-specific security verdict"));
        assert!(warning.contains("Chrome stable desktop release"));
    }

    fn feed(entries: &[(&str, &str)]) -> String {
        let mut values = Vec::new();
        for (title, content) in entries {
            values.push(serde_json::json!({"title": {"$t": title}, "content": {"$t": content}}));
        }
        serde_json::json!({"feed": {"entry": values}}).to_string()
    }

    const STABLE_156_2026_10_29: &str = r#"<p><span style="color: #666666; font-size: large;">The Stable and Extended Stable channel has been updated to 156.0.8078.79/.80 for Windows and</span><span> </span><span>Mac and </span><span>156.0.8078.79 to </span><span>Linux which will roll out over the coming days/weeks. A full list of changes in this build is available in the <a href="https://chromium.googlesource.com/chromium/src/+log/156.0.8078.74..156.0.8078.80?pretty=fuller&amp;n=10000" style="color: #4184f3;">Log</a>.</span></p>
<p><span>Security Fixes and Rewards</span></p>
<p><span>This update includes 21 security fixes.</span></p>
<p><span>[TBD][576215540] Critical CVE-2026-76024: Memory corruption in MediaStream.</span></p>
<p><span>[TBD][559780982] High CVE-2026-76025: Use after free in FileSystem.</span></p>
<p><span>[TBD][574213633] High CVE-2026-119260: Use after free in Frames.</span></p>"#;

    const STABLE_138_2025_06_24: &str = r#"<p><span>The Chrome team is delighted to announce the promotion of Chrome 138 to the stable channel for Windows, Mac and Linux. This will roll out over the coming days/weeks.</span></p>
<p><span>Chrome&nbsp;</span><span>138.0.7204.49 (Linux)&nbsp;&nbsp;</span><span>138.0.7204.49/50&nbsp;</span><span>Windows and Mac</span><span>&nbsp;contains a number of fixes and improvements -- a list of changes is available in the<a href="https://chromium.googlesource.com/chromium/src/+log/137.0.7151.120..138.0.7204.50?pretty=fuller&amp;n=10000"> log</a></span><span>. Watch out for upcoming Chrome and Chromium blog posts about new features and big efforts delivered in 138.</span></p>
<p><span>Extended stable channel has also been updated to&nbsp;</span><span>138.0.7204.50 for Win/Mac</span></p>
<p><span>Security Fixes and Rewards</span></p>
<p><span>This update includes 11 security fixes.</span></p>
<p><span>[$4000][407328533] Medium CVE-2025-6555: Use after free in Animation.</span></p>
<p><span>[$1000][40062462] Low CVE-2025-6556: Insufficient policy enforcement in Loader.</span></p>
<p><span>[$1000][406631048] Low CVE-2025-6557: Insufficient data validation in DevTools.</span></p>"#;

    const STABLE_130_2024_10_29: &str = r#"<p>The Stable channel has been updated to 130.0.6723.91/.92 for Windows, Mac and 130.0.6723.91 for Linux which will roll out over the coming days/weeks. A full list of changes in this build is available in the <a href="https://chromium.googlesource.com/chromium/src/+log/130.0.6723.70..130.0.6723.92?pretty=fuller&amp;n=10000">Log</a>.</p>
<p>The Extended Stable channel has been updated to 130.0.6723.92 for Windows and Mac which will roll out over the coming days/weeks.</p>
<p>2025-11-20: Updated to correct the security bugs included in the release</p>
<p>Security Fixes and Rewards</p>
<p>This update includes 3 security fixes.</p>
<p>[TBD][375123371] Critical CVE-2024-10487: Out of bounds write in Dawn.</p>
<p>[TBD][374310077] High CVE-2024-10488: Use after free in WebRTC.</p>
<p>The previous version of these notes did not include the following security fixes which were included in the release:</p>
<p>[TBD][371840056] High CVE-2024-11920: Inappropriate implementation in Dawn.</p>"#;

    #[test]
    fn family_is_picked_from_recognized_product_prefixes() {
        assert_eq!(
            family_for("chrome", "HeadlessChrome/156.0.8078.80"),
            Some(Family::Chrome)
        );
        assert_eq!(
            family_for("chrome", "Chrome/156.0.8078.80"),
            Some(Family::Chrome)
        );
        assert_eq!(
            family_for("chrome", "Chromium/156.0.8078.80"),
            Some(Family::Chrome)
        );
        assert_eq!(
            family_for("firefox", "Firefox/157.0"),
            Some(Family::Firefox)
        );
        assert_eq!(family_for("chrome", "Edg/156.0.8078.80"), None);
        assert_eq!(family_for("chrome", "Electron/35.0.0"), None);
        assert_eq!(family_for("chrome", "ChromeFork/156.0.8078.80"), None);
        assert_eq!(family_for("chrome", "NotChrome/156.0.8078.80"), None);
        assert_eq!(family_for("chrome", "SomeOther/1.0"), None);
        assert_eq!(family_for("chrome", ""), None);
    }

    #[test]
    fn browsers_without_a_feed_have_no_family() {
        assert_eq!(family_for("lightpanda", "Lightpanda/1.0"), None);
        assert_eq!(family_for("obscura", "Obscura/0.2"), None);
    }

    #[test]
    fn headless_chrome_and_chrome_have_identical_verdicts() {
        let advisories = advisories("156.0.8078.79", &[("CVE-2026-0001", "156.0.8078.74")]);
        let chrome = Version::parse("Chrome/156.0.8078.80").unwrap();
        let headless = Version::parse("HeadlessChrome/156.0.8078.80").unwrap();
        assert_eq!(chrome, headless);
        assert_eq!(
            decide(Family::Chrome, chrome, &advisories),
            decide(Family::Chrome, headless, &advisories)
        );
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
        let documented_later_cves = vec![CveFix {
            id: "CVE-2026-0002".to_string(),
            fixed_in: "156.0.8078.48".to_string(),
        }];
        let message = outdated_warning(
            Family::Chrome,
            installed,
            latest,
            &documented_later_cves,
            Some("linux"),
        );
        assert!(message.contains("Chrome 155.0.8059.85"));
        assert!(message.contains("156.0.8078.80"));
        assert!(message.contains("current stable Linux build"));
        assert!(message.contains("CVE(s) are documented in later Stable release notes"));
        assert!(message.contains("does not map each CVE unambiguously"));
        assert!(message.contains("may have appeared in an earlier Early Stable build"));
        assert!(message.contains("do not prove that a fix is missing"));
        assert!(message.contains("CVE-2026-0002"));
        assert!(!message.contains("security fix(es) shipped after this build"));
        assert!(message.contains("agent-browser install"));
        assert!(message.contains("--allow-outdated-browser"));
        assert!(message.contains(ALLOW_ENV));
    }

    #[test]
    fn firefox_warning_uses_its_own_update_hint() {
        let installed = Version::parse("153.0").unwrap();
        let latest = Version::parse("157.0").unwrap();
        let message = outdated_warning(Family::Firefox, installed, latest, &[], None);
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
    fn only_the_stable_desktop_post_supplies_fixes() {
        let body = feed(&[
            (
                "Early Stable Update for Desktop",
                r#"<p>The Stable channel has been updated to 157.0.8103.26/.27 for Windows, Mac will be coming shortly as part of our early stable release to a small percentage of users.</p><p>CVE-2026-0003 was fixed.</p>"#,
            ),
            (
                "Extended Stable Channel Update for Desktop",
                r#"<p>The Extended Stable channel has been updated to 156.0.8078.81 for Windows and Mac.</p><p>CVE-2026-0004 was fixed.</p>"#,
            ),
            (
                "Stable Channel Update for Android",
                r#"<p>Chrome for Android updated to 156.0.8078.80.</p><p>CVE-2026-0005 was fixed.</p>"#,
            ),
            (
                "Chrome Beta for Desktop Update",
                r#"<p>The Beta channel has been updated to 158.0.8127.2 for Windows, Mac and Linux.</p><p>CVE-2026-0006 was fixed.</p>"#,
            ),
            (
                "Dev Channel Update for Desktop",
                r#"<p>Chrome Dev updated to 159.0.8130.0.</p><p>CVE-2026-0007 was fixed.</p>"#,
            ),
            ("Stable Channel Update for Desktop", STABLE_156_2026_10_29),
        ]);
        let fixes = cve_fixes_from_feed(&body);
        let ids: Vec<&str> = fixes.iter().map(|fix| fix.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["CVE-2026-119260", "CVE-2026-76024", "CVE-2026-76025"]
        );
    }

    #[test]
    fn stable_post_credits_its_release_not_the_previous_log_build() {
        let body = feed(&[("Stable Channel Update for Desktop", STABLE_156_2026_10_29)]);
        let fixes = cve_fixes_from_feed(&body);
        assert_eq!(fixes.len(), 3);
        assert!(fixes.iter().all(|fix| fix.fixed_in == "156.0.8078.79"));

        let advisories = Advisories {
            fetched_at: 0,
            latest_stable: "156.0.8078.79".to_string(),
            chrome_desktop_release: true,
            chrome_platform: Some("linux".to_string()),
            cves: fixes,
        };
        for patched in ["156.0.8078.79", "156.0.8078.80"] {
            assert!(later_fixes(&advisories, Version::parse(patched).unwrap()).is_empty());
        }
        assert_eq!(
            later_fixes(&advisories, Version::parse("156.0.8078.74").unwrap()).len(),
            3
        );
    }

    #[test]
    fn promotion_post_credits_its_builds_not_the_extended_stable_update() {
        let body = feed(&[("Stable Channel Update for Desktop", STABLE_138_2025_06_24)]);
        let fixes = cve_fixes_from_feed(&body);
        assert_eq!(fixes.len(), 3);
        assert!(fixes.iter().all(|fix| fix.fixed_in == "138.0.7204.49"));
    }

    #[test]
    fn correction_notes_and_extended_stable_sentences_keep_the_stable_build() {
        let body = feed(&[("Stable Channel Update for Desktop", STABLE_130_2024_10_29)]);
        let fixes = cve_fixes_from_feed(&body);
        let ids: Vec<&str> = fixes.iter().map(|fix| fix.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["CVE-2024-10487", "CVE-2024-10488", "CVE-2024-11920"]
        );
        assert!(fixes.iter().all(|fix| fix.fixed_in == "130.0.6723.91"));
    }

    #[test]
    fn a_post_that_never_names_a_build_supplies_no_fix() {
        let body = feed(&[(
            "Stable Channel Update for Desktop",
            r#"<p>This stable update fixes CVE-2026-9999.</p><p>A list of changes is available in the <a href="https://chromium.googlesource.com/chromium/src/+log/156.0.8078.74..156.0.8078.80">log</a>.</p>"#,
        )]);
        assert!(cve_fixes_from_feed(&body).is_empty());
    }

    #[test]
    fn platform_shorthand_expands_to_each_build() {
        let clause = "156.0.8078.79/.80 for Windows and Mac and 156.0.8078.79 to Linux";
        let builds = builds_in(clause);
        assert_eq!(builds.len(), 3);
        assert_eq!(builds[0], Version::parse_build("156.0.8078.79").unwrap());
        assert_eq!(builds[1], Version::parse_build("156.0.8078.80").unwrap());
        assert_eq!(builds[2], Version::parse_build("156.0.8078.79").unwrap());
    }

    #[test]
    fn a_changelog_range_names_only_the_build_it_ends_at() {
        let builds = builds_in("137.0.7151.120..138.0.7204.50");
        assert_eq!(builds, vec![Version::parse_build("138.0.7204.50").unwrap()]);
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
    fn ambiguous_cve_builds_are_not_claimed_missing_on_a_later_platform_patch() {
        let advisories = advisories("156.0.8078.80", &[("CVE-2026-0001", "156.0.8078.79")]);
        let warning = decide(
            Family::Chrome,
            Version::parse_build("156.0.8078.79").unwrap(),
            &advisories,
        )
        .unwrap_err();
        assert!(
            warning.contains("No CVEs were identified as documented in later Stable release notes")
        );
        assert!(warning.contains(
            "does not establish that this build is free of known or unfixed vulnerabilities"
        ));
        assert!(!warning.contains("CVE-2026-0001"));
        assert!(!warning.contains("missing security fix"));
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
        let zero = feed(&[(
            "Stable Channel Update for Desktop",
            "Stable update 0.0 for Windows, Mac and Linux.",
        )]);
        let partial = feed(&[(
            "Stable Channel Update for Desktop",
            "Stable update 156 for Windows, Mac and Linux.",
        )]);
        assert!(chrome_desktop_stable_ceiling(&zero, ChromePlatform::Linux).is_none());
        assert!(chrome_desktop_stable_ceiling(&partial, ChromePlatform::Linux).is_none());
        assert!(firefox_stable_version(r#"{"LATEST_FIREFOX_VERSION":"0.1"}"#).is_none());
    }

    #[test]
    fn feed_that_is_not_json_yields_no_fixes() {
        assert!(cve_fixes_from_feed("<html>rate limited</html>").is_empty());
        assert!(cve_fixes_from_feed("{}").is_empty());
    }

    #[test]
    fn chrome_platforms_use_bounded_version_history_results() {
        let chrome = feed(&[
            (
                "Early Stable Update for Desktop",
                "Stable updated to 157.0.8103.26 for Windows, Mac and Linux.",
            ),
            ("Stable Channel Update for Desktop", STABLE_156_2026_10_29),
        ]);
        let linux_ceiling = chrome_desktop_stable_ceiling(&chrome, ChromePlatform::Linux).unwrap();
        let mac_ceiling = chrome_desktop_stable_ceiling(&chrome, ChromePlatform::Mac).unwrap();
        let windows_ceiling =
            chrome_desktop_stable_ceiling(&chrome, ChromePlatform::Windows).unwrap();
        assert_eq!(linux_ceiling.to_string(), "156.0.8078.79");
        assert_eq!(mac_ceiling.to_string(), "156.0.8078.80");
        assert_eq!(windows_ceiling.to_string(), "156.0.8078.80");
        assert_eq!(
            chrome_version_history_url("linux", linux_ceiling),
            "https://versionhistory.googleapis.com/v1/chrome/platforms/linux/channels/stable/versions?filter=version%3C%3D156.0.8078.79&order_by=version%20desc&pageSize=1"
        );
        for api_platform in ["mac", "mac_arm64", "win", "win64"] {
            let ceiling = if api_platform.starts_with("mac") {
                mac_ceiling
            } else {
                windows_ceiling
            };
            let url = chrome_version_history_url(api_platform, ceiling);
            assert!(url.contains(&format!("/platforms/{api_platform}/")));
            assert!(url.contains("filter=version%3C%3D156.0.8078.80"));
        }
        assert_eq!(
            ChromePlatform::Mac.version_history_platforms(),
            &["mac", "mac_arm64"][..]
        );
        assert_eq!(
            ChromePlatform::Windows.version_history_platforms(),
            &["win", "win64"][..]
        );

        let linux = version_history_latest_version(r#"{"versions":[{"version":"156.0.8078.79"}]}"#)
            .unwrap();
        let mac = version_history_latest_version(r#"{"versions":[{"version":"156.0.8078.80"}]}"#)
            .unwrap();
        let mac_arm64 =
            version_history_latest_version(r#"{"versions":[{"version":"156.0.8078.80"}]}"#)
                .unwrap();
        let windows =
            version_history_latest_version(r#"{"versions":[{"version":"156.0.8078.80"}]}"#)
                .unwrap();
        let windows_64 =
            version_history_latest_version(r#"{"versions":[{"version":"156.0.8078.80"}]}"#)
                .unwrap();
        assert_eq!(linux.to_string(), "156.0.8078.79");
        assert_eq!(
            conservative_latest(&[mac, mac_arm64]).unwrap().to_string(),
            "156.0.8078.80"
        );
        assert_eq!(
            conservative_latest(&[windows, windows_64])
                .unwrap()
                .to_string(),
            "156.0.8078.80"
        );
        let linux_advisories = advisories(&linux.to_string(), &[]);
        let mut mac_advisories = advisories(&mac.to_string(), &[]);
        mac_advisories.chrome_platform = Some("mac".to_string());
        let mut windows_advisories = advisories(&windows.to_string(), &[]);
        windows_advisories.chrome_platform = Some("windows".to_string());
        assert!(decide(
            Family::Chrome,
            Version::parse_build("156.0.8078.79").unwrap(),
            &linux_advisories
        )
        .is_ok());
        assert!(decide(
            Family::Chrome,
            Version::parse_build("156.0.8078.79").unwrap(),
            &mac_advisories
        )
        .is_err());
        assert!(decide(
            Family::Chrome,
            Version::parse_build("156.0.8078.79").unwrap(),
            &windows_advisories
        )
        .is_err());
        assert!(decide(
            Family::Chrome,
            Version::parse_build("156.0.8078.80").unwrap(),
            &windows_advisories
        )
        .is_ok());
        assert!(version_history_latest_version(r#"{"versions":[]}"#).is_none());

        let firefox = r#"{"LATEST_FIREFOX_VERSION":"157.0","FIREFOX_ESR":"153.6.0esr"}"#;
        assert_eq!(
            firefox_stable_version(firefox).unwrap().to_string(),
            "157.0.0.0"
        );
        assert!(firefox_stable_version("<html>").is_none());
    }

    #[test]
    fn stable_ceilings_are_specific_to_the_target_platform_group() {
        let body = feed(&[
            (
                "Early Stable Update for Desktop",
                "Early Stable updated Linux to 156.0.8078.80.",
            ),
            (
                "Stable Channel Update for Desktop",
                "Stable updated to 156.0.8078.79/.81 for Windows and Mac and 156.0.8078.79 for Linux.",
            ),
        ]);
        let linux = chrome_desktop_stable_ceiling(&body, ChromePlatform::Linux).unwrap();
        let mac = chrome_desktop_stable_ceiling(&body, ChromePlatform::Mac).unwrap();
        let windows = chrome_desktop_stable_ceiling(&body, ChromePlatform::Windows).unwrap();
        let early_linux_candidate = Version::parse_build("156.0.8078.80").unwrap();

        assert_eq!(linux.to_string(), "156.0.8078.79");
        assert_eq!(mac.to_string(), "156.0.8078.81");
        assert_eq!(windows.to_string(), "156.0.8078.81");
        assert!(linux < early_linux_candidate);
        assert!(early_linux_candidate < mac);
        assert!(chrome_version_history_url("linux", linux)
            .contains("filter=version%3C%3D156.0.8078.79"));
    }

    #[test]
    fn missing_or_ambiguous_platform_build_does_not_fall_back_to_another_group() {
        let missing_linux = feed(&[(
            "Stable Channel Update for Desktop",
            "Stable updated to 156.0.8078.79/.80 for Windows and Mac.",
        )]);
        assert!(chrome_desktop_stable_ceiling(&missing_linux, ChromePlatform::Linux).is_none());
        assert_eq!(
            chrome_desktop_stable_ceiling(&missing_linux, ChromePlatform::Mac)
                .unwrap()
                .to_string(),
            "156.0.8078.80"
        );

        let ambiguous_linux = feed(&[(
            "Stable Channel Update for Desktop",
            "Stable updated to 156.0.8078.79 for Linux and 156.0.8078.80 for Linux.",
        )]);
        assert!(chrome_desktop_stable_ceiling(&ambiguous_linux, ChromePlatform::Linux).is_none());
    }

    #[test]
    fn chrome_platform_is_inferred_only_from_the_connected_user_agent() {
        assert_eq!(
            ChromePlatform::from_user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/156.0"
            ),
            Some(ChromePlatform::Linux)
        );
        assert_eq!(
            ChromePlatform::from_user_agent(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/156.0"
            ),
            Some(ChromePlatform::Mac)
        );
        assert_eq!(
            ChromePlatform::from_user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/156.0"
            ),
            Some(ChromePlatform::Windows)
        );
        assert_eq!(
            ChromePlatform::from_user_agent("Chrome/156.0.8078.80"),
            None
        );
        assert_eq!(
            chrome_platform_from_user_agent("Chrome/156.0.8078.80"),
            Err(UNKNOWN_CHROME_PLATFORM_WARNING)
        );
        assert_eq!(
            ChromePlatform::from_user_agent(
                "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 Chrome/156.0"
            ),
            None
        );
    }

    #[test]
    fn incomplete_stable_desktop_text_fails_open_instead_of_using_an_older_release() {
        let chrome = feed(&[
            (
                "Early Stable Update for Desktop",
                "Stable updated to 157.0.8103.26 for Windows, Mac and Linux.",
            ),
            (
                "Stable Channel Update for Desktop",
                "The latest stable desktop release is being prepared.",
            ),
            (
                "Stable Channel Update for Desktop",
                "Stable updated to 155.0.8059.85 for Windows, Mac and Linux.",
            ),
        ]);
        assert!(chrome_desktop_stable_ceiling(&chrome, ChromePlatform::Linux).is_none());

        let (advisories, warning) = select_advisories(Family::Chrome, None, None);
        assert!(advisories.is_none());
        assert!(warning
            .unwrap()
            .contains("Could not determine a trustworthy current Chrome stable desktop release"));
    }

    #[test]
    fn legacy_cache_entries_stay_unread_under_versioned_keys() {
        let cache: Cache = serde_json::from_str(
            r#"{"families":{"chrome":{"fetched_at":1,"latest_stable":"156.0.8078.80","cves":[]},"firefox":{"fetched_at":1,"latest_stable":"157.0","cves":[]}}}"#,
        )
        .unwrap();
        let chrome = &cache.families["chrome"];
        let firefox = &cache.families["firefox"];
        assert_eq!(chrome.latest_stable, "156.0.8078.80");
        assert!(!cache_entry_is_compatible(
            Family::Chrome,
            Some(ChromePlatform::Linux),
            chrome
        ));
        assert!(cache_entry_is_compatible(Family::Firefox, None, firefox));
        for key in [
            Family::Chrome.cache_key(Some(ChromePlatform::Linux)),
            Family::Chrome.cache_key(Some(ChromePlatform::Mac)),
            Family::Chrome.cache_key(Some(ChromePlatform::Windows)),
            Family::Firefox.cache_key(None),
        ] {
            assert!(!cache.families.contains_key(&key.unwrap()));
        }
    }

    #[test]
    fn cache_keeps_chrome_platforms_separate_and_ignores_legacy_entries() {
        let dir = std::env::temp_dir().join(format!("agent-browser-guard-{}", std::process::id()));
        let path = dir.join(CACHE_FILE);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"families":{"chrome":{"fetched_at":1,"latest_stable":"157.0.8103.26","cves":[]}}}"#,
        )
        .unwrap();

        let linux = advisories("156.0.8078.79", &[("CVE-2026-0001", "156.0.8078.74")]);
        let mut mac = advisories("156.0.8078.80", &[]);
        mac.chrome_platform = Some("mac".to_string());
        write_cache_entry(&path, Family::Chrome, Some(ChromePlatform::Linux), &linux);
        write_cache_entry(&path, Family::Chrome, Some(ChromePlatform::Mac), &mac);
        let mut firefox = advisories("157.0", &[]);
        firefox.chrome_desktop_release = false;
        firefox.chrome_platform = None;
        write_cache_entry(&path, Family::Firefox, None, &firefox);

        let cache = read_cache(&path);
        assert_eq!(cache.families["chrome"].latest_stable, "157.0.8103.26");
        assert_eq!(
            cached_advisories(
                &path,
                Family::Chrome,
                Some(ChromePlatform::Linux),
                &crate::tls::TrustOptions::default(),
            )
            .unwrap()
            .latest_stable,
            "156.0.8078.79"
        );
        assert_eq!(
            cache.families["v2-chrome-linux"].latest_stable,
            "156.0.8078.79"
        );
        assert_eq!(
            cache.families["v2-chrome-mac"].latest_stable,
            "156.0.8078.80"
        );
        assert_eq!(cache.families["v2-firefox"].latest_stable, "157.0");
        assert_eq!(cache.families["v2-chrome-linux"].cves.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
