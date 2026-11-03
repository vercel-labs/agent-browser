//! What a tier returned for one request, and how a run's results are judged.

use super::*;

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
    /// The response says the run cannot go on, whatever tier sends it:
    /// `auth_expired`, `rate_limited` or `blocked`.
    pub kind: Option<&'static str>,
    pub retry_after: Option<String>,
}

impl TierResult {
    pub(crate) fn failed(req: &PreparedRequest, tier: Tier, ms: u128, f: Failure) -> TierResult {
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
            kind: f.kind,
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
    pub kind: Option<&'static str>,
}

impl Failure {
    pub(crate) fn new(drift: bool, status: Option<u16>, reason: String) -> Failure {
        Failure {
            status,
            reason,
            drift,
            shape_changed: false,
            kind: None,
        }
    }

    /// A response that ends the run: another tier would get the same answer,
    /// and it does not mean the site changed.
    pub(crate) fn stop(kind: &'static str, status: u16, reason: String) -> Failure {
        Failure {
            status: Some(status),
            reason,
            drift: false,
            shape_changed: false,
            kind: Some(kind),
        }
    }
}

/// Challenge pages are HTML; a JSON body that mentions a captcha is data.
/// A write whose outcome is not known is never sent again by another tier.
pub(crate) fn unknown_outcome(reason: String) -> Failure {
    Failure {
        status: None,
        reason,
        drift: false,
        shape_changed: false,
        kind: Some("outcome_unknown"),
    }
}

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

/// Judge a response the same way on every tier: blocked, rejected input,
/// changed shape, or data.
pub fn check_response(
    req: &PreparedRequest,
    status: u16,
    text: &str,
) -> Result<(Value, bool), Failure> {
    if (matches!(status, 401 | 403 | 429) || (200..300).contains(&status))
        && looks_like_html(text)
        && looks_like_challenge(text)
    {
        return Err(Failure::stop(
            "blocked",
            status,
            "bot challenge page instead of data".to_string(),
        ));
    }
    match status {
        401 | 403 => {
            return Err(Failure::stop(
                "auth_expired",
                status,
                format!("HTTP {}: the session is signed out or lacks access", status),
            ))
        }
        429 => {
            return Err(Failure::stop(
                "rate_limited",
                status,
                "HTTP 429: the site is rate limiting this session".to_string(),
            ))
        }
        407 => return Err(Failure::new(true, Some(status), format!("HTTP {}", status))),
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
    if let Err(reason) = signature_matches(&req.expect.signature, &current) {
        // A list that only went missing reads as no results; a list that
        // appeared under a new name means the contract moved.
        if matches_without_lists(&req.expect.signature, &current)
            && new_lists(&req.expect.signature, &current, "").is_empty()
        {
            return Ok((parsed, true));
        }
        return Err(Failure {
            status: Some(status),
            reason: format!("response shape changed: {}", reason),
            drift: true,
            shape_changed: true,
            kind: None,
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
    let checked = outcome
        .and_then(|(status, text)| check_response(req, status, &text).map(|body| (status, body)));
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
            kind: None,
            retry_after: None,
        },
        Err(mut f) => {
            // The recording sent credentials a run cannot copy, so a 401 or
            // 403 may only mean this tier lacks them; the next tier sends
            // them from the page.
            if f.kind == Some("auth_expired") && tier != Tier::Replay && req.expect.stripped_auth {
                f.kind = None;
                f.drift = true;
            }
            TierResult::failed(req, tier, ms, f)
        }
    }
}

pub(crate) fn with_retry_after(mut result: TierResult, retry_after: Option<String>) -> TierResult {
    if result.kind == Some("rate_limited") {
        result.retry_after = retry_after.filter(|v| !v.trim().is_empty());
    }
    result
}

/// A reference the run could not fill: the earlier response changed.
pub fn unresolved(req: &PreparedRequest, reason: String) -> TierResult {
    tier_result(
        req,
        Tier::Http,
        Instant::now(),
        Err(Failure::new(true, None, reason)),
    )
}

/// The last failed attempt, when every way of sending that request was
/// refused with a 401 or 403.
pub fn denied_everywhere(results: &[TierResult]) -> Option<&TierResult> {
    let last = results.last().filter(|r| !r.ok)?;
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
    let empty = last.iter().any(|r| r.ok && r.empty);
    if all_ok && shape_changed.is_none() && empty {
    } else if all_ok && shape_changed.is_none() {
        flow.verified_at = Some(now.to_string());
    } else if shape_changed.is_some() || (!all_ok && attempts.iter().any(|r| !r.ok && r.drift)) {
        flow.verified_at = None;
    }
    // Remember the tier that worked, so the next run starts there.
    for r in last.iter().filter(|r| r.ok) {
        for req in flow
            .steps
            .iter_mut()
            .flat_map(|s| s.requests.iter_mut())
            .filter(|req| req.id == r.id)
        {
            req.tier = (r.tier != Tier::Http).then_some(r.tier);
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
        "verified": all_ok && shape_changed.is_none() && !empty,
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
