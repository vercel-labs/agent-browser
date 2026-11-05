//! The replay tier: the recorded commands run once, requests read from what they sent.

use super::*;

/// Tier 3: the recorded commands with the run's params swapped in. A param
/// whose recorded value no command contains cannot be replayed: the browser
/// would answer with the recorded value instead.
pub fn replay_commands(flow: &Flow, cmd_params: &[(String, String)]) -> Result<Vec<Value>, String> {
    let given = |name: &str| cmd_params.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    // The params this run changes, as (name, recorded value): wherever the
    // recorded value stands in a command it becomes the param's placeholder.
    let swaps: Vec<(String, String)> = flow
        .params
        .iter()
        .filter_map(|p| {
            let example = p.example.as_deref()?;
            let value = given(&p.name)?;
            (!example.is_empty() && example != value.as_str())
                .then(|| (p.name.clone(), example.to_string()))
        })
        .collect();
    // Every placeholder takes the run's value, or the recorded example when
    // the run gives none.
    let values: HashMap<String, String> = flow
        .params
        .iter()
        .filter_map(|p| {
            let value = given(&p.name).cloned().or_else(|| p.example.clone())?;
            Some((p.name.clone(), value))
        })
        .collect();
    let opens_page = flow.steps.iter().any(|s| s.command["action"] == "navigate");
    // A flow recorded on an already open page starts from that page, so its
    // start URL is part of what the replay sends.
    let start = (!opens_page).then(|| json!({ "action": "navigate", "url": flow.start_url }));
    let mut recorded = serde_json::to_string(
        &flow
            .steps
            .iter()
            .map(|s| s.command.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();
    if start.is_some() {
        recorded.push(' ');
        recorded.push_str(&flow.start_url);
    }
    for p in &flow.params {
        if given(&p.name).is_none()
            && p.example.is_none()
            && recorded.contains(&format!("{{{{{}}}}}", p.name))
        {
            return Err(format!(
                "missing --param {}: the recording did not keep the typed value",
                p.name
            ));
        }
    }
    let in_commands = |text: &str| {
        recorded.contains(text) || recorded.contains(urlencoding::encode(text).as_ref())
    };
    if let Some((name, example)) = swaps.iter().find(|(name, example)| {
        let named = recorded.contains(&format!("{{{{{}}}}}", name));
        !named && !in_commands(example)
    }) {
        return Err(format!(
            "param '{}' is not in the recorded commands, so a replay would return the recorded value {}",
            name, example
        ));
    }
    if flow.steps.iter().any(|s| s.command["redacted"] == true) {
        return Err(
            "the recording typed a secret, which is not stored, so the commands cannot be replayed"
                .to_string(),
        );
    }
    Ok(start
        .into_iter()
        .chain(
            flow.steps
                .iter()
                .filter(|s| s.ok)
                .map(|s| s.command.clone()),
        )
        .map(|cmd| render_command(&cmd, None, &swaps, &values))
        .collect())
}

/// One command with the run's values in place: every string is read once,
/// the recorded values become placeholders, and each placeholder is filled
/// with the value encoded for where it lands, a URL or plain text.
fn render_command(
    v: &Value,
    key: Option<&str>,
    swaps: &[(String, String)],
    values: &HashMap<String, String>,
) -> Value {
    match v {
        Value::String(text) => {
            let slot = if key.is_some_and(|k| URL_KEYS.contains(&k)) {
                Slot::Url
            } else {
                Slot::Raw
            };
            let templated = template::templatize(text, swaps, slot);
            Value::String(template::bind_known(
                &template::parse(&templated),
                slot,
                values,
            ))
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| render_command(i, key, swaps, values))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, i)| (k.clone(), render_command(i, Some(k), swaps, values)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Commands a replay may send. Anything that runs page script, or that the
/// recorder does not know to be a plain interaction, is left out.
const REPLAY_ACTIONS: &[&str] = &[
    "navigate",
    "click",
    "dblclick",
    "fill",
    "type",
    "press",
    "select",
    "check",
    "uncheck",
    "hover",
    "focus",
    "scroll",
    "scrollintoview",
    "wait",
    "snapshot",
    "getbyrole",
    "getbytext",
    "getbylabel",
    "getbyplaceholder",
    "getbyalttext",
    "getbytitle",
    "getbytestid",
    "keyboard",
    "inserttext",
    "back",
    "forward",
    "reload",
    "tab_new",
    "tab_switch",
];

const SCRIPT_KEYS: &[&str] = &["script", "expression", "function", "fn", "js", "code"];

const URL_KEYS: &[&str] = &[
    "url", "href", "src", "action", "uri", "location", "origin", "target",
];

/// Check one value the way a browser reads it: the URL parser drops tabs,
/// newlines and leading control characters, so a string that parses as an
/// http(s) URL is host-checked whatever it looks like before parsing.
fn check_value(
    policy: &HostPolicy,
    v: &Value,
    key: Option<&str>,
    scripts: &mut bool,
) -> Result<(), String> {
    match v {
        Value::String(text) => {
            let forced = key.is_some_and(|k| URL_KEYS.contains(&k));
            if let Ok(parsed) = url::Url::parse(text) {
                match parsed.scheme() {
                    "http" | "https" => policy.check(text)?,
                    "about" if parsed.as_str() == "about:blank" => {}
                    scheme
                        if forced
                            || matches!(
                                scheme,
                                "ws" | "wss" | "ftp" | "file" | "javascript" | "data" | "blob"
                            ) =>
                    {
                        return Err(format!("'{}' is not an http(s) URL", text.trim()));
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        Value::Array(items) => items
            .iter()
            .try_for_each(|i| check_value(policy, i, key, scripts)),
        Value::Object(map) => {
            for (k, item) in map {
                if SCRIPT_KEYS.contains(&k.as_str()) && item.is_string() {
                    *scripts = true;
                }
                check_value(policy, item, Some(k), scripts)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Check that every command a replay would send is a plain interaction and
/// every URL in it stays on the flow's site.
pub fn check_replay_commands(flow: &Flow, commands: &[Value]) -> Result<(), String> {
    let policy = HostPolicy::of(flow);
    for cmd in commands {
        let action = cmd["action"].as_str().unwrap_or("");
        if !REPLAY_ACTIONS.contains(&action) {
            return Err(format!(
                "the recorded command '{}' is not allowed in a replay",
                action
            ));
        }
        let mut scripts = false;
        check_value(&policy, cmd, None, &mut scripts)?;
        if scripts {
            return Err(format!(
                "the recorded command '{}' carries a script, which a replay does not run",
                action
            ));
        }
    }
    Ok(())
}

/// The entry captured during a replay that stands for a recorded request:
/// same method, host and path, the same recorded query, and for requests with
/// a body the same body or GraphQL operation (one endpoint serves every
/// query). Query parameters the replay added and the recording never had,
/// such as cache-busters, are ignored. An exact URL wins; otherwise exactly
/// one entry must match, so an ambiguous or missing match is an error.
pub fn replay_lookup<'a>(
    entries: &'a [HarEntry],
    req: &PreparedRequest,
) -> Result<&'a HarEntry, String> {
    let key = |u: &str| {
        url::Url::parse(u)
            .ok()
            .map(|u| (u.host_str().unwrap_or("").to_string(), u.path().to_string()))
    };
    let operation = |b: Option<&str>| {
        b.and_then(|b| serde_json::from_str::<Value>(b).ok())
            .and_then(|v| v["operationName"].as_str().map(String::from))
    };
    let want = key(&req.url);
    let want_op = operation(req.body.as_deref());
    let same_body = |e: &HarEntry| match &req.body {
        None => true,
        // A body that still has references to fill cannot be compared yet.
        Some(b) if b.contains("{{@") => true,
        Some(b) => {
            let same_json = || {
                let (a, c) = (
                    serde_json::from_str::<Value>(b).ok()?,
                    serde_json::from_str::<Value>(e.post_data.as_deref()?).ok()?,
                );
                Some(a == c)
            };
            e.post_data.as_deref() == Some(b.as_str())
                || same_json() == Some(true)
                || (want_op.is_some() && operation(e.post_data.as_deref()) == want_op)
        }
    };
    let candidates: Vec<&HarEntry> = entries
        .iter()
        .rev()
        .filter(|e| e.method == req.method && key(&e.url) == want && same_body(e))
        .collect();
    if let Some(exact) = candidates.iter().find(|e| e.url == req.url) {
        return Ok(exact);
    }
    let wanted_query: Vec<(String, String)> = url::Url::parse(&req.url)
        .map(|u| {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let has_query = |e: &HarEntry| {
        let pairs: Vec<(String, String)> = url::Url::parse(&e.url)
            .map(|u| {
                u.query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        wanted_query.iter().all(|(k, v)| {
            pairs
                .iter()
                .any(|(pk, pv)| pk == k && (v.contains("{{") || pv == v))
        })
    };
    let matching: Vec<&HarEntry> = candidates.into_iter().filter(|e| has_query(e)).collect();
    match matching.as_slice() {
        [] => Err(format!(
            "no replayed request matches {} {} with its recorded query",
            req.method, req.url
        )),
        [one] => Ok(one),
        _ => Err(format!(
            "{} {} matches several replayed requests, so none is used",
            req.method, req.url
        )),
    }
}

pub fn replay_result(
    req: &PreparedRequest,
    started: Instant,
    entry: Option<&HarEntry>,
) -> TierResult {
    // A write the replay may have sent is never drift: without a readable
    // answer its outcome is unknown, and the agent must not repeat it.
    let outcome = match entry {
        None if req.write => Err(Failure::unknown_outcome(
            "the replay may have sent the write, but it was not captured".to_string(),
        )),
        None => Err(Failure::new(
            true,
            None,
            "replaying the recorded commands did not send this request".to_string(),
        )),
        Some(e) => match (e.status, e.response_body.as_deref()) {
            (Some(status), Some(body)) if !e.response_body_base64 => {
                Ok((status as u16, body.to_string()))
            }
            _ if req.write => Err(Failure::unknown_outcome(
                "the replayed write has no readable answer".to_string(),
            )),
            (Some(status), _) => Err(Failure::new(
                true,
                Some(status as u16),
                "the replayed request has no readable body".to_string(),
            )),
            _ => Err(Failure::new(
                true,
                None,
                "the replayed request got no response".to_string(),
            )),
        },
    };
    let retry_after = entry.and_then(|e| {
        e.response_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
            .map(|(_, v)| v.clone())
    });
    with_retry_after(
        tier_result(req, Tier::Replay, started, outcome),
        retry_after,
    )
}

/// What the single replay of a run captured.
pub struct ReplayRun {
    entries: Vec<HarEntry>,
    failure: Option<String>,
}

/// The replay tier over a browser. The recording runs at most once per run;
/// every request that falls back to it reads from that capture.
pub async fn send(
    flow: &Flow,
    params: &[(String, String)],
    browser: &mut dyn Browser,
    run: &mut Option<ReplayRun>,
    req: &PreparedRequest,
    ctx: &SendCtx<'_>,
) -> TierResult {
    let started = Instant::now();
    let fail = |f: Failure| TierResult::fail(req, Tier::Replay, started, f);
    // A write that was already sent is only read back, never sent again.
    if req.write && ctx.ledger.already_sent(&req.id) {
        return match run.as_ref().map(|r| replay_lookup(&r.entries, req)) {
            Some(Ok(entry)) => replay_result(req, started, Some(entry)),
            Some(Err(reason)) => fail(Failure::unknown_outcome(format!(
                "the write was already sent in this run and its answer cannot be read back: {}",
                reason
            ))),
            None => fail(Failure::unknown_outcome(
                "the write was already sent in this run".to_string(),
            )),
        };
    }
    if run.is_none() {
        // A replay repeats every recorded command, so it also sends the
        // writes no kept request covers; each must be approved.
        for w in flow.replay_only_writes() {
            if let Err(reason) = browser.write_approved(w) {
                return fail(Failure::unavailable(format!(
                    "replaying the recorded commands would also send {} {} ({}): {}",
                    w.method, w.url, w.id, reason
                )));
            }
        }
        // A write another tier already sent would go out a second time.
        if let Some(id) = ctx.ledger.first_repeated_by_replay(flow) {
            return fail(Failure::unavailable(format!(
                "replaying the recorded commands would send write {} again; it was already sent in this run",
                id
            )));
        }
        match capture(flow, params, browser, ctx.wanted, started).await {
            Ok(captured) => *run = Some(captured),
            Err(Ok(reason)) => return fail(Failure::unavailable(reason)),
            Err(Err(reason)) => return fail(Failure::drift(reason)),
        }
    }
    let Some(captured) = run.as_ref() else {
        return fail(Failure::drift("the replay did not run".to_string()));
    };
    // The replay ran the commands that send a write, even when a later
    // command failed: the capture answers it, and no capture does not say
    // the write was not sent.
    if req.write {
        return match replay_lookup(&captured.entries, req) {
            Ok(entry) => replay_result(req, started, Some(entry)),
            Err(reason) => fail(Failure::unknown_outcome(format!(
                "the replay may have sent the write, but no answer for it was captured: {}{}",
                reason,
                captured
                    .failure
                    .as_deref()
                    .map(|f| format!(" ({})", f))
                    .unwrap_or_default()
            ))),
        };
    }
    if let Some(reason) = captured.failure.clone() {
        return fail(Failure::drift(reason));
    }
    match replay_lookup(&captured.entries, req) {
        Ok(entry) => replay_result(req, started, Some(entry)),
        Err(reason) => fail(Failure::drift(reason)),
    }
}

/// Run the recorded commands once and capture the requests they send.
/// `Err(Ok(reason))` means the replay cannot run at all, `Err(Err(reason))`
/// that it could not start.
async fn capture(
    flow: &Flow,
    params: &[(String, String)],
    browser: &mut dyn Browser,
    wanted: &[&PreparedRequest],
    started: Instant,
) -> Result<ReplayRun, Result<String, String>> {
    browser.ensure_browser().await.map_err(Err)?;
    let mark = browser.begin_capture().await.map_err(Err)?;
    let commands = match replay_commands(flow, params) {
        Ok(c) => c,
        Err(e) => {
            browser.end_capture().await;
            return Err(Ok(e));
        }
    };
    if let Err(reason) = check_replay_commands(flow, &commands) {
        browser.end_capture().await;
        return Err(Ok(reason));
    }
    let mut failed = None;
    for (i, c) in commands.into_iter().enumerate() {
        let mut c = c;
        c["id"] = json!(format!("flow-replay-{}", i + 1));
        // A page opened by the replay may still be rendering the element.
        let resp = loop {
            let resp = browser.run(&c).await;
            let missing = resp
                .get("error")
                .and_then(|e| e.as_str())
                .is_some_and(|e| e.contains("No element found") || e.contains("Element not found"));
            if !missing || Instant::now() >= started + Duration::from_secs(20) {
                break resp;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        // The next command may depend on what this one fetched (a token the
        // page keeps), so let its requests finish first.
        loop {
            let seen = browser.captured_since(mark).await;
            if seen.iter().all(|e| e.loading_finished_timestamp.is_some())
                || Instant::now() >= started + Duration::from_secs(20)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // The replay stops as soon as the tab leaves the flow's site.
        if let Some(url) = browser.tab_url().await.filter(|u| u.starts_with("http")) {
            if let Err(reason) = HostPolicy::of(flow).check(&url) {
                failed = Some(format!(
                    "replay stopped after command {}: {}",
                    i + 1,
                    reason
                ));
                break;
            }
        }
        if resp.get("success") != Some(&json!(true)) {
            failed = Some(format!(
                "replaying command {} failed: {}",
                i + 1,
                resp.get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown error")
            ));
            break;
        }
    }
    // Bodies arrive on later event drains; wait within the request timeout
    // until every request that may fall back has its answer.
    let deadline = started + Duration::from_secs(20);
    while failed.is_none() {
        let slice = browser.captured_since(mark).await;
        let ready = |w: &&PreparedRequest| {
            replay_lookup(&slice, w).is_ok_and(|e| {
                e.response_body.is_some() || e.status.is_some_and(|s| !(200..300).contains(&s))
            })
        };
        if wanted.iter().all(ready) || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let entries = browser.captured_since(mark).await;
    browser.end_capture().await;
    Ok(ReplayRun {
        entries,
        failure: failed,
    })
}
