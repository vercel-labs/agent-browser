//! Deciding which tier sends a request next, and the writes already sent.

use async_trait::async_trait;

use super::*;

pub(crate) fn is_accepted(status: Option<u16>) -> bool {
    matches!(status, Some(200..=399))
}

/// The writes a run has sent, by any tier. A write goes out once per run.
#[derive(Default)]
pub struct WriteLedger {
    sent: std::collections::HashSet<String>,
    replayed: bool,
}

impl WriteLedger {
    /// Note a write's answer: only an accepted answer or an unknown outcome
    /// means it may have been applied; a 4xx rejection does not.
    pub fn record(&mut self, req: &PreparedRequest, result: &TierResult) {
        if req.write && (is_accepted(result.status) || result.kind == Some("outcome_unknown")) {
            self.sent.insert(req.id.clone());
        }
    }

    /// The replay repeated every recorded command, writes included.
    pub fn replay_ran(&mut self, flow: &Flow) {
        self.replayed = true;
        self.sent.extend(all_write_ids(flow));
    }

    pub fn already_sent(&self, id: &str) -> bool {
        self.sent.contains(id)
    }

    /// A write the replay would repeat that this run already sent.
    pub fn first_repeated_by_replay(&self, flow: &Flow) -> Option<String> {
        all_write_ids(flow)
            .into_iter()
            .find(|id| self.sent.contains(id))
    }
}

/// The tier to try next for a request, given the attempts made so far, or
/// `None` when the request is settled. A write that was already sent is only
/// answered from the replay's capture, never sent again.
pub fn next_tier(
    flow: &Flow,
    req: &PreparedRequest,
    attempts: &[TierResult],
    ledger: &WriteLedger,
) -> Option<Tier> {
    if req.write && ledger.already_sent(&req.id) {
        return (attempts.is_empty() && ledger.replayed).then_some(Tier::Replay);
    }
    let order = tiers_for(flow, &req.id);
    let Some(last) = attempts.last() else {
        return order.first().copied();
    };
    // The site answered with something every tier would get again, or the
    // request worked, or a write was accepted.
    if last.ok || last.kind.is_some() || (req.write && is_accepted(last.status)) {
        return None;
    }
    // A rejected input fails the same way on every tier, except a write the
    // site did not accept: its tokens may be stale and the page can send it once.
    if !last.drift && !req.write {
        return None;
    }
    let at = order.iter().position(|t| *t == last.tier)?;
    order.get(at + 1).copied()
}

/// Whether a request starts at the replay tier.
pub fn starts_at_replay(flow: &Flow, id: &str) -> bool {
    tiers_for(flow, id).first() == Some(&Tier::Replay)
}

/// Tiers to try for a request, starting from the one that last worked.
pub(crate) fn tiers_for(flow: &Flow, id: &str) -> Vec<Tier> {
    let preferred = flow
        .request(id)
        .and_then(|r| r.tier)
        .and_then(|t| Tier::ALL.iter().position(|x| *x == t))
        .unwrap_or(0);
    Tier::ALL[preferred..].to_vec()
}

/// What a sender may look at besides the request.
pub struct SendCtx<'a> {
    pub ledger: &'a WriteLedger,
    /// Requests from this one on that may fall back to the replay.
    pub wanted: &'a [&'a PreparedRequest],
}

/// One way of sending a request.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Sender: Send {
    async fn send(&mut self, req: &PreparedRequest, ctx: &SendCtx<'_>) -> TierResult;
}

/// The senders of a run, one per tier, sharing whatever they borrow.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Senders: Send {
    async fn send(&mut self, tier: Tier, req: &PreparedRequest, ctx: &SendCtx<'_>) -> TierResult;
    /// Whether the recorded commands have run in this run.
    fn replay_ran(&self) -> bool;
}

/// What the HTTP and in-page tiers need from the browser session.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Host: Send {
    async fn cookie_header(&mut self, url: &str) -> Option<String>;
    async fn routed_response(&mut self, url: &str) -> Option<RoutedResponse>;
    async fn tab_url(&mut self) -> Option<String>;
    async fn eval(&mut self, script: &str) -> Result<Value, String>;
}

/// What the replay needs: run commands and read back the requests they sent.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait CommandRunner: Host {
    async fn ensure_browser(&mut self) -> Result<(), String>;
    /// Start collecting requests; the mark says where this replay's begin.
    async fn begin_capture(&mut self) -> Result<usize, String>;
    async fn run(&mut self, cmd: &Value) -> Value;
    /// Requests sent since `mark`, after letting pending responses land.
    async fn captured_since(&mut self, mark: usize) -> Vec<HarEntry>;
    async fn end_capture(&mut self);
    /// Whether a write only the replay sends may go out.
    fn write_approved(&self, id: &str) -> Result<(), String>;
}

/// Send each request through its tiers. `check` runs on every request after
/// its references are filled and may refuse the run.
pub async fn run_requests(
    flow: &Flow,
    requests: &[PreparedRequest],
    senders: &mut dyn Senders,
    check: &(dyn Fn(&PreparedRequest) -> Result<(), String> + Sync),
) -> Result<Vec<TierResult>, String> {
    let mut results = Vec::new();
    let mut bodies: HashMap<String, Value> = HashMap::new();
    let mut ledger = WriteLedger::default();
    for (index, prepared) in requests.iter().enumerate() {
        let req = &match prepared.resolve(&bodies) {
            Ok(r) => r,
            Err(e) => {
                results.push(unresolved(prepared, e));
                break;
            }
        };
        check(req)?;
        let wanted: Vec<&PreparedRequest> = requests[index..]
            .iter()
            .filter(|r| r.id == req.id || r.write || starts_at_replay(flow, &r.id))
            .collect();
        let first_attempt = results.len();
        let mut done = false;
        while let Some(tier) = next_tier(flow, req, &results[first_attempt..], &ledger) {
            let result = {
                let ctx = SendCtx {
                    ledger: &ledger,
                    wanted: &wanted,
                };
                senders.send(tier, req, &ctx).await
            };
            if tier == Tier::Replay && senders.replay_ran() {
                ledger.replay_ran(flow);
            }
            if let Some(body) = result.body.as_ref().filter(|_| result.ok) {
                bodies.insert(req.id.clone(), body.clone());
            }
            ledger.record(req, &result);
            done = result.ok;
            results.push(result);
        }
        if !done {
            break;
        }
    }
    Ok(results)
}
