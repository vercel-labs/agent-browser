//! An in-memory sender for tests: scripted answers per tier, and a log of
//! what was sent, so the run loop needs no browser or network.

use std::collections::HashMap;

use async_trait::async_trait;

use super::*;

/// What a tier answers for a request id.
#[derive(Clone)]
pub struct Scripted {
    pub status: Option<u16>,
    /// The response text, as the tier would read it.
    pub text: String,
    pub stop: Option<Stop>,
    pub drift: bool,
}

impl Scripted {
    pub fn json(status: u16, text: &str) -> Scripted {
        Scripted {
            status: Some(status),
            text: text.to_string(),
            stop: None,
            drift: false,
        }
    }

    pub fn unknown() -> Scripted {
        Scripted {
            status: None,
            text: String::new(),
            stop: Some(Stop::OutcomeUnknown),
            drift: false,
        }
    }
}

#[derive(Default)]
pub struct MemorySenders {
    answers: HashMap<(Tier, String), Scripted>,
    /// Every send, in order.
    pub sent: Vec<(Tier, String)>,
    replayed: bool,
}

impl MemorySenders {
    pub fn answer(mut self, tier: Tier, id: &str, answer: Scripted) -> Self {
        self.answers.insert((tier, id.to_string()), answer);
        self
    }

    pub fn times(&self, id: &str) -> usize {
        self.sent.iter().filter(|(_, i)| i == id).count()
    }
}

#[allow(clippy::double_must_use)]
#[async_trait]
impl Senders for MemorySenders {
    async fn send(&mut self, tier: Tier, req: &PreparedRequest, _ctx: &SendCtx<'_>) -> TierResult {
        self.sent.push((tier, req.id.clone()));
        if tier == Tier::Replay {
            self.replayed = true;
        }
        let started = Instant::now();
        let Some(answer) = self.answers.get(&(tier, req.id.clone())).cloned() else {
            return tier_result(
                req,
                tier,
                started,
                Err(Failure::new(true, None, "no scripted answer".to_string())),
            );
        };
        if let Some(stop) = answer.stop {
            return tier_result(
                req,
                tier,
                started,
                Err(Failure {
                    status: answer.status,
                    reason: stop.code().to_string(),
                    drift: answer.drift,
                    shape_changed: false,
                    stop: Some(stop),
                }),
            );
        }
        tier_result(
            req,
            tier,
            started,
            Ok((answer.status.unwrap_or(0), answer.text)),
        )
    }

    fn replay_ran(&self) -> bool {
        self.replayed
    }
}
