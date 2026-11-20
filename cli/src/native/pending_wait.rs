//! Waits that poll without holding the daemon's state lock.
//!
//! Every command runs under the daemon's single state lock. A wait-family
//! command (`wait`, `waitforurl`, `waitforloadstate`, `waitforfunction`,
//! `waitfordownload`) used to poll under that lock for its whole timeout, so
//! every other command queued behind it. Now the command does its setup
//! under the lock, hands the daemon a [`WaitPlan`]'s poll to drive with the
//! lock released, and finishes under the lock again once the poll settles.
//!
//! The daemon drives each poll against a [`Cancel`] token, fired when the
//! client that issued the wait disconnects, and for every pending wait when
//! `close` arrives or the daemon receives a shutdown signal.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// Why a wait stopped before its condition held or its timeout elapsed.
pub const CLIENT_DISCONNECTED: &str = "the client that issued it disconnected";
pub const SESSION_CLOSED: &str = "the session was closed";
pub const DAEMON_SHUTTING_DOWN: &str = "the daemon is shutting down";
pub const BROWSER_CHANGED: &str = "the browser changed while it waited";

/// The part of a wait that runs without the state lock. It owns what it
/// needs (a shared CDP client, session ids), so it never borrows the state.
pub type WaitPoll = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

const CANCELLED_PREFIX: &str = "Wait cancelled: ";

/// The error a cancelled wait reports.
pub fn cancelled_error(reason: &str) -> String {
    format!("{}{}", CANCELLED_PREFIX, reason)
}

/// Whether an error is a wait's cancellation rather than its own failure.
pub fn is_cancelled(error: &str) -> bool {
    error.starts_with(CANCELLED_PREFIX)
}

/// A one-shot cancellation signal with a reason. Cloning shares the signal.
#[derive(Clone, Default)]
pub struct Cancel(Arc<CancelInner>);

#[derive(Default)]
struct CancelInner {
    reason: Mutex<Option<&'static str>>,
    notify: Notify,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fire the signal. The first reason wins.
    pub fn cancel(&self, reason: &'static str) {
        let mut slot = self.0.reason.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(reason);
        }
        drop(slot);
        self.0.notify.notify_waiters();
    }

    pub fn reason(&self) -> Option<&'static str> {
        *self.0.reason.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Resolves with the reason once the signal fires.
    pub async fn cancelled(&self) -> &'static str {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            // Register before checking so a cancel between the check and the
            // await is not missed.
            notified.as_mut().enable();
            if let Some(reason) = self.reason() {
                return reason;
            }
            notified.await;
        }
    }

    /// Drive `poll` until it settles or this signal fires.
    pub async fn run(&self, poll: WaitPoll) -> Result<(), String> {
        tokio::select! {
            biased;
            reason = self.cancelled() => Err(cancelled_error(reason)),
            result = poll => result,
        }
    }
}

/// The daemon's registry of waits now polling without the state lock.
#[derive(Default)]
pub struct PendingWaits {
    pending: Mutex<Vec<(u64, Cancel)>>,
    next_id: AtomicU64,
}

impl PendingWaits {
    pub fn new() -> Self {
        Self::default()
    }

    /// Track a polling wait until the returned guard drops.
    pub fn register(self: &Arc<Self>, cancel: &Cancel) -> Registration {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock().push((id, cancel.clone()));
        Registration {
            waits: self.clone(),
            id,
        }
    }

    /// Cancel every polling wait, for `close` and daemon shutdown.
    pub fn cancel_all(&self, reason: &'static str) {
        for (_, cancel) in self.lock().iter() {
            cancel.cancel(reason);
        }
    }

    /// Whether a wait is polling. The session is in use even though no
    /// command holds the state lock.
    pub fn any(&self) -> bool {
        !self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(u64, Cancel)>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Registration {
    waits: Arc<PendingWaits>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.waits.lock().retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancel_wakes_a_waiter_and_keeps_the_first_reason() {
        let cancel = Cancel::new();
        let waiter = tokio::spawn({
            let cancel = cancel.clone();
            async move { cancel.cancelled().await }
        });
        tokio::task::yield_now().await;
        cancel.cancel(CLIENT_DISCONNECTED);
        cancel.cancel(SESSION_CLOSED);
        let reason = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter woke")
            .unwrap();
        assert_eq!(reason, CLIENT_DISCONNECTED);
        // A late waiter sees the signal without a new notification.
        assert_eq!(cancel.cancelled().await, CLIENT_DISCONNECTED);
    }

    #[tokio::test]
    async fn run_drops_the_poll_when_cancelled() {
        let cancel = Cancel::new();
        let poll: WaitPoll = Box::pin(std::future::pending());
        let run = tokio::spawn({
            let cancel = cancel.clone();
            async move { cancel.run(poll).await }
        });
        tokio::task::yield_now().await;
        cancel.cancel(SESSION_CLOSED);
        let result = tokio::time::timeout(Duration::from_secs(1), run)
            .await
            .expect("run returned")
            .unwrap();
        assert_eq!(result, Err(cancelled_error(SESSION_CLOSED)));
    }

    #[test]
    fn registry_cancels_registered_waits_until_they_drop() {
        let waits = Arc::new(PendingWaits::new());
        assert!(!waits.any());
        let first = Cancel::new();
        let second = Cancel::new();
        let first_registration = waits.register(&first);
        let second_registration = waits.register(&second);
        assert!(waits.any());

        drop(second_registration);
        waits.cancel_all(SESSION_CLOSED);
        assert_eq!(first.reason(), Some(SESSION_CLOSED));
        assert_eq!(second.reason(), None);

        drop(first_registration);
        assert!(!waits.any());
    }
}
