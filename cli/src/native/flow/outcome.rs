//! What one request of a run came back with, and how a run's results are judged.

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

pub struct RequestResult {
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
    /// The response says the run cannot go on.
    pub stop: Option<Stop>,
    pub retry_after: Option<String>,
}

impl RequestResult {
    /// A request that did not produce an answer.
    pub fn fail(req: &PreparedRequest, started: Instant, f: Failure) -> RequestResult {
        let ms = started.elapsed().as_millis();
        RequestResult::failed(req, ms, f)
    }

    fn failed(req: &PreparedRequest, ms: u128, f: Failure) -> RequestResult {
        RequestResult {
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
    /// The request failed in a way that says the site changed.
    pub fn drift(reason: String) -> Failure {
        Failure::new(true, None, reason)
    }

    /// A write was sent and no answer came back: it is not sent again.
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

    /// A response that ends the run, and does not mean the site changed.
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

/// Judge a response: blocked, rejected input, changed shape, or data.
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

pub fn request_result(
    req: &PreparedRequest,
    started: Instant,
    outcome: Result<(u16, String), Failure>,
) -> RequestResult {
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
        Ok((status, (body, empty))) => RequestResult {
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
            stop: None,
            retry_after: None,
        },
        Err(f) => RequestResult::failed(req, ms, f),
    }
}

pub(crate) fn with_retry_after(
    mut result: RequestResult,
    retry_after: Option<String>,
) -> RequestResult {
    if result.stop == Some(Stop::RateLimited) {
        result.retry_after = retry_after.filter(|v| !v.trim().is_empty());
    }
    result
}

/// Fold the results of a run into its response. The flow is verified only
/// when every request came back with the recorded shape.
pub fn finish_run(
    flow: &mut Flow,
    results: &[RequestResult],
    now: &str,
    total_ms: u128,
    shape: &Shape,
) -> Value {
    let all_ok = !results.is_empty() && results.iter().all(|r| r.ok);
    let shape_changed = results.iter().find(|r| r.shape_changed);
    // An empty answer cannot confirm the recorded lists still exist, so it
    // leaves the verified state as it was.
    let any_empty = results.iter().any(|r| r.ok && r.empty);
    // Only the answer itself reads as no results; an empty dependency does
    // not replace a response that has data.
    let empty = results
        .iter()
        .any(|r| r.id == flow.output && r.ok && r.empty);
    if all_ok && shape_changed.is_none() && any_empty {
    } else if all_ok && shape_changed.is_none() {
        flow.verified_at = Some(now.to_string());
    } else if shape_changed.is_some() || (!all_ok && results.iter().any(|r| !r.ok && r.drift)) {
        flow.verified_at = None;
    }
    let output = results
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
    let steps: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "step": r.step,
                "status": r.status,
                "ms": r.ms,
                "error": r.reason,
            })
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
    if let Some(r) = shape_changed {
        response["drift"] = json!({ "step": r.step, "reason": r.reason });
    }
    response
}

/// How a run that did not return data ended.
#[derive(Debug, PartialEq)]
pub enum RunOutcome {
    /// A response that ends the run.
    Stopped {
        stop: Stop,
        step: usize,
        status: Option<u16>,
        reason: String,
        retry_after: Option<String>,
    },
    /// The request failed in a way that says the site changed.
    Drift { step: usize, reason: String },
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
    pub fn stopped(results: &[RequestResult]) -> Option<RunOutcome> {
        let (stop, result) = results.iter().rev().find_map(|r| r.stop.map(|s| (s, r)))?;
        Some(RunOutcome::Stopped {
            stop,
            step: result.step,
            status: result.status,
            reason: result.reason.clone().unwrap_or_default(),
            retry_after: result.retry_after.clone(),
        })
    }

    /// Why a run that finished without data failed.
    pub fn failed(results: &[RequestResult], flow: &Flow) -> RunOutcome {
        let failure = results.iter().rev().find(|r| !r.ok);
        let step = failure.map(|r| r.step).unwrap_or(0);
        if let Some(r) = results.iter().find(|r| r.step == step && r.drift) {
            return RunOutcome::Drift {
                step,
                reason: r.reason.clone().unwrap_or_default(),
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
                reason,
                retry_after,
                ..
            } => {
                let wait = retry_after
                    .as_deref()
                    .map(|w| format!("; retry after {}", w))
                    .unwrap_or_default();
                format!(
                    "{}: flow '{}' step {}: {}{}. {}",
                    stop.code(),
                    name,
                    step,
                    reason,
                    wait,
                    stop.advice()
                )
            }
            RunOutcome::Drift { step, reason } => format!(
                "flow '{}' failed at step {}: {}. The flow is now marked unverified; drive the browser for this task instead",
                name, step, reason
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
