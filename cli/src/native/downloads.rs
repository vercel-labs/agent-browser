//! Downloads observed on the attached browser, so `wait --download` can
//! claim a download that finished before the command arrived (#561).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadState {
    InProgress,
    Completed,
    Canceled,
}

#[derive(Debug, Clone)]
pub struct DownloadRecord {
    pub guid: String,
    pub state: DownloadState,
    /// Where the browser saved the file. Chrome only reports it on the
    /// completed `Browser.downloadProgress` event when `eventsEnabled` is set.
    pub file_path: Option<String>,
    /// Set once any `Browser.*` event arrives for this download. From then on
    /// only `Browser.downloadProgress` finishes it: `Page.downloadProgress`
    /// reports completion first, before the path is known.
    browser_events: bool,
    claimed: bool,
}

/// Downloads in the order they began. Records live as long as the browser
/// instance; `clear` runs whenever a new browser is attached.
#[derive(Debug, Default)]
pub struct DownloadLedger {
    records: Vec<DownloadRecord>,
}

impl DownloadLedger {
    pub fn clear(&mut self) {
        self.records.clear();
    }

    /// Record a download event. `Page.*` and `Browser.*` events for one
    /// download share a guid; only `Browser.downloadProgress` carries the file
    /// path, and only when `eventsEnabled` is set. Provider connections that
    /// never enable it still report completion through `Page.*`.
    /// Returns false for any other method.
    pub fn observe(&mut self, method: &str, params: &Value) -> bool {
        let Some(guid) = params.get("guid").and_then(|v| v.as_str()) else {
            return false;
        };
        match method {
            "Browser.downloadWillBegin" | "Page.downloadWillBegin" => {
                let record = self.record_mut(guid);
                record.browser_events |= method.starts_with("Browser.");
                true
            }
            "Browser.downloadProgress" | "Page.downloadProgress" => {
                let state = params.get("state").and_then(|v| v.as_str());
                let file_path = params.get("filePath").and_then(|v| v.as_str());
                let from_browser = method.starts_with("Browser.");
                let record = self.record_mut(guid);
                record.browser_events |= from_browser;
                if record.browser_events && !from_browser {
                    return true;
                }
                match state {
                    Some("completed") => record.state = DownloadState::Completed,
                    Some("canceled") => record.state = DownloadState::Canceled,
                    _ => {}
                }
                if let Some(path) = file_path {
                    record.file_path = Some(path.to_string());
                }
                true
            }
            _ => false,
        }
    }

    /// Claim the oldest finished download that no earlier wait has claimed.
    pub fn claim_finished(&mut self) -> Option<DownloadRecord> {
        let record = self
            .records
            .iter_mut()
            .find(|r| !r.claimed && r.state != DownloadState::InProgress)?;
        record.claimed = true;
        Some(record.clone())
    }

    /// Mark a download as handled elsewhere, so `wait --download` never
    /// claims it. `download <selector> <path>` owns its own file.
    pub fn mark_claimed(&mut self, guid: &str) {
        self.record_mut(guid).claimed = true;
    }

    fn record_mut(&mut self, guid: &str) -> &mut DownloadRecord {
        if let Some(index) = self.records.iter().position(|r| r.guid == guid) {
            return &mut self.records[index];
        }
        self.records.push(DownloadRecord {
            guid: guid.to_string(),
            state: DownloadState::InProgress,
            file_path: None,
            browser_events: false,
            claimed: false,
        });
        self.records.last_mut().expect("record was just pushed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn completed(ledger: &mut DownloadLedger, guid: &str, path: &str) {
        ledger.observe("Browser.downloadWillBegin", &json!({ "guid": guid }));
        ledger.observe(
            "Browser.downloadProgress",
            &json!({ "guid": guid, "state": "completed", "filePath": path }),
        );
    }

    #[test]
    fn claims_a_download_that_finished_before_the_wait() {
        let mut ledger = DownloadLedger::default();
        completed(&mut ledger, "a", "/dl/a.bin");
        let record = ledger.claim_finished().expect("finished download");
        assert_eq!(record.file_path.as_deref(), Some("/dl/a.bin"));
        assert!(
            ledger.claim_finished().is_none(),
            "a download is claimed once"
        );
    }

    #[test]
    fn claims_in_the_order_downloads_began() {
        let mut ledger = DownloadLedger::default();
        ledger.observe("Browser.downloadWillBegin", &json!({ "guid": "first" }));
        completed(&mut ledger, "second", "/dl/second.bin");
        ledger.observe(
            "Browser.downloadProgress",
            &json!({ "guid": "first", "state": "completed", "filePath": "/dl/first.bin" }),
        );
        assert_eq!(ledger.claim_finished().unwrap().guid, "first");
        assert_eq!(ledger.claim_finished().unwrap().guid, "second");
    }

    #[test]
    fn skips_downloads_still_in_progress() {
        let mut ledger = DownloadLedger::default();
        ledger.observe("Browser.downloadWillBegin", &json!({ "guid": "slow" }));
        ledger.observe(
            "Browser.downloadProgress",
            &json!({ "guid": "slow", "state": "inProgress" }),
        );
        assert!(ledger.claim_finished().is_none());
    }

    #[test]
    fn reports_canceled_downloads() {
        let mut ledger = DownloadLedger::default();
        ledger.observe(
            "Browser.downloadProgress",
            &json!({ "guid": "x", "state": "canceled" }),
        );
        assert_eq!(
            ledger.claim_finished().unwrap().state,
            DownloadState::Canceled
        );
    }

    #[test]
    fn merges_page_and_browser_events_for_one_download() {
        let mut ledger = DownloadLedger::default();
        ledger.observe("Page.downloadWillBegin", &json!({ "guid": "g" }));
        ledger.observe("Browser.downloadWillBegin", &json!({ "guid": "g" }));
        ledger.observe(
            "Page.downloadProgress",
            &json!({ "guid": "g", "state": "completed" }),
        );
        ledger.observe(
            "Browser.downloadProgress",
            &json!({ "guid": "g", "state": "completed", "filePath": "/dl/g.bin" }),
        );
        assert_eq!(
            ledger.claim_finished().unwrap().file_path.as_deref(),
            Some("/dl/g.bin")
        );
        assert!(ledger.claim_finished().is_none());
    }

    #[test]
    fn waits_for_the_browser_event_that_carries_the_path() {
        let mut ledger = DownloadLedger::default();
        ledger.observe("Page.downloadWillBegin", &json!({ "guid": "g" }));
        ledger.observe("Browser.downloadWillBegin", &json!({ "guid": "g" }));
        ledger.observe(
            "Page.downloadProgress",
            &json!({ "guid": "g", "state": "completed" }),
        );
        assert!(
            ledger.claim_finished().is_none(),
            "Page completion arrives before the path is known"
        );
    }

    #[test]
    fn page_events_alone_finish_a_download_without_a_path() {
        let mut ledger = DownloadLedger::default();
        ledger.observe("Page.downloadWillBegin", &json!({ "guid": "p" }));
        ledger.observe(
            "Page.downloadProgress",
            &json!({ "guid": "p", "state": "completed" }),
        );
        let record = ledger.claim_finished().unwrap();
        assert_eq!(record.state, DownloadState::Completed);
        assert_eq!(record.file_path, None);
    }

    #[test]
    fn never_claims_a_download_marked_as_handled() {
        let mut ledger = DownloadLedger::default();
        ledger.mark_claimed("owned");
        completed(&mut ledger, "owned", "/dl/owned.bin");
        assert!(ledger.claim_finished().is_none());
    }

    #[test]
    fn ignores_other_events_and_clears() {
        let mut ledger = DownloadLedger::default();
        assert!(!ledger.observe("Page.frameNavigated", &json!({ "guid": "p" })));
        completed(&mut ledger, "a", "/dl/a.bin");
        ledger.clear();
        assert!(ledger.claim_finished().is_none());
    }
}
