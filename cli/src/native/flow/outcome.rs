//! What a tier returned for one request, and how a run's results are judged.

use super::*;

/// Why a run ends without trying anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// A 401 or 403: the session is signed out or lacks access.
    AuthExpired,
    /// A 401 or 403 for a request that needed a credential the file cannot hold.
    CredentialNotSendable,
    /// A 401 or 403 for a request that carries a recorded signature or timestamp.
    RequestSigned,
    /// A 429.
    RateLimited,
    /// A bot challenge page instead of data.
    Blocked,
    /// A write was sent and no answer came back.
    OutcomeUnknown,
    /// A write was accepted but its answer is not the recorded one.
    WriteApplied,
    /// The host resolved to an address flow run does not send to; nothing
    /// was sent, and no tier tries it.
    AddressRefused,
}

impl Stop {
    /// The machine-readable code on the error response.
    pub fn code(self) -> &'static str {
        match self {
            Stop::AuthExpired => "auth_expired",
            Stop::CredentialNotSendable => "credential_not_sendable",
            Stop::RequestSigned => "request_signed",
            Stop::RateLimited => "rate_limited",
            Stop::Blocked => "blocked",
            Stop::OutcomeUnknown => "outcome_unknown",
            Stop::WriteApplied => "write_applied",
            Stop::AddressRefused => "address_refused",
        }
    }

    fn advice(self) -> &'static str {
        match self {
            Stop::AuthExpired => "Sign in again in the browser, then rerun",
            Stop::CredentialNotSendable => {
                "The flow cannot send the credential this request needs; use the page UI for this task"
            }
            Stop::RequestSigned => {
                "The signature was recorded at capture time and cannot be replayed; use the page for this task"
            }
            Stop::RateLimited => "Wait before rerunning",
            Stop::OutcomeUnknown => "Check the site before rerunning; the write is not sent again",
            Stop::WriteApplied => {
                "The site accepted the write, so it was applied; do not repeat it. Check the site for the result"
            }
            Stop::Blocked => "Continue in the UI or let the user clear the challenge",
            Stop::AddressRefused => "Nothing was sent; continue in the UI",
        }
    }
}

#[derive(Clone)]
pub struct TierResult {
    pub id: String,
    pub step: usize,
    pub ok: bool,
    pub status: Option<u16>,
    pub reason: Option<String>,
    pub body: Option<Value>,
    pub ms: u128,
    /// The failure says the site or its gate changed, not that the inputs were wrong.
    pub drift: bool,
    /// The response came back with a different shape than the recording.
    pub shape_changed: bool,
    /// The response matches the recording except that its lists are absent,
    /// which is how many APIs answer a query with no results.
    pub empty: bool,
    pub tier: Tier,
    /// The response says the run cannot go on, whatever tier sends it.
    pub stop: Option<Stop>,
    /// The stop this denial would be, when a later tier may still get through.
    pub soft_stop: Option<Stop>,
    /// The request never got an answer; another tier may reach the site.
    pub transport: bool,
    /// The tier was not tried, so the result says nothing about it.
    pub skipped: bool,
    pub retry_after: Option<String>,
}

impl TierResult {
    /// A way of sending that did not produce an answer.
    pub fn fail(req: &PreparedRequest, tier: Tier, started: Instant, f: Failure) -> TierResult {
        let ms = started.elapsed().as_millis();
        TierResult::failed(req, tier, ms, f)
    }

    fn failed(req: &PreparedRequest, tier: Tier, ms: u128, f: Failure) -> TierResult {
        TierResult {
            id: req.id.clone(),
            step: req.step,
            ok: false,
            status: f.status,
            reason: Some(f.reason),
            body: None,
            ms,
            drift: f.drift,
            shape_changed: f.shape_changed,
            empty: false,
            tier,
            stop: f.stop,
            soft_stop: None,
            transport: f.transport,
            skipped: f.skipped,
            retry_after: None,
        }
    }
}

#[derive(Debug)]
pub struct Failure {
    pub status: Option<u16>,
    pub reason: String,
    pub drift: bool,
    pub shape_changed: bool,
    pub stop: Option<Stop>,
    pub transport: bool,
    pub skipped: bool,
}

impl Failure {
    /// The site answered with something other than what was recorded: a
    /// different shape, a page that is not JSON, a missing element in a
    /// replayed command. The flow is unverified and the next tier is tried.
    /// A local failure, a timeout or a skipped tier is never drift.
    pub fn drift(reason: String) -> Failure {
        Failure::new(true, None, reason)
    }

    /// The tier was not tried; another tier may still send the request.
    pub fn skipped(reason: String) -> Failure {
        Failure {
            skipped: true,
            ..Failure::new(false, None, reason)
        }
    }

    /// The tier cannot express this run; that says nothing about the site.
    pub fn unavailable(reason: String) -> Failure {
        Failure::new(false, None, reason)
    }

    /// A write whose outcome is not known is never sent again by another tier.
    pub fn unknown_outcome(reason: String) -> Failure {
        Failure {
            status: None,
            reason,
            drift: false,
            shape_changed: false,
            stop: Some(Stop::OutcomeUnknown),
            transport: false,
            skipped: false,
        }
    }

    pub(crate) fn new(drift: bool, status: Option<u16>, reason: String) -> Failure {
        Failure {
            status,
            reason,
            drift,
            shape_changed: false,
            stop: None,
            transport: false,
            skipped: false,
        }
    }

    /// The request never reached the site or got no answer back; a read may
    /// still get through another way.
    pub fn transport(reason: String) -> Failure {
        Failure {
            transport: true,
            ..Failure::new(false, None, reason)
        }
    }

    /// A response that ends the run: another tier would get the same answer,
    /// and it does not mean the site changed.
    /// The resolver refused the destination before any connection: the
    /// run ends without trying another tier.
    pub fn refused(reason: String) -> Failure {
        Failure {
            stop: Some(Stop::AddressRefused),
            ..Failure::new(false, None, reason)
        }
    }

    pub(crate) fn stop(stop: Stop, status: u16, reason: String) -> Failure {
        Failure {
            status: Some(status),
            reason,
            drift: false,
            shape_changed: false,
            stop: Some(stop),
            transport: false,
            skipped: false,
        }
    }
}

/// Challenge pages are HTML; a JSON body that mentions a captcha is data.
pub(crate) fn looks_like_html(body: &str) -> bool {
    let head = body.trim_start();
    head.starts_with('<') && serde_json::from_str::<Value>(body).is_err()
}

pub(crate) fn looks_like_challenge(body: &str) -> bool {
    let head: String = body
        .chars()
        .take(4000)
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "cf-chl",
        "captcha",
        "challenge-platform",
        "px-captcha",
        "_incapsula_",
    ]
    .iter()
    .any(|m| head.contains(m))
}

/// Words of a header name, split on non-alphanumerics and camelCase
/// (`xNonce`, `HMACSignature`).
fn header_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = vec![String::new()];
    for (i, c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            words.push(String::new());
            continue;
        }
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1);
        let boundary = c.is_ascii_uppercase()
            && prev.is_some_and(|p| {
                p.is_ascii_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase()))
            });
        if boundary {
            words.push(String::new());
        }
        words.last_mut().unwrap().push(c.to_ascii_lowercase());
    }
    words
}

/// Whether the recorded value of a header named like a timestamp or a
/// signature looks computed per request. A timestamp is an epoch in seconds or
/// milliseconds, with or without a fraction, or an ISO-8601 date and time. A
/// signature is hex or base64 that does not read as words, or a
/// `t=...,v1=...` list with such a part.
fn per_request_value(timestamp: bool, value: &str) -> bool {
    let v = value.trim();
    if timestamp {
        return is_epoch(v) || is_iso_datetime(v);
    }
    if v.contains(',') || v.contains(';') {
        let parts: Vec<&str> = v.split([',', ';']).map(str::trim).collect();
        return parts.len() >= 2
            && parts.iter().all(|p| p.contains('='))
            && parts.iter().any(|p| {
                let val = p.split_once('=').map(|(_, v)| v).unwrap_or("");
                is_epoch(val) || is_opaque(val)
            });
    }
    is_opaque(v)
}

fn is_epoch(v: &str) -> bool {
    let whole = v
        .split_once('.')
        .map(|(w, f)| (w, Some(f)))
        .unwrap_or((v, None));
    matches!(whole.0.len(), 10 | 13)
        && whole.0.bytes().all(|b| b.is_ascii_digit())
        && whole
            .1
            .is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

fn is_iso_datetime(v: &str) -> bool {
    let b = v.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_digit());
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    v.len() >= 19
        && [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
            .iter()
            .all(|i| digit(*i))
        && at(4, b'-')
        && at(7, b'-')
        && (at(10, b'T') || at(10, b' '))
        && at(13, b':')
        && at(16, b':')
}

/// 16 or more characters of hex, or 20 or more of base64 with a digit, that
/// are not just words: `SignatureVersion2` and `RSASSA-PSS-SHA256` are
/// settings, not signatures.
fn is_opaque(v: &str) -> bool {
    let hex = v.len() >= 16 && v.bytes().all(|b| b.is_ascii_hexdigit());
    let base64 = v.len() >= 20
        && v.bytes().any(|b| b.is_ascii_digit())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'));
    (hex || base64) && !reads_as_words(v)
}

/// Every word of the value (split on separators and camelCase) is a plain
/// word of 3 or more letters, optionally ending in a number, a short version
/// like `v4` or `p256`, or a small number.
fn reads_as_words(v: &str) -> bool {
    header_words(v).iter().filter(|w| !w.is_empty()).all(|w| {
        let letters = w.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
        let digits = &w[letters..];
        let numeric = digits.len() <= 4 && digits.bytes().all(|b| b.is_ascii_digit());
        numeric && (letters >= 3 || (letters > 0 && !digits.is_empty()) || letters == 0)
    })
}

/// Recorded headers that read as a per-request signature or timestamp: a
/// name with the whole word `ts`, `timestamp`, `sig`, `signature`, `hmac` or
/// `nonce`, and a value that looks computed.
pub(crate) fn signature_headers(headers: &[(String, String)]) -> Vec<String> {
    classify_signature_headers(headers, &[]).0
}

fn classify_signature_headers(
    headers: &[(String, String)],
    repeated: &[(String, String)],
) -> (Vec<String>, Vec<String>) {
    let (mut signed, mut unsure) = (Vec::new(), Vec::new());
    for (name, value) in headers {
        let words = header_words(name);
        let has = |list: &[&str]| words.iter().any(|w| list.contains(&w.as_str()));
        let stamp = has(&["ts", "timestamp"]);
        if !stamp && !has(&["sig", "signature", "hmac", "nonce"]) {
            continue;
        }
        let same_everywhere = repeated
            .iter()
            .any(|(k, v)| *k == name.to_ascii_lowercase() && v == value);
        if !same_everywhere && per_request_value(stamp, value) {
            signed.push(name.clone());
        } else {
            unsure.push(name.clone());
        }
    }
    (signed, unsure)
}

/// Judge a response the same way on every tier: blocked, rejected input,
/// changed shape, or data.
pub fn check_response(
    req: &PreparedRequest,
    status: u16,
    text: &str,
) -> Result<(Value, bool), Failure> {
    check_response_on(Tier::Http, req, status, text)
}

/// The recorded credential and signature headers describe what the first
/// tiers send; the replay sends whatever the page's own scripts produce.
fn check_response_on(
    tier: Tier,
    req: &PreparedRequest,
    status: u16,
    text: &str,
) -> Result<(Value, bool), Failure> {
    let recorded = tier != Tier::Replay;
    if (matches!(status, 401 | 403 | 429) || (200..300).contains(&status))
        && looks_like_html(text)
        && looks_like_challenge(text)
    {
        return Err(Failure::stop(
            Stop::Blocked,
            status,
            "bot challenge page instead of data".to_string(),
        ));
    }
    match status {
        401 | 403 if recorded && req.expect.stripped_auth => {
            return Err(Failure::stop(
                Stop::CredentialNotSendable,
                status,
                format!(
                    "HTTP {}: the recording sent a credential header the file does not keep",
                    status
                ),
            ))
        }
        401 | 403 => {
            let (signed, unsure) = if recorded {
                classify_signature_headers(&req.headers, &req.repeated_headers)
            } else {
                (Vec::new(), Vec::new())
            };
            if !signed.is_empty() {
                return Err(Failure::stop(
                    Stop::RequestSigned,
                    status,
                    format!(
                        "HTTP {}: the request carries {}, recorded as a per-request signature or timestamp",
                        status,
                        signed.join(", ")
                    ),
                ));
            }
            let hint = if unsure.is_empty() {
                String::new()
            } else {
                format!(
                    "; the request also carries {}, which may be a per-request value",
                    unsure.join(", ")
                )
            };
            return Err(Failure::stop(
                Stop::AuthExpired,
                status,
                format!(
                    "HTTP {}: the session is signed out or lacks access{}",
                    status, hint
                ),
            ));
        }
        429 => {
            return Err(Failure::stop(
                Stop::RateLimited,
                status,
                "HTTP 429: the site is rate limiting this session".to_string(),
            ))
        }
        // The proxy refused the request; the site never saw it.
        407 => {
            return Err(Failure::transport(format!(
                "HTTP 407: the proxy requires authentication"
            )))
        }
        _ => {}
    }
    if !(200..300).contains(&status) {
        return Err(Failure::new(
            false,
            Some(status),
            format!("HTTP {}", status),
        ));
    }
    let parsed: Value = serde_json::from_str(text)
        .map_err(|_| Failure::new(true, Some(status), "response is not JSON".to_string()))?;
    let current = signature(&parsed);
    // The signature is only a few levels deep; a list below that is checked by path.
    let gone = req
        .expect
        .list
        .as_deref()
        .filter(|path| list_gone(path, &parsed));
    if let Some(path) = gone {
        let found = match moved_list(&req.expect.signature, &parsed) {
            Some(to) => format!("; a list now appears at '{}'", to),
            None => String::new(),
        };
        return Err(Failure {
            status: Some(status),
            reason: format!(
                "response shape changed: the list at '{}' is missing{}",
                path, found
            ),
            drift: true,
            shape_changed: true,
            stop: None,
            transport: false,
            skipped: false,
        });
    }
    if let Err(reason) = signature_matches(&req.expect.signature, &current) {
        // Counts that went missing are allowed; the result is empty only when the list is. A
        // missing list is drift: a false drift sends the agent back to the
        // page, a missed one returns a silently wrong empty answer.
        if matches_without_counts(&req.expect.signature, &current) {
            let empty = match req.expect.list.as_deref() {
                Some(path) => lookup(&parsed, path)
                    .and_then(|v| v.as_array())
                    .is_none_or(|a| a.is_empty()),
                None => main_list(&parsed).is_none_or(|(_, items)| items.is_empty()),
            };
            return Ok((parsed, empty));
        }
        let found = match moved_list(&req.expect.signature, &parsed) {
            Some(path) => format!("; a list now appears at '{}'", path),
            None => String::new(),
        };
        return Err(Failure {
            status: Some(status),
            reason: format!("response shape changed: {}{}", reason, found),
            drift: true,
            shape_changed: true,
            stop: None,
            transport: false,
            skipped: false,
        });
    }
    Ok((parsed, false))
}

pub fn tier_result(
    req: &PreparedRequest,
    tier: Tier,
    started: Instant,
    outcome: Result<(u16, String), Failure>,
) -> TierResult {
    tier_result_of(req, tier, started, outcome, true)
}

/// `sent` is false for an answer a session route made up: no write left the
/// machine, so a 5xx or 408 does not mean it may have been applied.
pub fn tier_result_of(
    req: &PreparedRequest,
    tier: Tier,
    started: Instant,
    outcome: Result<(u16, String), Failure>,
    sent: bool,
) -> TierResult {
    let ms = started.elapsed().as_millis();
    let outcome = match req.expect.extract.as_ref().filter(|x| !x.is_null()) {
        Some(spec) => outcome.and_then(|(status, text)| match extract_embedded(&text, spec) {
            Some(json) => Ok((status, json)),
            None if (200..300).contains(&status) => Err(Failure::new(
                true,
                Some(status),
                "the page no longer embeds the recorded data".to_string(),
            )),
            None => Ok((status, text)),
        }),
        None => outcome,
    };
    let checked = outcome.and_then(|(status, text)| {
        check_response_on(tier, req, status, &text).map(|body| (status, body))
    });
    match checked {
        Ok((status, (body, empty))) => TierResult {
            id: req.id.clone(),
            step: req.step,
            ok: true,
            status: Some(status),
            reason: None,
            body: Some(body),
            ms,
            drift: false,
            shape_changed: false,
            empty,
            tier,
            stop: None,
            soft_stop: None,
            transport: false,
            skipped: false,
            retry_after: None,
        },
        Err(mut f) => {
            // A write answered with a 5xx or 408 may still have been applied,
            // unless a session route answered it and nothing was sent.
            if sent && req.write && f.stop.is_none() && matches!(f.status, Some(408 | 500..=599)) {
                f.stop = Some(Stop::OutcomeUnknown);
                f.drift = false;
                f.reason = format!("{}: the write may have been applied", f.reason);
            }
            // A challenge page in answer to a write that went out does not say
            // it was not applied.
            if sent
                && req.write
                && f.stop == Some(Stop::Blocked)
                && matches!(f.status, Some(200..=299))
            {
                f.stop = Some(Stop::OutcomeUnknown);
                f.reason = format!(
                    "{}: the write was sent, but the answer does not say it was applied",
                    f.reason
                );
            }
            // A write the site answered with a 2xx JSON body of another shape
            // was applied. Any other accepted-looking answer (a redirect to a
            // login, an HTML page) does not say it was.
            if sent && req.write && f.stop.is_none() && is_accepted(f.status) {
                if f.shape_changed && matches!(f.status, Some(200..=299)) {
                    f.stop = Some(Stop::WriteApplied);
                    f.drift = false;
                    f.reason = format!("{}: the write was applied", f.reason);
                } else {
                    f.stop = Some(Stop::OutcomeUnknown);
                    f.drift = false;
                    f.reason = format!(
                        "{}: the write was sent, but the answer does not say it was applied",
                        f.reason
                    );
                }
            }
            // The recording sent credentials or a signature a run cannot
            // copy, so a 401 or 403 may only mean this tier lacks them; the
            // next tier sends them from the page.
            let soft = f
                .stop
                .filter(|s| matches!(s, Stop::CredentialNotSendable | Stop::RequestSigned))
                .filter(|_| tier != Tier::Replay);
            if soft.is_some() {
                f.stop = None;
                f.drift = true;
            }
            TierResult {
                soft_stop: soft,
                ..TierResult::failed(req, tier, ms, f)
            }
        }
    }
}

pub(crate) fn with_retry_after(mut result: TierResult, retry_after: Option<String>) -> TierResult {
    if result.stop == Some(Stop::RateLimited) {
        result.retry_after = retry_after.filter(|v| !v.trim().is_empty());
    }
    result
}

/// The last failed attempt, when every way of sending that request was
/// refused with a 401 or 403. A last answer whose shape changed is drift.
pub fn denied_everywhere(results: &[TierResult]) -> Option<&TierResult> {
    let last = results.last().filter(|r| !r.ok && !r.shape_changed)?;
    let attempts: Vec<&TierResult> = results.iter().filter(|r| r.id == last.id).collect();
    (attempts.iter().all(|r| !r.ok) && attempts.iter().any(|r| matches!(r.status, Some(401 | 403))))
        .then_some(last)
}

/// Fold tier results into the run response. `attempts` holds every tier
/// tried, in order; the last attempt per request is the one that counts.
/// The flow is verified only when every request came back with the recorded
/// shape and no tier saw that shape change.
pub fn finish_run(
    flow: &mut Flow,
    attempts: &[TierResult],
    now: &str,
    total_ms: u128,
    shape: &Shape,
) -> Value {
    let mut last: Vec<&TierResult> = Vec::new();
    for a in attempts {
        match last.iter().position(|r| r.id == a.id) {
            Some(i) => last[i] = a,
            None => last.push(a),
        }
    }
    let all_ok = !last.is_empty() && last.iter().all(|r| r.ok);
    let shape_changed = attempts.iter().find(|a| a.shape_changed);
    // An empty answer cannot confirm the recorded lists still exist, so it
    // leaves the verified state as it was.
    let any_empty = last.iter().any(|r| r.ok && r.empty);
    // Only the answer itself reads as no results; an empty dependency does
    // not replace a response that has data.
    let empty = last.iter().any(|r| r.id == flow.output && r.ok && r.empty);
    if all_ok && shape_changed.is_none() && any_empty {
    } else if all_ok && shape_changed.is_none() {
        flow.verified_at = Some(now.to_string());
    } else if shape_changed.is_some()
        || (!all_ok && attempts.iter().any(|r| !r.ok && r.drift && !r.skipped))
    {
        flow.verified_at = None;
    }
    // Remember the tier that worked, so the next run starts there. A transport
    // error or a skipped tier says nothing about its tier, so the next run
    // starts after the last tier that failed for a reason that does.
    for r in last.iter().filter(|r| r.ok) {
        let learned = attempts
            .iter()
            .take_while(|a| !std::ptr::eq(*a, *r))
            .filter(|a| a.id == r.id && !a.transport && !a.skipped)
            .last();
        let before_any = attempts
            .iter()
            .take_while(|a| !std::ptr::eq(*a, *r))
            .any(|a| a.id == r.id);
        let next = match learned {
            Some(f) => Tier::ALL
                .iter()
                .position(|t| *t == f.tier)
                .and_then(|i| Tier::ALL.get(i + 1).copied()),
            None if before_any => continue,
            None => Some(r.tier),
        };
        let Some(next) = next else { continue };
        for req in flow
            .steps
            .iter_mut()
            .flat_map(|s| s.requests.iter_mut())
            .filter(|req| req.id == r.id)
        {
            req.tier = (next != Tier::Http).then_some(next);
        }
    }
    let output = last
        .iter()
        .find(|r| r.id == flow.output && r.ok)
        .and_then(|r| r.body.clone())
        .map(|b| {
            if empty {
                json!({ "total": 0, "matched": 0, "items": [] })
            } else {
                apply_shape(b, shape)
            }
        });
    let steps: Vec<Value> = last
        .iter()
        .map(|r| {
            let fell_back: Vec<Value> = attempts
                .iter()
                .filter(|a| a.id == r.id && !std::ptr::eq(*a, *r))
                .map(|a| json!({ "tier": a.tier, "error": a.reason }))
                .collect();
            let mut step = json!({
                "id": r.id,
                "step": r.step,
                "tier": if r.ok { json!(r.tier) } else { Value::Null },
                "status": r.status,
                "ms": r.ms,
                "error": r.reason,
            });
            if !fell_back.is_empty() {
                step["fellBack"] = json!(fell_back);
            }
            step
        })
        .collect();
    let mut response = json!({
        "name": flow.name,
        "origin": flow.origin,
        "ok": all_ok,
        "verified": all_ok && shape_changed.is_none() && !any_empty,
        "ms": total_ms,
        "steps": steps,
        "data": output,
    });
    if empty {
        response["empty"] = json!(true);
    }
    if let Some(a) = shape_changed {
        response["drift"] = json!({ "tier": a.tier, "step": a.step, "reason": a.reason });
    }
    response
}

/// A run that ended on a stop still saw the shapes of the tiers before it:
/// a changed shape leaves the flow unverified. Returns whether it did.
pub fn unverify_on_shape_change(flow: &mut Flow, attempts: &[TierResult]) -> bool {
    let changed = attempts.iter().any(|a| a.shape_changed);
    if changed {
        flow.verified_at = None;
    }
    changed
}

/// How a run that did not return data ended.
#[derive(Debug, PartialEq)]
pub enum RunOutcome {
    /// A response that ends the run, or a request every way of sending was
    /// refused.
    Stopped {
        stop: Stop,
        step: usize,
        tier: Tier,
        status: Option<u16>,
        reason: String,
        retry_after: Option<String>,
    },
    /// Every tier failed in a way that says the site changed.
    Drift {
        step: usize,
        tried: String,
        /// A response no longer has the recorded shape.
        shape_changed: bool,
    },
    /// A request failed; `examples` are the recorded param values when the
    /// site answered, since it likely rejected the new ones.
    Failed {
        step: usize,
        reason: String,
        examples: Option<String>,
    },
}

impl RunOutcome {
    /// A stop in the results, before anything is judged.
    pub fn stopped(results: &[TierResult]) -> Option<RunOutcome> {
        let (stop, result) = results
            .iter()
            .rev()
            .find_map(|r| r.stop.map(|s| (s, r)))
            .or_else(|| {
                denied_everywhere(results).map(|r| {
                    let stop = results
                        .iter()
                        .rev()
                        .filter(|a| a.id == r.id)
                        .find_map(|a| a.soft_stop)
                        .unwrap_or(Stop::CredentialNotSendable);
                    (stop, r)
                })
            })?;
        Some(RunOutcome::Stopped {
            stop,
            step: result.step,
            tier: result.tier,
            status: result.status,
            reason: result.reason.clone().unwrap_or_default(),
            retry_after: result.retry_after.clone(),
        })
    }

    /// Why a run that finished without data failed.
    pub fn failed(results: &[TierResult], flow: &Flow) -> RunOutcome {
        // A skipped tier was never tried, so the last real failure is the one to report.
        let failure = results
            .iter()
            .rev()
            .find(|r| !r.ok && !r.skipped)
            .or_else(|| results.iter().rev().find(|r| !r.ok));
        let step = failure.map(|r| r.step).unwrap_or(0);
        // Only the failing request's own attempts say what went wrong.
        let own = |r: &&TierResult| failure.is_some_and(|f| r.id == f.id);
        if results.iter().filter(own).any(|r| r.drift && !r.skipped) {
            let tried = results
                .iter()
                .filter(own)
                .map(|r| format!("{}: {}", r.tier.name(), r.reason.as_deref().unwrap_or("ok")))
                .collect::<Vec<_>>()
                .join("; ");
            let shape_changed = results
                .iter()
                .filter(own)
                .any(|r| r.drift && r.shape_changed);
            return RunOutcome::Drift {
                step,
                tried,
                shape_changed,
            };
        }
        let examples = flow
            .params
            .iter()
            .map(|p| format!("{}={}", p.name, p.example.as_deref().unwrap_or("")))
            .collect::<Vec<_>>()
            .join(" ");
        RunOutcome::Failed {
            step,
            reason: failure.and_then(|r| r.reason.clone()).unwrap_or_default(),
            examples: failure.and_then(|r| r.status).map(|_| examples),
        }
    }

    pub fn message(&self, name: &str) -> String {
        match self {
            RunOutcome::Stopped {
                stop,
                step,
                tier,
                reason,
                retry_after,
                ..
            } => {
                let wait = retry_after
                    .as_deref()
                    .map(|w| format!("; retry after {}", w))
                    .unwrap_or_default();
                format!(
                    "{}: flow '{}' step {} ({}): {}{}. {}",
                    stop.code(),
                    name,
                    step,
                    tier.name(),
                    reason,
                    wait,
                    stop.advice()
                )
            }
            RunOutcome::Drift {
                step,
                tried,
                shape_changed: true,
            } => format!(
                "drifted: flow '{}' step {} ({}). The flow is now marked unverified; use the page for this task and save the flow again",
                name, step, tried
            ),
            RunOutcome::Drift { step, tried, .. } => format!(
                "flow '{}' failed at step {} ({}). The flow is now marked unverified; drive the browser for this task instead",
                name, step, tried
            ),
            RunOutcome::Failed {
                step,
                reason,
                examples: None,
            } => format!("flow '{}' failed at step {}: {}", name, step, reason),
            RunOutcome::Failed {
                step,
                reason,
                examples: Some(examples),
            } => format!(
                "flow '{}' failed at step {}: {}. The site likely rejected the param values; the recorded run used {}",
                name, step, reason, examples
            ),
        }
    }

    /// The error response's `code` and `data`, for a stop.
    pub fn data(&self, name: &str) -> Option<Value> {
        match self {
            RunOutcome::Stopped {
                stop,
                step,
                status,
                retry_after,
                ..
            } => Some(json!({
                "status": stop.code(),
                "flow": name,
                "step": step,
                "httpStatus": status,
                "retryAfter": retry_after,
            })),
            RunOutcome::Drift {
                step,
                shape_changed: true,
                ..
            } => Some(json!({ "status": "drifted", "flow": name, "step": step })),
            _ => None,
        }
    }
}
