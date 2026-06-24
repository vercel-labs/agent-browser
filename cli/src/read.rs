use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use url::Url;

const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const BODY_LIMIT: usize = 2 * 1024 * 1024;
const READ_ACCEPT: &str = "text/markdown, text/plain;q=0.9, text/html;q=0.7, */*;q=0.1";
const USER_AGENT_VALUE: &str = concat!("agent-browser/", env!("CARGO_PKG_VERSION"), " read");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmsMode {
    Index,
    Full,
}

pub fn parse_llms_mode(raw: &str) -> Result<LlmsMode, String> {
    match raw {
        "index" => Ok(LlmsMode::Index),
        "full" => Ok(LlmsMode::Full),
        _ => Err(format!(
            "Invalid read --llms value '{}': expected index or full",
            raw
        )),
    }
}

#[derive(Debug, Clone)]
pub struct ReadOptions {
    /// Return the fetched response body without markdown or HTML extraction.
    pub raw: bool,
    /// Fail unless the selected response is served as Content-Type: text/markdown.
    pub require_md: bool,
    /// Return the nearest ancestor llms.txt or llms-full.txt view.
    pub llms: Option<LlmsMode>,
    /// Return a heading outline for the selected page content.
    pub outline: bool,
    /// Filter page sections, /llms.txt links, /llms-full.txt sections, or outline headings.
    pub filter: Option<String>,
    /// HTTP request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Extra HTTP headers. A supplied Accept header disables markdown negotiation fallbacks.
    pub headers: HashMap<String, String>,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            raw: false,
            require_md: false,
            llms: None,
            outline: false,
            filter: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            headers: HashMap::new(),
        }
    }
}

pub fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

pub fn parse_timeout_ms(raw: &str) -> Result<u64, String> {
    let ms = raw
        .parse::<u64>()
        .map_err(|_| format!("Invalid read timeout: {}", raw))?;
    if ms == 0 {
        return Err("Read timeout must be greater than 0".to_string());
    }
    Ok(ms)
}

pub fn normalize_url(raw: &str) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Read URL is empty".to_string());
    }

    let candidate = if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.contains("://")
    {
        trimmed.to_string()
    } else {
        format!("https://{}", trimmed)
    };

    let mut url = Url::parse(&candidate).map_err(|e| format!("Invalid read URL: {}", e))?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(format!(
                "Unsupported read URL scheme '{}': use http or https",
                scheme
            ))
        }
    }
    if url.host_str().is_none() {
        return Err("Read URL must include a host".to_string());
    }
    url.set_fragment(None);
    Ok(url)
}

struct ReadFetch {
    final_url: String,
    status: u16,
    content_type: String,
    success: bool,
    body: String,
    truncated: bool,
}

#[derive(Clone)]
struct LlmsLink {
    title: String,
    url: Url,
}

pub async fn run_read(raw_url: &str, options: ReadOptions) -> Result<Value, String> {
    let target = normalize_url(raw_url)?;
    let client = Client::builder()
        .timeout(Duration::from_millis(options.timeout_ms))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    match options.llms {
        Some(LlmsMode::Index) => return run_llms_index(&client, &target, &options).await,
        Some(LlmsMode::Full) => return run_llms_full(&client, &target, &options).await,
        None => {}
    }

    let primary = fetch_read_url(&client, target.clone(), &options)
        .await
        .map_err(|e| format!("Read request failed: {}", e))?;

    if !options.raw && !is_markdown_content_type(&primary.content_type) && should_try_md(&options) {
        if let Some(md_url) = markdown_fallback_url(&target) {
            match fetch_read_url(&client, md_url.clone(), &options).await {
                Ok(md) if md.success && is_markdown_content_type(&md.content_type) => {
                    return Ok(read_json_from_content(
                        &target,
                        &md,
                        "path-markdown",
                        md.body.clone(),
                        &options,
                    ));
                }
                Ok(_) | Err(_) => {}
            }
        }
    }

    if !options.raw && should_try_md(&options) {
        if let Some(llms) = try_llms_link(&client, &target, &options).await {
            return llms;
        }
    }

    if !primary.success {
        return Err(format!("Read failed with HTTP {}", primary.status));
    }

    let (source, content) = content_from_fetch(&primary, &options)?;
    Ok(read_json_from_content(
        &target, &primary, source, content, &options,
    ))
}

async fn run_llms_index(
    client: &Client,
    target: &Url,
    options: &ReadOptions,
) -> Result<Value, String> {
    let fetch = fetch_first_llms_file(client, target, "llms.txt", options).await?;
    let content = format_llms_index(&fetch.body, &fetch.final_url, options.filter.as_deref())?;
    Ok(read_json(target, &fetch, "llms-index", content))
}

async fn run_llms_full(
    client: &Client,
    target: &Url,
    options: &ReadOptions,
) -> Result<Value, String> {
    let fetch = fetch_first_llms_file(client, target, "llms-full.txt", options).await?;
    let content = if let Some(filter) = options.filter.as_deref() {
        filter_markdown_sections(&fetch.body, filter, "No matching llms-full.txt sections")
    } else {
        fetch.body.clone()
    };
    Ok(read_json(target, &fetch, "llms-full", content))
}

async fn try_llms_link(
    client: &Client,
    target: &Url,
    options: &ReadOptions,
) -> Option<Result<Value, String>> {
    let (llms_url, llms) = fetch_optional_llms_file(client, target, "llms.txt", options).await?;
    let link = find_llms_link_for_target(&llms.body, &llms_url, target)?;
    let fetch = fetch_read_url(client, link.url.clone(), options)
        .await
        .ok()?;
    if !fetch.success {
        return None;
    }
    if options.require_md && !is_markdown_content_type(&fetch.content_type) {
        return None;
    }
    let content = if is_html_content_type(&fetch.content_type) {
        html_to_markdownish(&fetch.body)
    } else {
        fetch.body.clone()
    };
    Some(Ok(read_json_from_content(
        target,
        &fetch,
        "llms-link",
        content,
        options,
    )))
}

async fn fetch_read_url(
    client: &Client,
    target: Url,
    options: &ReadOptions,
) -> Result<ReadFetch, String> {
    let mut request = client
        .get(target.clone())
        .header(USER_AGENT, USER_AGENT_VALUE);
    let has_accept_header = options
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("accept"));
    if !has_accept_header {
        request = request.header(ACCEPT, READ_ACCEPT);
    }
    for (key, value) in &options.headers {
        request = request.header(key, value);
    }

    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let success = response.status().is_success();
    let (body, truncated) = read_limited_text(response).await?;

    Ok(ReadFetch {
        final_url,
        status,
        content_type,
        success,
        body,
        truncated,
    })
}

fn content_from_fetch(
    fetch: &ReadFetch,
    options: &ReadOptions,
) -> Result<(&'static str, String), String> {
    let content_type_base = fetch.content_type.split(';').next().unwrap_or("").trim();
    let content_type_lower = content_type_base.to_ascii_lowercase();
    if options.require_md && !is_markdown_content_type(&fetch.content_type) {
        Err(expected_markdown_error(&fetch.content_type))
    } else if options.raw {
        Ok(("raw", fetch.body.clone()))
    } else if content_type_lower == "text/markdown" {
        Ok(("accept-markdown", fetch.body.clone()))
    } else if content_type_lower.starts_with("text/plain") {
        Ok(("text", fetch.body.clone()))
    } else if content_type_lower == "text/html" || content_type_lower == "application/xhtml+xml" {
        Ok(("html-fallback", html_to_markdownish(&fetch.body)))
    } else {
        Ok(("raw", fetch.body.clone()))
    }
}

fn read_json(target: &Url, fetch: &ReadFetch, source: &str, content: String) -> Value {
    json!({
        "url": target.to_string(),
        "finalUrl": fetch.final_url.clone(),
        "status": fetch.status,
        "contentType": fetch.content_type.clone(),
        "source": source,
        "truncated": fetch.truncated,
        "content": content,
    })
}

fn read_json_from_content(
    target: &Url,
    fetch: &ReadFetch,
    source: &str,
    content: String,
    options: &ReadOptions,
) -> Value {
    if options.outline {
        let outline = format_page_outline(&content, &fetch.final_url, options.filter.as_deref());
        read_json(target, fetch, &format!("{}-outline", source), outline)
    } else if let Some(filter) = options.filter.as_deref() {
        let filtered = filter_page_sections(&content, filter);
        read_json(target, fetch, &format!("{}-filtered", source), filtered)
    } else {
        read_json(target, fetch, source, content)
    }
}

fn is_markdown_content_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("text/markdown")
}

fn expected_markdown_error(content_type: &str) -> String {
    let base = content_type.split(';').next().unwrap_or("").trim();
    format!(
        "Expected text/markdown, got {}",
        if base.is_empty() {
            "unknown content type"
        } else {
            base
        }
    )
}

fn is_html_content_type(content_type: &str) -> bool {
    let base = content_type.split(';').next().unwrap_or("").trim();
    base.eq_ignore_ascii_case("text/html") || base.eq_ignore_ascii_case("application/xhtml+xml")
}

fn should_try_md(options: &ReadOptions) -> bool {
    !options
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("accept"))
}

fn markdown_fallback_url(url: &Url) -> Option<Url> {
    if url.path().ends_with(".md") {
        return None;
    }
    let mut md_url = url.clone();
    let path = url.path();
    let next_path = if path == "/" || path.is_empty() {
        "/index.md".to_string()
    } else {
        format!("{}.md", path.trim_end_matches('/'))
    };
    md_url.set_path(&next_path);
    Some(md_url)
}

async fn fetch_first_llms_file(
    client: &Client,
    target: &Url,
    filename: &str,
    options: &ReadOptions,
) -> Result<ReadFetch, String> {
    let mut last_status = None;
    for url in llms_file_candidates(target, filename) {
        let fetch = fetch_read_url(client, url, options)
            .await
            .map_err(|e| format!("Read request failed: {}", e))?;
        if fetch.success {
            if is_html_content_type(&fetch.content_type) {
                last_status = Some(fetch.status);
                continue;
            }
            if options.require_md && !is_markdown_content_type(&fetch.content_type) {
                return Err(expected_markdown_error(&fetch.content_type));
            }
            return Ok(fetch);
        }
        last_status = Some(fetch.status);
    }

    match last_status {
        Some(status) => Err(format!("{} failed with HTTP {}", filename, status)),
        None => Err(format!("{} not found", filename)),
    }
}

async fn fetch_optional_llms_file(
    client: &Client,
    target: &Url,
    filename: &str,
    options: &ReadOptions,
) -> Option<(Url, ReadFetch)> {
    for url in llms_file_candidates(target, filename) {
        let fetch = fetch_read_url(client, url.clone(), options).await.ok()?;
        if fetch.success && !is_html_content_type(&fetch.content_type) {
            return Some((url, fetch));
        }
    }
    None
}

fn llms_file_candidates(url: &Url, filename: &str) -> Vec<Url> {
    let mut candidates = Vec::new();
    let mut prefixes = Vec::new();
    let path = url.path().trim_matches('/');
    if !path.is_empty() {
        let segments = path.split('/').collect::<Vec<_>>();
        for len in (1..=segments.len()).rev() {
            prefixes.push(format!("/{}", segments[..len].join("/")));
        }
    }
    prefixes.push(String::new());

    for prefix in prefixes {
        let mut candidate = url.clone();
        let path = if prefix.is_empty() {
            format!("/{}", filename)
        } else {
            format!("{}/{}", prefix.trim_end_matches('/'), filename)
        };
        candidate.set_path(&path);
        candidate.set_query(None);
        candidate.set_fragment(None);
        if !candidates
            .iter()
            .any(|existing: &Url| existing == &candidate)
        {
            candidates.push(candidate);
        }
    }

    candidates
}

fn parse_llms_links(body: &str, base_url: &Url) -> Vec<LlmsLink> {
    let mut links = Vec::new();
    for line in body.lines() {
        let Some(line) = markdown_list_item_text(line) else {
            continue;
        };
        let mut cursor = 0;
        while let Some(label_start_rel) = line[cursor..].find('[') {
            let label_start = cursor + label_start_rel;
            if label_start > 0 && line.as_bytes().get(label_start - 1) == Some(&b'!') {
                cursor = label_start + 1;
                continue;
            }
            let Some(label_end_rel) = line[label_start + 1..].find("](") else {
                break;
            };
            let label_end = label_start + 1 + label_end_rel;
            let href_start = label_end + 2;
            let Some(href_end_rel) = line[href_start..].find(')') else {
                break;
            };
            let href_end = href_start + href_end_rel;
            let title = line[label_start + 1..label_end].trim();
            let href = line[href_start..href_end]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches('<')
                .trim_matches('>');
            if !title.is_empty() && !href.is_empty() {
                if let Ok(url) = base_url.join(href) {
                    links.push(LlmsLink {
                        title: title.to_string(),
                        url,
                    });
                }
            }
            cursor = href_end + 1;
        }
    }
    links
}

fn find_llms_link_for_target(body: &str, base_url: &Url, target: &Url) -> Option<LlmsLink> {
    let target_key = doc_match_key(target)?;
    let links = dedupe_llms_links(parse_llms_links(body, base_url));
    if let Some(exact) = links
        .iter()
        .find(|link| doc_match_key(&link.url).as_ref() == Some(&target_key))
    {
        return Some(exact.clone());
    }

    let target_origin = origin_key(target)?;
    let target_segment = last_doc_segment(target)?;
    let mut candidates = links
        .into_iter()
        .filter(|link| origin_key(&link.url).as_ref() == Some(&target_origin))
        .filter(|link| {
            last_doc_segment(&link.url).as_ref() == Some(&target_segment)
                || slugify_label(&link.title) == target_segment
        })
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

fn markdown_list_item_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest);
        }
    }

    let marker_end = trimmed.find(['.', ')'])?;
    if marker_end == 0 || !trimmed[..marker_end].chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    trimmed[marker_end + 1..].strip_prefix(' ')
}

fn dedupe_llms_links(mut links: Vec<LlmsLink>) -> Vec<LlmsLink> {
    let mut seen = HashSet::new();
    links.retain(|link| {
        seen.insert(format!(
            "{}\0{}",
            link.title.to_ascii_lowercase(),
            link.url.as_str()
        ))
    });
    links
}

fn doc_match_key(url: &Url) -> Option<String> {
    Some(format!("{}{}", origin_key(url)?, normalized_doc_path(url)))
}

fn origin_key(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let authority = if let Some(port) = url.port() {
        format!("{}:{}", host, port)
    } else {
        host.to_string()
    };
    Some(format!("{}://{}", url.scheme(), authority))
}

fn normalized_doc_path(url: &Url) -> String {
    let mut path = url.path().trim_end_matches('/').to_string();
    if path.is_empty() {
        path = "/".to_string();
    }
    if let Some(stripped) = path.strip_suffix(".md") {
        path = stripped.to_string();
    }
    if path.ends_with("/index") {
        path.truncate(path.len() - "/index".len());
        if path.is_empty() {
            path = "/".to_string();
        }
    }
    path
}

fn last_doc_segment(url: &Url) -> Option<String> {
    normalized_doc_path(url)
        .trim_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(|segment| segment.to_ascii_lowercase())
}

fn slugify_label(label: &str) -> String {
    let mut slug = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn format_llms_index(body: &str, final_url: &str, filter: Option<&str>) -> Result<String, String> {
    let base = Url::parse(final_url).map_err(|e| format!("Invalid llms.txt URL: {}", e))?;
    let mut links = dedupe_llms_links(parse_llms_links(body, &base));
    if let Some(filter) = filter {
        let needle = filter.to_ascii_lowercase();
        links.retain(|link| {
            link.title.to_ascii_lowercase().contains(&needle)
                || link.url.as_str().to_ascii_lowercase().contains(&needle)
        });
    }
    if links.is_empty() {
        if filter.is_some() {
            return Ok("No matching llms.txt links".to_string());
        }
        return Ok(normalize_markdownish(body));
    }

    let mut out = format!("# llms.txt\n\nSource: {}\n", final_url);
    for link in links {
        out.push_str(&format!("\n- [{}]({})", link.title, link.url));
    }
    Ok(out)
}

struct Heading {
    level: usize,
    title: String,
}

fn format_page_outline(content: &str, final_url: &str, filter: Option<&str>) -> String {
    let mut headings = parse_markdown_headings(content);
    if let Some(filter) = filter {
        let needle = filter.to_ascii_lowercase();
        headings.retain(|heading| heading.title.to_ascii_lowercase().contains(&needle));
    }
    if headings.is_empty() {
        if filter.is_some() {
            return "No matching headings".to_string();
        }
        return "No headings found".to_string();
    }

    let mut out = format!("# Outline\n\nSource: {}\n", final_url);
    for heading in headings {
        out.push('\n');
        out.push_str(&"  ".repeat(heading.level.saturating_sub(1)));
        out.push_str("- ");
        out.push_str(&heading.title);
    }
    out
}

fn parse_markdown_headings(content: &str) -> Vec<Heading> {
    content.lines().filter_map(parse_markdown_heading).collect()
}

fn filter_page_sections(content: &str, filter: &str) -> String {
    let needle = filter.to_ascii_lowercase();
    let lines = content.lines().collect::<Vec<_>>();
    let headings = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| parse_markdown_heading(line).map(|heading| (index, heading)))
        .collect::<Vec<_>>();

    let mut sections = Vec::new();
    let mut captured_until = 0;
    for (i, (start, heading)) in headings.iter().enumerate() {
        if *start < captured_until || !heading.title.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        let end = headings[i + 1..]
            .iter()
            .find(|(_, next)| next.level <= heading.level)
            .map(|(index, _)| *index)
            .unwrap_or(lines.len());
        captured_until = end;
        sections.push(lines[*start..end].join("\n").trim().to_string());
    }

    if !sections.is_empty() {
        return sections.join("\n\n");
    }

    filter_markdown_sections(content, filter, "No matching page sections")
}

fn parse_markdown_heading(line: &str) -> Option<Heading> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.is_empty()
        && !rest
            .chars()
            .next()
            .map(char::is_whitespace)
            .unwrap_or(false)
    {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim();
    if title.is_empty() {
        None
    } else {
        Some(Heading {
            level,
            title: title.to_string(),
        })
    }
}

fn filter_markdown_sections(body: &str, filter: &str, no_match_message: &str) -> String {
    let needle = filter.to_ascii_lowercase();
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in body.lines() {
        if line.trim_start().starts_with('#') && !current.trim().is_empty() {
            if current.to_ascii_lowercase().contains(&needle) {
                sections.push(current.trim().to_string());
            }
            current.clear();
        }
        current.push_str(line);
        current.push('\n');
    }

    if !current.trim().is_empty() {
        if current.to_ascii_lowercase().contains(&needle) {
            sections.push(current.trim().to_string());
        }
    }

    if !sections.is_empty() {
        return sections.join("\n\n");
    }

    let matching_lines = body
        .lines()
        .filter(|line| line.to_ascii_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    if matching_lines.is_empty() {
        no_match_message.to_string()
    } else {
        matching_lines.join("\n")
    }
}

async fn read_limited_text(response: reqwest::Response) -> Result<(String, bool), String> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        let remaining = BODY_LIMIT.saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

fn html_to_markdownish(html: &str) -> String {
    let stripped = strip_ignored_html_blocks(html);
    let mut out = String::new();
    let mut chars = stripped.chars().peekable();
    let mut in_pre = false;

    while let Some(ch) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }

        let mut tag = String::new();
        for next in chars.by_ref() {
            if next == '>' {
                break;
            }
            tag.push(next);
        }
        handle_html_tag(&tag, &mut out, &mut in_pre);
    }

    let decoded = decode_html_entities(&out);
    if in_pre {
        normalize_markdownish(&format!("{}\n```", decoded))
    } else {
        normalize_markdownish(&decoded)
    }
}

fn strip_ignored_html_blocks(html: &str) -> String {
    let mut remaining = html.to_string();
    for tag in ["script", "style", "noscript", "svg", "head"] {
        remaining = strip_tag_block(&remaining, tag);
    }
    remaining
}

fn strip_tag_block(input: &str, tag: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    let lower = input.to_ascii_lowercase();
    let close = format!("</{}>", tag);

    while let Some(start) = find_open_tag(&lower, tag, cursor) {
        out.push_str(&input[cursor..start]);
        let after_start = lower[start..]
            .find('>')
            .map(|idx| start + idx + 1)
            .unwrap_or(input.len());
        if let Some(close_rel) = lower[after_start..].find(&close) {
            cursor = after_start + close_rel + close.len();
        } else {
            cursor = input.len();
        }
    }
    out.push_str(&input[cursor..]);
    out
}

fn find_open_tag(lower: &str, tag: &str, cursor: usize) -> Option<usize> {
    let needle = format!("<{}", tag);
    let mut search = cursor;
    while let Some(rel) = lower[search..].find(&needle) {
        let start = search + rel;
        let after_name = start + needle.len();
        let is_boundary = lower
            .as_bytes()
            .get(after_name)
            .map(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/' | b'>'))
            .unwrap_or(true);
        if is_boundary {
            return Some(start);
        }
        search = after_name;
    }
    None
}

fn handle_html_tag(raw: &str, out: &mut String, in_pre: &mut bool) {
    let tag = raw.trim();
    if tag.is_empty() || tag.starts_with('!') || tag.starts_with('?') {
        return;
    }
    let closing = tag.starts_with('/');
    let name = tag
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_ascii_lowercase();

    if closing {
        match name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "section" | "article"
            | "main" | "header" | "footer" | "nav" | "blockquote" | "li" | "tr" | "table"
            | "ul" | "ol" => out.push_str("\n\n"),
            "pre" => {
                out.push_str("\n```\n\n");
                *in_pre = false;
            }
            _ => {}
        }
        return;
    }

    match name.as_str() {
        "br" => out.push('\n'),
        "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav"
        | "blockquote" | "table" | "tr" | "ul" | "ol" => out.push_str("\n\n"),
        "li" => out.push_str("\n- "),
        "h1" => out.push_str("\n\n# "),
        "h2" => out.push_str("\n\n## "),
        "h3" => out.push_str("\n\n### "),
        "h4" => out.push_str("\n\n#### "),
        "h5" => out.push_str("\n\n##### "),
        "h6" => out.push_str("\n\n###### "),
        "pre" => {
            out.push_str("\n\n```\n");
            *in_pre = true;
        }
        _ => {}
    }
}

fn decode_html_entities(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }

        let mut entity = String::new();
        while let Some(&next) = chars.peek() {
            if next == ';' {
                chars.next();
                break;
            }
            if entity.len() >= 16 || next.is_whitespace() || next == '&' {
                break;
            }
            entity.push(next);
            chars.next();
        }

        match decode_entity(&entity) {
            Some(decoded) => out.push(decoded),
            None => {
                out.push('&');
                out.push_str(&entity);
            }
        }
    }
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ if entity.starts_with("#x") || entity.starts_with("#X") => {
            u32::from_str_radix(&entity[2..], 16)
                .ok()
                .and_then(char::from_u32)
        }
        _ if entity.starts_with('#') => entity[1..].parse::<u32>().ok().and_then(char::from_u32),
        _ => None,
    }
}

fn normalize_markdownish(input: &str) -> String {
    let mut lines = Vec::new();
    let mut blank_count = 0;
    let mut in_fence = false;

    for raw in input.lines() {
        let line = if in_fence {
            raw.trim_end().to_string()
        } else {
            raw.split_whitespace().collect::<Vec<_>>().join(" ")
        };

        if line.trim() == "```" {
            in_fence = !in_fence;
        }

        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 && !lines.is_empty() {
                lines.push(String::new());
            }
        } else {
            blank_count = 0;
            lines.push(line);
        }
    }

    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn normalize_url_adds_https() {
        let url = normalize_url("example.com/docs").unwrap();
        assert_eq!(url.as_str(), "https://example.com/docs");
    }

    #[test]
    fn markdown_fallback_url_appends_md_before_query() {
        let url = normalize_url("https://example.com/docs/intro?lang=en").unwrap();
        let md = markdown_fallback_url(&url).unwrap();
        assert_eq!(md.as_str(), "https://example.com/docs/intro.md?lang=en");
    }

    #[test]
    fn llms_file_candidates_walks_to_origin_root() {
        let url = normalize_url("https://example.com/docs/organize/navigation?x=1").unwrap();
        let candidates = llms_file_candidates(&url, "llms.txt")
            .into_iter()
            .map(|url| url.to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            candidates,
            vec![
                "https://example.com/docs/organize/navigation/llms.txt",
                "https://example.com/docs/organize/llms.txt",
                "https://example.com/docs/llms.txt",
                "https://example.com/llms.txt",
            ]
        );
    }

    #[test]
    fn html_to_markdownish_extracts_readable_text() {
        let html = r#"
          <html><head><title>Skip</title><style>.x{}</style></head>
          <body><main><h1>Docs &amp; API</h1><p>Hello <strong>world</strong>.</p><ul><li>One</li><li>Two</li></ul></main></body></html>
        "#;
        let text = html_to_markdownish(html);
        assert!(text.contains("# Docs & API"));
        assert!(text.contains("Hello world."));
        assert!(text.contains("- One"));
        assert!(!text.contains(".x{}"));
    }

    #[test]
    fn html_to_markdownish_keeps_header_after_head() {
        let html = r#"
          <html><head><title>Skip</title></head><body>
          <header><a href="/">Home</a></header>
          <article><h1>agent-browser</h1><p>Browser automation CLI.</p></article>
          </body></html>
        "#;
        let text = html_to_markdownish(html);
        assert!(text.contains("Home"));
        assert!(text.contains("# agent-browser"));
        assert!(text.contains("Browser automation CLI."));
    }

    #[test]
    fn format_llms_index_uses_list_links_and_dedupes() {
        let body = r#"
# Docs

Inline [Authentication](/inline-auth) should not become a TOC item.

- [Authentication](/docs/auth)
- [Authentication](/docs/auth)
  - [Channels](/docs/channels)
1. [Introduction](/docs/introduction)
"#;
        let content =
            format_llms_index(body, "https://example.com/llms.txt", Some("auth")).unwrap();

        assert!(content.contains("[Authentication](https://example.com/docs/auth)"));
        assert!(!content.contains("inline-auth"));
        assert!(!content.contains("Introduction"));
        assert_eq!(content.matches("Authentication").count(), 1);
    }

    #[test]
    fn format_page_outline_extracts_and_filters_headings() {
        let content = "# Intro\n\nWelcome\n\n## Install\n\n### Token auth\n\n## Usage\n";
        let outline = format_page_outline(content, "https://example.com/docs", Some("auth"));

        assert!(outline.contains("# Outline"));
        assert!(outline.contains("Source: https://example.com/docs"));
        assert!(outline.contains("    - Token auth"));
        assert!(!outline.contains("Install"));
        assert!(!outline.contains("Usage"));
    }

    #[test]
    fn filter_page_sections_prefers_matching_headings() {
        let content = "# Guide\n\nIntro.\n\n## Setup\n\nInstall.\n\n## Response rendering\n\nRender JSON.\n\n### Custom renderer\n\nUse a component.\n\n## Further reading\n\nNext.";
        let filtered = filter_page_sections(content, "Response rendering");

        assert!(filtered.contains("## Response rendering"));
        assert!(filtered.contains("Render JSON."));
        assert!(filtered.contains("### Custom renderer"));
        assert!(filtered.contains("Use a component."));
        assert!(!filtered.contains("## Setup"));
        assert!(!filtered.contains("## Further reading"));
    }

    #[test]
    fn content_from_fetch_require_md_checks_raw_response() {
        let fetch = ReadFetch {
            final_url: "https://example.com".to_string(),
            status: 200,
            content_type: "text/html; charset=utf-8".to_string(),
            success: true,
            body: "<h1>HTML</h1>".to_string(),
            truncated: false,
        };
        let options = ReadOptions {
            raw: true,
            require_md: true,
            ..ReadOptions::default()
        };

        let err = content_from_fetch(&fetch, &options).unwrap_err();
        assert_eq!(err, "Expected text/markdown, got text/html");
    }

    #[tokio::test]
    async fn run_read_prefers_markdown_accept() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request
                .to_ascii_lowercase()
                .contains("accept: text/markdown"));
            let body = "# Markdown\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let data = run_read(&base, ReadOptions::default()).await.unwrap();
        assert_eq!(data["source"], "accept-markdown");
        assert_eq!(data["content"], "# Markdown\n");
    }

    #[tokio::test]
    async fn run_read_tries_md_suffix_after_html() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            for expected_path in ["/docs/intro", "/docs/intro.md"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {} ", expected_path)));
                assert!(request.contains("accept: text/markdown"));
                let (content_type, body) = if expected_path.ends_with(".md") {
                    ("text/markdown", "# Markdown fallback\n")
                } else {
                    ("text/html", "<h1>HTML</h1>")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let data = run_read(&base, ReadOptions::default()).await.unwrap();
        assert_eq!(data["url"], base);
        assert_eq!(data["finalUrl"], format!("{}.md", base));
        assert_eq!(data["source"], "path-markdown");
        assert_eq!(data["content"], "# Markdown fallback\n");
    }

    #[tokio::test]
    async fn run_read_uses_llms_link_after_direct_fallbacks() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            for expected_path in [
                "/docs/intro",
                "/docs/intro.md",
                "/docs/intro/llms.txt",
                "/docs/llms.txt",
                "/markdown/intro.md",
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {} ", expected_path)));
                let (status, content_type, body) = match expected_path {
                    "/docs/intro" => ("200 OK", "text/html", "<h1>HTML</h1>"),
                    "/docs/intro.md" => ("404 Not Found", "text/html", "missing"),
                    "/docs/intro/llms.txt" => ("404 Not Found", "text/html", "missing"),
                    "/docs/llms.txt" => {
                        ("200 OK", "text/markdown", "- [Intro](/markdown/intro.md)\n")
                    }
                    "/markdown/intro.md" => ("200 OK", "text/markdown", "# Intro via llms\n"),
                    _ => unreachable!(),
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let data = run_read(&base, ReadOptions::default()).await.unwrap();
        assert_eq!(data["source"], "llms-link");
        assert_eq!(
            data["finalUrl"],
            format!("http://{}/markdown/intro.md", addr)
        );
        assert_eq!(data["content"], "# Intro via llms\n");
    }

    #[tokio::test]
    async fn run_read_llms_index_filters_links() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            for expected_path in ["/docs/intro/llms.txt", "/docs/llms.txt"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {} ", expected_path)));
                let (status, content_type, body) = if expected_path == "/docs/llms.txt" {
                    (
                        "200 OK",
                        "text/markdown",
                        "- [Intro](/docs/intro)\n- [Authentication](/docs/auth)\n",
                    )
                } else {
                    ("200 OK", "text/html", "<h1>Not docs</h1>")
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let options = ReadOptions {
            llms: Some(LlmsMode::Index),
            filter: Some("auth".to_string()),
            ..ReadOptions::default()
        };
        let data = run_read(&base, options).await.unwrap();
        let content = data["content"].as_str().unwrap();
        assert_eq!(data["source"], "llms-index");
        assert_eq!(data["finalUrl"], format!("http://{}/docs/llms.txt", addr));
        assert!(content.contains("Authentication"));
        assert!(!content.contains("Intro]"));
    }

    #[tokio::test]
    async fn run_read_llms_full_filters_sections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            for expected_path in ["/docs/intro/llms-full.txt", "/docs/llms-full.txt"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {} ", expected_path)));
                let (status, content_type, body) = if expected_path == "/docs/llms-full.txt" {
                    (
                        "200 OK",
                        "text/markdown",
                        "# Intro\nWelcome.\n\n## Auth\nUse token auth.\n\n## Other\nNo match.\n",
                    )
                } else {
                    ("404 Not Found", "text/html", "missing")
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let options = ReadOptions {
            llms: Some(LlmsMode::Full),
            filter: Some("token".to_string()),
            ..ReadOptions::default()
        };
        let data = run_read(&base, options).await.unwrap();
        let content = data["content"].as_str().unwrap();
        assert_eq!(data["source"], "llms-full");
        assert_eq!(
            data["finalUrl"],
            format!("http://{}/docs/llms-full.txt", addr)
        );
        assert!(content.contains("## Auth"));
        assert!(!content.contains("# Intro"));
        assert!(!content.contains("## Other"));
    }

    #[tokio::test]
    async fn run_read_llms_full_require_md_rejects_text_plain() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.starts_with("get /docs/intro/llms-full.txt "));
            let body = "# Full docs\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let options = ReadOptions {
            llms: Some(LlmsMode::Full),
            require_md: true,
            ..ReadOptions::default()
        };
        let err = run_read(&base, options).await.unwrap_err();
        assert_eq!(err, "Expected text/markdown, got text/plain");
    }

    #[tokio::test]
    async fn run_read_outline_extracts_selected_page_headings() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.starts_with("get /docs/intro "));
            let body = "# Intro\n\n## Install\n\n### Token auth\n\n## Usage\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let options = ReadOptions {
            outline: true,
            filter: Some("auth".to_string()),
            ..ReadOptions::default()
        };
        let data = run_read(&base, options).await.unwrap();
        let content = data["content"].as_str().unwrap();
        assert_eq!(data["source"], "accept-markdown-outline");
        assert!(content.contains("    - Token auth"));
        assert!(!content.contains("Install"));
        assert!(!content.contains("Usage"));
    }

    #[tokio::test]
    async fn run_read_filter_extracts_selected_page_section() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.starts_with("get /docs/intro "));
            let body = "# Intro\n\n## Setup\n\nInstall.\n\n## Response rendering\n\nRender JSON.\n\n### Custom renderer\n\nUse a component.\n\n## Further reading\n\nNext.\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let options = ReadOptions {
            filter: Some("Response rendering".to_string()),
            ..ReadOptions::default()
        };
        let data = run_read(&base, options).await.unwrap();
        let content = data["content"].as_str().unwrap();
        assert_eq!(data["source"], "accept-markdown-filtered");
        assert!(content.contains("## Response rendering"));
        assert!(content.contains("### Custom renderer"));
        assert!(!content.contains("## Setup"));
        assert!(!content.contains("## Further reading"));
    }

    #[tokio::test]
    async fn run_read_allows_accept_override() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.contains("accept: application/json"));
            assert!(!request.contains("accept: text/markdown"));
            let body = "{\"ok\":true}\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let mut options = ReadOptions::default();
        options
            .headers
            .insert("Accept".to_string(), "application/json".to_string());
        let data = run_read(&base, options).await.unwrap();
        assert_eq!(data["source"], "raw");
        assert_eq!(data["content"], "{\"ok\":true}\n");
    }
}
