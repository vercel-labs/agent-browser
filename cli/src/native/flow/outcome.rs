//! What a tier returned for one request, and how a run's results are judged.

use super::*;

/// Why a run ends without trying anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// A 401 or 403: the session is signed out or lacks access.
    AuthExpired,
    /// A 401 or 403 for a request that needed a credential the file cannot hold.
    CredentialNotSendable,
    /// A 429.
    RateLimited,
    /// A bot challenge page instead of data.
    Blocked,
    /// A write was sent and no answer came back.
    OutcomeUnknown,
}

impl Stop {
    /// The machine-readable code on the error response.
    pub fn code(self) -> &'static str {
        match self {
            Stop::AuthExpired => "auth_expired",
            Stop::CredentialNotSendable => "credential_not_sendable",
            Stop::RateLimited => "rate_limited",
            Stop::Blocked => "blocked",
            Stop::OutcomeUnknown => "outcome_unknown",
        }
    }

    fn advice(self) -> &'static str {
        match self {
            Stop::AuthExpired => "Sign in again in the browser, then rerun",
            Stop::CredentialNotSendable => {
                "The flow cannot send the credential this request needs; use the page UI for this task"
            }
            Stop::RateLimited => "Wait before rerunning",
            Stop::OutcomeUnknown => "Check the site before rerunning; the write is not sent again",
            Stop::Blocked => "Continue in the UI or let the user clear the challenge",
        }
    }
}

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
}

impl Failure {
    /// The way failed in a way another way may not: try the next tier.
    pub fn drift(reason: String) -> Failure {
        Failure::new(true, None, reason)
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
        }
    }

    pub(crate) fn new(drift: bool, status: Option<u16>, reason: String) -> Failure {
        Failure {
            status,
            reason,
            drift,
            shape_changed: false,
            stop: None,
        }
    }

    /// A response that ends the run: another tier would get the same answer,
    /// and it does not mean the site changed.
    pub(crate) fn stop(stop: Stop, status: u16, reason: String) -> Failure {
        Failure {
            status: Some(status),
            reason,
            drift: false,
            shape_changed: false,
            stop: Some(stop),
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
            Stop::Blocked,
            status,
            "bot challenge page instead of data".to_string(),
        ));
    }
    match status {
        401 | 403 if req.expect.stripped_auth => {
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
            return Err(Failure::stop(
                Stop::AuthExpired,
                status,
                format!("HTTP {}: the session is signed out or lacks access", status),
            ))
        }
        429 => {
            return Err(Failure::stop(
                Stop::RateLimited,
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
            stop: None,
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
            stop: None,
            retry_after: None,
        },
        Err(mut f) => {
            // The recording sent credentials a run cannot copy, so a 401 or
            // 403 may only mean this tier lacks them; the next tier sends
            // them from the page.
            if f.stop == Some(Stop::CredentialNotSendable) && tier != Tier::Replay {
                f.stop = None;
                f.drift = true;
            }
            TierResult::failed(req, tier, ms, f)
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
    Drift { step: usize, tried: String },
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
            .or_else(|| denied_everywhere(results).map(|r| (Stop::CredentialNotSendable, r)))?;
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
        let failure = results.iter().rev().find(|r| !r.ok);
        let step = failure.map(|r| r.step).unwrap_or(0);
        if results.iter().any(|r| r.step == step && r.drift) {
            let tried = results
                .iter()
                .filter(|r| r.step == step)
                .map(|r| format!("{}: {}", r.tier.name(), r.reason.as_deref().unwrap_or("ok")))
                .collect::<Vec<_>>()
                .join("; ");
            return RunOutcome::Drift { step, tried };
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
            RunOutcome::Drift { step, tried } => format!(
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
            _ => None,
        }
    }
}
