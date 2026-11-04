//! An in-memory sender for tests: scripted answers per request id, and a log
//! of what was sent, so the run loop needs no browser or network.

use std::collections::HashMap;

use async_trait::async_trait;

use super::*;

/// What a request answers.
#[derive(Clone)]
pub struct Scripted {
    pub status: Option<u16>,
    /// The response text, as the request would read it.
    pub text: String,
    pub stop: Option<Stop>,
}

impl Scripted {
    pub fn json(status: u16, text: &str) -> Scripted {
        Scripted {
            status: Some(status),
            text: text.to_string(),
            stop: None,
        }
    }

    pub fn unknown() -> Scripted {
        Scripted {
            status: None,
            text: String::new(),
            stop: Some(Stop::OutcomeUnknown),
        }
    }
}

#[derive(Default)]
pub struct MemorySender {
    answers: HashMap<String, Scripted>,
    /// Every send, in order.
    pub sent: Vec<String>,
}

impl MemorySender {
    pub fn answer(mut self, id: &str, answer: Scripted) -> Self {
        self.answers.insert(id.to_string(), answer);
        self
    }

    pub fn times(&self, id: &str) -> usize {
        self.sent.iter().filter(|i| *i == id).count()
    }
}

#[allow(clippy::double_must_use)]
#[async_trait]
impl Sender for MemorySender {
    async fn send(&mut self, req: &PreparedRequest) -> RequestResult {
        self.sent.push(req.id.clone());
        let started = Instant::now();
        let Some(answer) = self.answers.get(&req.id).cloned() else {
            return RequestResult::fail(req, started, Failure::drift("no scripted answer".into()));
        };
        match answer.stop {
            Some(Stop::OutcomeUnknown) => RequestResult::fail(
                req,
                started,
                Failure::unknown_outcome("no answer".to_string()),
            ),
            Some(stop) => RequestResult::fail(
                req,
                started,
                Failure::stop(stop, answer.status.unwrap_or(0), stop.code().to_string()),
            ),
            None => request_result(req, started, Ok((answer.status.unwrap_or(0), answer.text))),
        }
    }
}
