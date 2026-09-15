//! Capture supported successful agent-browser actions as typed browser-test intent.
//!
//! A capture failure does not fail the browser command. Unsafe targets and
//! unsupported mutating actions are omitted and become durable warnings.

mod playwright;
pub mod probe;
mod sidecar;
mod steps;

pub use playwright::{render_playwright_with_report, FormatIssue};
pub use probe::ElementCapture;
pub use steps::{
    action_breaks_navigation_attribution, action_can_navigate, action_is_omitted,
    attach_navigation, bind_popup, can_assert_navigation, can_open_popup, enrich_recent_steps,
    record_action, set_frame_scope, single_element_target, step_scope, ActionContext, ClickKind,
    NavigationKind, PointerKind, Scope, SelectorKind, Step, Target,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::native::browser::PageInfo;

const PRIVATE_CAPTURE_KEY: &str = "__agentBrowserCodegenCapture";

fn warning_counts(state: &CodegenState, steps: &[Step]) -> (usize, usize) {
    let security = usize::from(steps.iter().any(|step| {
        matches!(
            step,
            Step::Change { .. }
                | Step::Fill { .. }
                | Step::SetValue { .. }
                | Step::Type { .. }
                | Step::Select { .. }
                | Step::Upload { .. }
        )
    })) + usize::from(steps.iter().any(Step::has_password));
    let capture = state
        .capture_errors
        .iter()
        .filter(|warning| {
            !warning.starts_with("typed-values-stored:")
                && !warning.starts_with("password-value-stored:")
        })
        .count();
    (capture, security)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WarningSummary {
    code: String,
    message: String,
    count: usize,
    affected_action_ids: Vec<u64>,
    format: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    emitted_steps: Vec<usize>,
}

fn grouped_format_warnings(
    issues: &[FormatIssue],
    steps: &[Step],
    spans: &[ActionSpan],
    format: &str,
) -> Vec<WarningSummary> {
    let mut emitted_number = HashMap::new();
    let mut next_emitted = 1usize;
    for (index, step) in steps.iter().enumerate() {
        let emitted = if format == "json" {
            !step.to_recorder_json().is_null()
        } else {
            !issues
                .iter()
                .any(|issue| issue.step_index == index && issue.omitted)
        };
        if emitted {
            emitted_number.insert(index, next_emitted);
            next_emitted += 1;
        }
    }
    let mut grouped = BTreeMap::<(&str, &str), Vec<&FormatIssue>>::new();
    for issue in issues {
        grouped
            .entry((issue.code, issue.message))
            .or_default()
            .push(issue);
    }
    grouped
        .into_iter()
        .map(|((code, message), issues)| {
            let mut affected_action_ids = issues
                .iter()
                .filter_map(|issue| {
                    spans
                        .iter()
                        .find(|span| {
                            issue.step_index >= span.start
                                && issue.step_index < span.start + span.len
                        })
                        .map(|span| span.action_id)
                })
                .collect::<Vec<_>>();
            affected_action_ids.sort_unstable();
            affected_action_ids.dedup();
            affected_action_ids.truncate(10);
            let emitted_steps = issues
                .iter()
                .filter_map(|issue| emitted_number.get(&issue.step_index).copied())
                .collect::<Vec<_>>();
            WarningSummary {
                code: code.to_string(),
                message: message.to_string(),
                count: issues.len(),
                affected_action_ids,
                format: format.to_string(),
                emitted_steps,
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
struct RenderedRecorder {
    flow: Value,
    emitted: usize,
    issues: Vec<FormatIssue>,
}

fn render_recorder(
    title: &str,
    steps: &[Step],
    pages: &[sidecar::PersistedPage],
) -> RenderedRecorder {
    let page_urls = pages
        .iter()
        .map(|page| (page.page_id.as_str(), page.url.as_str()))
        .collect::<HashMap<_, _>>();
    let duplicate_urls = pages
        .iter()
        .fold(HashMap::<&str, usize>::new(), |mut counts, page| {
            *counts.entry(page.url.as_str()).or_default() += 1;
            counts
        });
    let mut emitted_steps = Vec::new();
    let mut issues = Vec::new();
    let mut warned_ambiguous_pages = HashSet::new();
    for (step_index, step) in steps.iter().enumerate() {
        if let Some((code, message, omitted)) = steps::recorder_issue_for_step(step) {
            issues.push(FormatIssue {
                code,
                message,
                step_index,
                omitted,
            });
        }
        let mut value = step.to_recorder_json();
        if value.is_null() {
            continue;
        }
        if let Some(logical_id) = value.get("target").and_then(Value::as_str) {
            // The URL captured with the action wins. The final page URL is the
            // fallback for a journal written before capture stored it.
            let captured = steps::step_scope(step).and_then(|scope| scope.page_url.as_deref());
            if let Some(url) = captured.or_else(|| page_urls.get(logical_id).copied()) {
                if duplicate_urls.get(url).copied().unwrap_or_default() > 1
                    && warned_ambiguous_pages.insert(logical_id.to_string())
                {
                    issues.push(FormatIssue {
                        code: "recorder-page-target-ambiguous",
                        message:
                            "Recorder JSON uses a URL target that identifies more than one page.",
                        step_index,
                        omitted: false,
                    });
                }
                value["target"] = json!(url);
            } else {
                issues.push(FormatIssue {
                    code: "recorder-page-target-omitted",
                    message: "Recorder JSON omitted a step because its page URL is not available.",
                    step_index,
                    omitted: true,
                });
                continue;
            }
        }
        // Recorder resolves a selector with `querySelector`, which is the
        // first match, exactly as the command did. Report it, so the two
        // formats warn alike about the same unverified target.
        if steps::single_element_target(step).is_some_and(|target| !target.verified) {
            issues.push(FormatIssue {
                code: "recorder-target-not-unique",
                message: "Recorder uses the first match, because the recorded selector was not verified to address one element.",
                step_index,
                omitted: false,
            });
        }
        emitted_steps.push(value);
    }
    RenderedRecorder {
        emitted: emitted_steps.len(),
        flow: json!({ "title": title, "steps": emitted_steps }),
        issues,
    }
}

/// Add capture data to an internal handler value. The dispatcher removes it
/// before it creates the public command response.
pub fn attach_private_capture(data: &mut Value, capture: Option<ElementCapture>) {
    let Some(capture) = capture else {
        return;
    };
    if let Some(object) = data.as_object_mut() {
        if let Ok(value) = serde_json::to_value(capture) {
            object.insert(PRIVATE_CAPTURE_KEY.to_string(), value);
        }
    }
}

pub fn take_private_capture(data: &mut Value) -> Option<ElementCapture> {
    let value = data.as_object_mut()?.remove(PRIVATE_CAPTURE_KEY)?;
    serde_json::from_value(value).ok()
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodegenStatus {
    #[default]
    Inactive,
    Active,
    Restored,
    Degraded,
    RecoveryError,
    CleanupPending,
}

impl CodegenStatus {
    fn is_capturing(&self) -> bool {
        matches!(self, Self::Active | Self::Restored | Self::Degraded)
    }
}

#[derive(Clone, Debug)]
struct ActionSpan {
    action_id: u64,
    action: String,
    start: usize,
    len: usize,
    journaled: bool,
    persisted_steps: Vec<Step>,
    step_ids: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageIdentity {
    pub page_id: String,
    pub target_id: String,
    pub session_id: String,
    pub url: String,
}

#[derive(Clone, Debug)]
struct PendingStep {
    action_id: u64,
    step_index: usize,
}

/// State intentionally lives with the daemon so a flow spans browser relaunches.
pub struct CodegenState {
    pub status: CodegenStatus,
    pub title: String,
    pub steps: Vec<Step>,
    pub viewport_emitted: bool,
    pub last_url: Option<String>,
    pub sidecar_path: Option<PathBuf>,
    pub capture_errors: Vec<String>,
    pub cleanup_paths: Vec<PathBuf>,
    pub artifact_path: Option<String>,
    action_spans: Vec<ActionSpan>,
    next_action_id: u64,
    next_step_id: u64,
    next_warning_id: u64,
    next_sequence: u64,
    persisted_last_url: Option<String>,
    pages: Vec<sidecar::PersistedPage>,
    next_page_id: u64,
    start_page_id: Option<String>,
    last_active_page_id: Option<String>,
    initial_state_captured: bool,
    persisted_page_state: sidecar::PersistedPageState,
    sessions: HashMap<String, String>,
    pending_navigation: HashMap<String, PendingStep>,
    pending_popup: HashMap<String, PendingStep>,
    /// Pages with a detached page-side call in flight. This is runtime state: a
    /// daemon restart cannot resume the call, so the journal does not carry it.
    awaiting_page_effect: HashSet<String>,
    main_frames: HashMap<String, String>,
}

impl CodegenState {
    pub fn new() -> Self {
        Self {
            status: CodegenStatus::Inactive,
            title: "agent-browser flow".to_string(),
            steps: Vec::new(),
            viewport_emitted: false,
            last_url: None,
            sidecar_path: None,
            capture_errors: Vec::new(),
            cleanup_paths: Vec::new(),
            artifact_path: None,
            action_spans: Vec::new(),
            next_action_id: 1,
            next_step_id: 1,
            next_warning_id: 1,
            next_sequence: 1,
            persisted_last_url: None,
            pages: Vec::new(),
            next_page_id: 1,
            start_page_id: None,
            last_active_page_id: None,
            initial_state_captured: false,
            persisted_page_state: sidecar::PersistedPageState {
                next_page_id: 1,
                ..sidecar::PersistedPageState::default()
            },
            sessions: HashMap::new(),
            pending_navigation: HashMap::new(),
            pending_popup: HashMap::new(),
            awaiting_page_effect: HashSet::new(),
            main_frames: HashMap::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.status.is_capturing()
    }

    fn page_state(&self) -> sidecar::PersistedPageState {
        sidecar::PersistedPageState {
            pages: self.pages.clone(),
            next_page_id: self.next_page_id,
            start_page_id: self.start_page_id.clone(),
            last_active_page_id: self.last_active_page_id.clone(),
            initial_state_captured: self.initial_state_captured,
        }
    }

    fn allocate_page(&mut self, page: &PageInfo) -> String {
        let page_id = format!("p{}", self.next_page_id);
        self.next_page_id += 1;
        self.pages.push(sidecar::PersistedPage {
            page_id: page_id.clone(),
            target_id: Some(page.target_id.clone()),
            opener_target_id: page.opener_id.clone(),
            popup_attributed: !self.initial_state_captured,
            url: page.url.clone(),
            closed: false,
            url_unrecorded: false,
        });
        page_id
    }

    /// Update logical page bindings from the current browser state. Page IDs
    /// are monotonic for the full recording and never use URLs or runtime tab IDs.
    pub fn sync_runtime_pages(
        &mut self,
        runtime_pages: &[PageInfo],
        active_target_id: Option<&str>,
        local_relaunch: bool,
    ) -> Option<PageIdentity> {
        self.sync_runtime_pages_inner(runtime_pages, active_target_id, local_relaunch, true, true)
    }

    pub fn sync_action_result_pages(
        &mut self,
        runtime_pages: &[PageInfo],
        active_target_id: Option<&str>,
    ) -> Option<PageIdentity> {
        self.sync_runtime_pages_inner(runtime_pages, active_target_id, false, false, false)
    }

    fn sync_runtime_pages_inner(
        &mut self,
        runtime_pages: &[PageInfo],
        active_target_id: Option<&str>,
        local_relaunch: bool,
        observe_url_changes: bool,
        update_page_urls: bool,
    ) -> Option<PageIdentity> {
        if !self.is_active() {
            return None;
        }
        self.sessions.clear();
        if local_relaunch {
            for page in &mut self.pages {
                page.target_id = None;
            }
        }

        let mut observed_navigations = Vec::new();
        let mut ordered = runtime_pages.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|page| (Some(page.target_id.as_str()) != active_target_id) as u8);
        for runtime in ordered {
            let mut logical_index = self
                .pages
                .iter()
                .position(|page| page.target_id.as_deref() == Some(&runtime.target_id));
            if logical_index.is_none()
                && active_target_id == Some(runtime.target_id.as_str())
                && (local_relaunch
                    || self.start_page_id.is_none()
                    || self.last_active_page_id.as_ref().is_some_and(|page_id| {
                        self.pages.iter().any(|page| {
                            &page.page_id == page_id && page.target_id.is_none() && !page.closed
                        })
                    }))
            {
                logical_index = self.last_active_page_id.as_ref().and_then(|page_id| {
                    self.pages
                        .iter()
                        .position(|page| &page.page_id == page_id && !page.closed)
                });
            }
            let page_id = if let Some(index) = logical_index {
                let page = &mut self.pages[index];
                if observe_url_changes && !page.url.is_empty() && page.url != runtime.url {
                    observed_navigations.push((page.page_id.clone(), runtime.url.clone()));
                }
                page.target_id = Some(runtime.target_id.clone());
                if page.opener_target_id.is_none() {
                    page.opener_target_id = runtime.opener_id.clone();
                }
                if update_page_urls {
                    page.url = runtime.url.clone();
                }
                page.closed = false;
                page.page_id.clone()
            } else {
                self.allocate_page(runtime)
            };
            self.sessions
                .insert(runtime.session_id.clone(), page_id.clone());
            if active_target_id == Some(runtime.target_id.as_str()) {
                self.start_page_id.get_or_insert_with(|| page_id.clone());
                self.last_active_page_id = Some(page_id);
            }
        }

        for (page_id, url) in observed_navigations {
            self.observe_navigation(&page_id, &url);
        }

        let active = active_target_id.and_then(|target_id| {
            let runtime = runtime_pages
                .iter()
                .find(|page| page.target_id == target_id)?;
            let page_id = self
                .pages
                .iter()
                .find(|page| page.target_id.as_deref() == Some(target_id))?
                .page_id
                .clone();
            Some(PageIdentity {
                page_id,
                target_id: runtime.target_id.clone(),
                session_id: runtime.session_id.clone(),
                url: runtime.url.clone(),
            })
        });
        if let Some(identity) = &active {
            self.last_url = Some(identity.url.clone());
        }
        active
    }

    pub fn scope_for_page(page: &PageIdentity) -> Scope {
        Scope {
            target: page.page_id.clone(),
            frame: Vec::new(),
            page_url: None,
        }
    }

    pub fn ensure_initial_page_state(
        &mut self,
        page: &PageIdentity,
        viewport: Option<(i32, i32, f64, bool)>,
    ) {
        if self.initial_state_captured {
            return;
        }
        let (width, height, scale, mobile) = viewport.unwrap_or((1280, 720, 1.0, false));
        let scope = Self::scope_for_page(page);
        self.capture_action(
            "codegen_start",
            vec![
                Step::ScopedViewport {
                    width: width.into(),
                    height: height.into(),
                    device_scale_factor: scale,
                    is_mobile: mobile,
                    scope: scope.clone(),
                },
                Step::ScopedNavigation {
                    kind: NavigationKind::Goto,
                    url: page.url.clone(),
                    scope,
                },
            ],
        );
        self.viewport_emitted = true;
        self.initial_state_captured = true;
    }

    pub fn page_for_target(&self, target_id: &str) -> Option<String> {
        self.pages
            .iter()
            .find(|page| page.target_id.as_deref() == Some(target_id))
            .map(|page| page.page_id.clone())
    }

    pub fn page_for_session(&self, session_id: &str) -> Option<String> {
        self.sessions.get(session_id).cloned()
    }

    pub fn mark_page_closed(&mut self, page_id: &str) {
        if let Some(page) = self.pages.iter_mut().find(|page| page.page_id == page_id) {
            page.closed = true;
            page.target_id = None;
        }
        self.pending_navigation.remove(page_id);
        self.pending_popup.remove(page_id);
        self.awaiting_page_effect.remove(page_id);
    }

    /// Drop the pending navigation and popup steps for a page. A command that
    /// codegen cannot attribute must not let its own page move attach a URL to
    /// an earlier click.
    pub fn discard_pending_steps(&mut self, page_id: &str) {
        self.pending_navigation.remove(page_id);
        self.pending_popup.remove(page_id);
    }

    pub fn update_page_url(&mut self, page_id: &str, url: &str) {
        if let Some(page) = self.pages.iter_mut().find(|page| page.page_id == page_id) {
            page.url = url.to_string();
        }
        if self.last_active_page_id.as_deref() == Some(page_id)
            || page_id == "main"
            || page_id == "p1"
        {
            self.last_url = Some(url.to_string());
        }
    }

    /// A command that codegen cannot express moved a page. Keep the true URL,
    /// mark that no step reaches it, and warn. The warning names the action and
    /// the page only, so it cannot leak a captured value.
    pub fn observe_unrecorded_navigation(&mut self, page_id: &str, url: &str, action: &str) {
        self.update_page_url(page_id, url);
        self.mark_unrecorded_page_url(page_id, action);
    }

    fn mark_unrecorded_page_url(&mut self, page_id: &str, action: &str) {
        if let Some(page) = self.pages.iter_mut().find(|page| page.page_id == page_id) {
            page.url_unrecorded = true;
        }
        self.capture_warning(
            "unrecorded-navigation",
            &format!(
                "`{action}` moved page {page_id}. Codegen cannot record that command, so the flow has no step that reaches the new page."
            ),
            None,
        );
    }

    /// A detached `webmcp invoke` returns before the page tool runs, so the
    /// post-action URL check sees nothing. Mark the page, and treat the next
    /// URL change that no recorded step explains as unrecorded.
    pub fn expect_unattributed_page_effect(&mut self, page_id: &str) {
        self.awaiting_page_effect.insert(page_id.to_string());
    }

    pub fn page_url_is_unrecorded(&self, page_id: &str) -> bool {
        self.pages
            .iter()
            .find(|page| page.page_id == page_id)
            .is_some_and(|page| page.url_unrecorded)
    }

    pub fn clear_unrecorded_page_url(&mut self, page_id: &str) {
        if let Some(page) = self.pages.iter_mut().find(|page| page.page_id == page_id) {
            page.url_unrecorded = false;
        }
    }

    pub fn page_url(&self, page_id: &str) -> Option<&str> {
        self.pages
            .iter()
            .find(|page| page.page_id == page_id)
            .map(|page| page.url.as_str())
            .or_else(|| {
                (page_id == "main" || page_id == "p1")
                    .then_some(self.last_url.as_deref())
                    .flatten()
            })
    }

    pub fn register_action_intent(&mut self, action_id: u64, page_id: &str) {
        let Some(span) = self
            .action_spans
            .iter()
            .find(|span| span.action_id == action_id)
        else {
            return;
        };
        let navigation = (span.start..span.start + span.len)
            .rev()
            .find(|index| can_assert_navigation(&self.steps[*index]));
        let popup = (span.start..span.start + span.len)
            .rev()
            .find(|index| can_open_popup(&self.steps[*index]));
        if let Some(step_index) = navigation {
            self.pending_navigation.insert(
                page_id.to_string(),
                PendingStep {
                    action_id,
                    step_index,
                },
            );
        }
        if let Some(step_index) = popup {
            if self.pending_popup.contains_key(page_id) {
                self.capture_warning(
                    "ambiguous-popup-origin",
                    "More than one click can be the opener for a new page. Codegen did not guess the opener.",
                    Some(action_id),
                );
                self.pending_popup.remove(page_id);
            } else {
                self.pending_popup.insert(
                    page_id.to_string(),
                    PendingStep {
                        action_id,
                        step_index,
                    },
                );
            }
        }
    }

    pub fn observe_navigation(&mut self, page_id: &str, url: &str) -> bool {
        self.update_page_url(page_id, url);
        let Some(pending) = self.pending_navigation.get(page_id).cloned() else {
            if self.awaiting_page_effect.remove(page_id) {
                self.mark_unrecorded_page_url(page_id, "webmcp invoke --detach");
            }
            return false;
        };
        if let Some(step) = self.steps.get_mut(pending.step_index) {
            attach_navigation(step, url);
            return true;
        }
        false
    }

    pub fn observe_popup(&mut self, opener_target_id: &str, popup_page_id: &str) -> bool {
        let Some(opener_page_id) = self.page_for_target(opener_target_id) else {
            self.capture_warning(
                "ambiguous-popup-origin",
                "Chrome did not report a known opener page. Codegen did not guess the opener.",
                None,
            );
            return false;
        };
        let Some(pending) = self.pending_popup.remove(&opener_page_id) else {
            self.capture_warning(
                "ambiguous-popup-origin",
                "No pending click matches the new page opener. Codegen did not guess the opener.",
                None,
            );
            return false;
        };
        if let Some(step) = self.steps.get_mut(pending.step_index) {
            bind_popup(step, popup_page_id);
            return true;
        }
        self.capture_warning(
            "ambiguous-popup-origin",
            "The pending popup action is not available. Codegen did not guess the opener.",
            Some(pending.action_id),
        );
        false
    }

    pub fn bind_unattributed_popups(&mut self) {
        let candidates = self
            .pages
            .iter()
            .filter(|page| !page.popup_attributed)
            .filter_map(|page| Some((page.page_id.clone(), page.opener_target_id.clone()?)))
            .collect::<Vec<_>>();
        for (page_id, opener_target_id) in candidates {
            self.observe_popup(&opener_target_id, &page_id);
            if let Some(page) = self.pages.iter_mut().find(|page| page.page_id == page_id) {
                page.popup_attributed = true;
            }
        }
    }

    pub fn clear_runtime_bindings(&mut self, keep_targets: bool) {
        self.sessions.clear();
        self.main_frames.clear();
        if !keep_targets {
            for page in &mut self.pages {
                page.target_id = None;
            }
        }
    }

    pub fn observe_cdp_navigation(
        &mut self,
        session_id: &str,
        frame_id: &str,
        url: &str,
        main_frame_event: bool,
    ) -> bool {
        if main_frame_event {
            self.main_frames
                .insert(session_id.to_string(), frame_id.to_string());
        } else if self.main_frames.get(session_id).map(String::as_str) != Some(frame_id) {
            return false;
        }
        let Some(page_id) = self.page_for_session(session_id) else {
            return false;
        };
        self.observe_navigation(&page_id, url)
    }

    pub fn pending_navigation_sessions(&self) -> Vec<(String, String)> {
        self.pending_navigation
            .keys()
            .filter_map(|page_id| {
                let session_id = self.sessions.iter().find_map(|(session_id, mapped_page)| {
                    (mapped_page == page_id).then(|| session_id.clone())
                })?;
                Some((page_id.clone(), session_id))
            })
            .collect()
    }

    pub fn restore(session_id: &str) -> Self {
        let path = sidecar::path_for_session(session_id);
        let existing = sidecar::existing_known_paths(&path);
        if existing.is_empty() {
            return Self::new();
        }
        if !path.exists() {
            let mut state = Self::new();
            state.status = CodegenStatus::RecoveryError;
            state.sidecar_path = Some(path);
            state.cleanup_paths = existing;
            state.capture_errors.push(
                "Unsupported codegen metadata from an earlier development build. Run `codegen discard`."
                    .to_string(),
            );
            return state;
        }
        match sidecar::recover(&path) {
            Ok(recovered) => {
                // Capture resumes from here, so the journal must be able to
                // accept the next append.
                if let Err(error) = sidecar::repair(&path, recovered.valid_bytes) {
                    let mut state = Self::new();
                    state.status = CodegenStatus::RecoveryError;
                    state.sidecar_path = Some(path.clone());
                    state.cleanup_paths = sidecar::existing_known_paths(&path);
                    state.capture_errors.push(error);
                    return state;
                }
                Self::from_recovered(path, recovered)
            }
            Err(error) => {
                let mut state = Self::new();
                state.status = CodegenStatus::RecoveryError;
                state.sidecar_path = Some(path.clone());
                state.cleanup_paths = sidecar::existing_known_paths(&path);
                state.capture_errors.push(error);
                state
            }
        }
    }

    fn from_recovered(path: PathBuf, recovered: sidecar::RecoveredJournal) -> Self {
        let mut state = Self::new();
        state.title = recovered.title;
        state.last_url = recovered.last_url.clone();
        state.persisted_last_url = recovered.last_url;
        state.pages = recovered.page_state.pages.clone();
        state.next_page_id = recovered.page_state.next_page_id.max(1);
        state.start_page_id = recovered.page_state.start_page_id.clone();
        state.last_active_page_id = recovered.page_state.last_active_page_id.clone();
        state.initial_state_captured = recovered.page_state.initial_state_captured;
        state.persisted_page_state = recovered.page_state;
        state.next_sequence = recovered.next_sequence;
        state.next_warning_id = recovered.next_warning_id;
        state.sidecar_path = Some(path.clone());
        state.capture_errors = recovered.degraded_messages;
        state.capture_errors.extend(recovered.warnings);
        state.artifact_path = recovered
            .terminal
            .as_ref()
            .and_then(|terminal| match terminal {
                sidecar::TerminalRecord::OutputWritten { path, .. } => path.clone(),
                sidecar::TerminalRecord::DiscardRequested => None,
            });
        state.status = if recovered.terminal.is_some() {
            state.cleanup_paths = sidecar::existing_known_paths(&path);
            CodegenStatus::CleanupPending
        } else if state.capture_errors.is_empty() {
            CodegenStatus::Restored
        } else {
            CodegenStatus::Degraded
        };
        for action in recovered.actions {
            let start = state.steps.len();
            let len = action.steps.len();
            let step_ids = action
                .steps
                .iter()
                .map(|captured| captured.step_id)
                .collect::<Vec<_>>();
            let steps = action
                .steps
                .iter()
                .map(|captured| captured.step.clone())
                .collect::<Vec<_>>();
            state.steps.extend(steps.clone());
            state.next_action_id = state.next_action_id.max(action.action_id + 1);
            state.next_step_id = step_ids
                .iter()
                .fold(state.next_step_id, |next, step_id| next.max(step_id + 1));
            state.action_spans.push(ActionSpan {
                action_id: action.action_id,
                action: action.action,
                start,
                len,
                journaled: true,
                persisted_steps: steps,
                step_ids,
            });
        }
        state.viewport_emitted = state
            .steps
            .iter()
            .any(|step| matches!(step, Step::SetViewport { .. } | Step::ScopedViewport { .. }));
        for span in &state.action_spans {
            for step_index in span.start..span.start + span.len {
                let Some(scope) = step_scope(&state.steps[step_index]) else {
                    continue;
                };
                let pending = PendingStep {
                    action_id: span.action_id,
                    step_index,
                };
                if can_assert_navigation(&state.steps[step_index]) {
                    state
                        .pending_navigation
                        .insert(scope.target.clone(), pending.clone());
                }
                if can_open_popup(&state.steps[step_index]) {
                    state.pending_popup.insert(scope.target.clone(), pending);
                }
            }
        }
        state
    }

    fn append_record(&mut self, record: sidecar::JournalRecord) -> Result<(), String> {
        let path = self
            .sidecar_path
            .as_ref()
            .ok_or_else(|| "Codegen journal path is not available.".to_string())?;
        sidecar::append(path, self.next_sequence, &record)?;
        self.next_sequence += 1;
        Ok(())
    }

    fn mark_degraded(&mut self, error: String) {
        if !self.capture_errors.contains(&error) {
            self.capture_errors.push(error.clone());
        }
        self.status = CodegenStatus::Degraded;
        let _ = self.append_record(sidecar::JournalRecord::Degraded { message: error });
    }

    pub fn capture_action(&mut self, action: &str, steps: Vec<Step>) -> u64 {
        let action_id = self.next_action_id;
        self.next_action_id += 1;
        let start = self.steps.len();
        let len = steps.len();
        let step_ids = (self.next_step_id..self.next_step_id + len as u64).collect::<Vec<_>>();
        self.next_step_id += len as u64;
        self.steps.extend(steps.clone());
        let captured = sidecar::CapturedAction {
            action_id,
            action: action.to_string(),
            steps: step_ids
                .iter()
                .copied()
                .zip(steps.iter().cloned())
                .map(|(step_id, step)| sidecar::CapturedStep { step_id, step })
                .collect(),
        };
        let journaled = match self.append_record(sidecar::JournalRecord::Action(captured)) {
            Ok(()) => true,
            Err(error) => {
                self.mark_degraded(error);
                false
            }
        };
        self.action_spans.push(ActionSpan {
            action_id,
            action: action.to_string(),
            start,
            len,
            journaled,
            persisted_steps: if journaled { steps } else { Vec::new() },
            step_ids,
        });
        action_id
    }

    pub fn capture_warning(&mut self, code: &str, message: &str, action_id: Option<u64>) {
        let warning_id = self.next_warning_id;
        self.next_warning_id += 1;
        let display = format!("{code}: {message}");
        self.capture_errors.push(display);
        if let Err(error) = self.append_record(sidecar::JournalRecord::Warning {
            warning_id,
            code: code.to_string(),
            message: message.to_string(),
            action_id,
            step_id: None,
        }) {
            self.mark_degraded(error);
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

pub fn codegen_start(
    state: &mut CodegenState,
    title: Option<&str>,
    session_id: &str,
) -> Result<Value, String> {
    if state.status != CodegenStatus::Inactive {
        return Err(match state.status {
            CodegenStatus::Active | CodegenStatus::Restored | CodegenStatus::Degraded => {
                "Codegen already has a recording. Run `codegen stop` or `codegen discard`."
                    .to_string()
            }
            CodegenStatus::RecoveryError | CodegenStatus::CleanupPending => {
                "Codegen recovery requires cleanup. Run `codegen discard`.".to_string()
            }
            CodegenStatus::Inactive => unreachable!(),
        });
    }
    let title = title
        .filter(|title| !title.is_empty())
        .unwrap_or("agent-browser flow")
        .to_string();
    let expected_path = sidecar::path_for_session(session_id);
    let (path, next_sequence) = match sidecar::create(session_id, &title) {
        Ok(created) => created,
        Err(error) => {
            let existing = sidecar::existing_known_paths(&expected_path);
            if !existing.is_empty() {
                state.status = CodegenStatus::RecoveryError;
                state.sidecar_path = Some(expected_path);
                state.cleanup_paths = existing;
                state.capture_errors.push(error.clone());
            }
            return Err(error);
        }
    };
    state.reset();
    state.status = CodegenStatus::Active;
    state.title = title;
    state.sidecar_path = Some(path);
    state.next_sequence = next_sequence;
    Ok(json!({
        "started": true,
        "state": state.status,
        "active": true,
        "title": state.title,
        "journalPath": state.sidecar_path,
    }))
}

pub fn codegen_status(state: &CodegenState) -> Value {
    let recorder = render_recorder(&state.title, &state.steps, &state.pages);
    let playwright = render_playwright_with_report(&state.title, &state.steps);
    let recorder_omitted = recorder.issues.iter().filter(|issue| issue.omitted).count();
    let recorder_lossy = recorder.issues.len() - recorder_omitted;
    let playwright_omitted = playwright
        .issues
        .iter()
        .filter(|issue| issue.omitted)
        .count();
    let playwright_lossy = playwright.issues.len() - playwright_omitted;
    let (capture_warning_count, security_warning_count) = warning_counts(state, &state.steps);
    json!({
        "state": state.status,
        "active": state.is_active(),
        "title": state.title,
        "steps": state.steps.len(),
        "internalSteps": state.steps.len(),
        "capturedActions": state.action_spans.len(),
        "warningCount": state.capture_errors.len(),
        "captureWarningCount": capture_warning_count,
        "securityWarningCount": security_warning_count,
        "journalPath": state.sidecar_path,
        "captureErrors": state.capture_errors,
        "cleanupPaths": state.cleanup_paths,
        "artifactPath": state.artifact_path,
        "projectedFormats": {
            "json": { "emitted": recorder.emitted, "omitted": recorder_omitted, "lossy": recorder_lossy },
            "playwright": { "emitted": playwright.emitted, "omitted": playwright_omitted, "lossy": playwright_lossy },
        },
    })
}

pub fn persist(state: &mut CodegenState) -> Result<(), String> {
    if !state.is_active() {
        return Ok(());
    }
    let mut first_error = None;
    for index in 0..state.action_spans.len() {
        let (journaled, start, len, persisted_steps) = {
            let span = &state.action_spans[index];
            (
                span.journaled,
                span.start,
                span.len,
                span.persisted_steps.clone(),
            )
        };
        if !journaled {
            continue;
        }
        let current_steps = state.steps[start..start + len].to_vec();
        if current_steps == persisted_steps {
            continue;
        }
        let captured = {
            let span = &state.action_spans[index];
            sidecar::CapturedAction {
                action_id: span.action_id,
                action: span.action.clone(),
                steps: span
                    .step_ids
                    .iter()
                    .copied()
                    .zip(current_steps.iter().cloned())
                    .map(|(step_id, step)| sidecar::CapturedStep { step_id, step })
                    .collect(),
            }
        };
        match state.append_record(sidecar::JournalRecord::UpdateAction(captured)) {
            Ok(()) => state.action_spans[index].persisted_steps = current_steps,
            Err(error) => {
                first_error.get_or_insert_with(|| error.clone());
                state.mark_degraded(error);
                break;
            }
        }
    }
    if state.last_url != state.persisted_last_url {
        match state.append_record(sidecar::JournalRecord::State {
            last_url: state.last_url.clone(),
        }) {
            Ok(()) => state.persisted_last_url = state.last_url.clone(),
            Err(error) => {
                first_error.get_or_insert_with(|| error.clone());
                state.mark_degraded(error);
            }
        }
    }
    let page_state = state.page_state();
    if page_state != state.persisted_page_state {
        match state.append_record(sidecar::JournalRecord::Pages(page_state.clone())) {
            Ok(()) => state.persisted_page_state = page_state,
            Err(error) => {
                first_error.get_or_insert_with(|| error.clone());
                state.mark_degraded(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub fn codegen_stop(
    state: &mut CodegenState,
    path: Option<&str>,
    format: &str,
) -> Result<Value, String> {
    if !state.is_active() {
        return Err(match state.status {
            CodegenStatus::CleanupPending => {
                "Codegen output is complete, but journal cleanup is pending. Run `codegen discard`."
                    .to_string()
            }
            CodegenStatus::RecoveryError => {
                "Codegen journal cannot be recovered. Run `codegen discard`.".to_string()
            }
            _ => "No codegen flow in progress. Start one with `codegen start`.".to_string(),
        });
    }
    let steps = if state.status == CodegenStatus::Degraded {
        state.steps.clone()
    } else {
        let journal_path = state
            .sidecar_path
            .as_ref()
            .ok_or_else(|| "Codegen journal path is not available.".to_string())?;
        match sidecar::recover(journal_path) {
            Ok(recovered) => recovered
                .actions
                .into_iter()
                .flat_map(|action| action.steps.into_iter().map(|captured| captured.step))
                .collect(),
            Err(error) => {
                state.status = CodegenStatus::RecoveryError;
                state.capture_errors.push(error.clone());
                return Err(error);
            }
        }
    };
    let recorder = render_recorder(&state.title, &steps, &state.pages);
    let playwright = render_playwright_with_report(&state.title, &steps);
    let (output, emitted, issues) = if format == "playwright" {
        (playwright.output, playwright.emitted, playwright.issues)
    } else {
        (
            serde_json::to_string_pretty(&recorder.flow).map_err(|error| error.to_string())?,
            recorder.emitted,
            recorder.issues,
        )
    };
    if let Some(path) = path {
        sidecar::write_output_atomic(Path::new(path), &output)?;
    }
    if let Err(error) = state.append_record(sidecar::JournalRecord::OutputWritten {
        format: format.to_string(),
        path: path.map(str::to_string),
    }) {
        state.mark_degraded(error.clone());
        return Err(format!(
            "Codegen output was generated, but its terminal journal record failed: {error}. Retry `codegen stop` or run `codegen discard`."
        ));
    }

    let password_steps: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| step.has_password().then_some(index + 1))
        .collect();
    let typed_values = steps.iter().any(|step| {
        matches!(
            step,
            Step::Change { .. }
                | Step::Fill { .. }
                | Step::SetValue { .. }
                | Step::Type { .. }
                | Step::Select { .. }
                | Step::Upload { .. }
        )
    });
    let omitted = issues.iter().filter(|issue| issue.omitted).count();
    let lossy = issues.len() - omitted;
    let warnings = grouped_format_warnings(&issues, &steps, &state.action_spans, format);
    let (capture_warning_count, security_warning_count) = warning_counts(state, &steps);
    let mut data = json!({
        "state": "inactive",
        "active": false,
        "title": state.title,
        "steps": steps.len(),
        "internalSteps": steps.len(),
        "emittedSteps": emitted,
        "omittedSteps": omitted,
        "lossySteps": lossy,
        "capturedActions": state.action_spans.len(),
        "format": format,
        "flow": recorder.flow,
        "captureErrors": state.capture_errors,
        "captureWarningCount": capture_warning_count,
        "securityWarningCount": security_warning_count,
        "cleanupWarningCount": state.cleanup_paths.len(),
        "warnings": warnings,
    });
    if let Some(path) = path {
        data["path"] = json!(path);
    } else {
        data["output"] = json!(output);
    }
    if !password_steps.is_empty() {
        data["warning"] = json!(format!(
            "Recorded credentials in step(s) {} verbatim. Do not commit this artifact.",
            password_steps
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if typed_values {
        data["warning"] = json!(
            "The recording stores typed values and file paths verbatim. Review the artifact before you share or commit it."
        );
    }

    state.status = CodegenStatus::Inactive;
    state.artifact_path = path.map(str::to_string);
    let journal_path = state
        .sidecar_path
        .clone()
        .ok_or_else(|| "Codegen journal path is not available.".to_string())?;
    match sidecar::remove_known_files(&journal_path) {
        Ok(()) => {
            state.sidecar_path = None;
            state.cleanup_paths.clear();
            Ok(data)
        }
        Err(remaining) => {
            state.status = CodegenStatus::CleanupPending;
            state.cleanup_paths = remaining.clone();
            Err(format!(
                "Codegen output was written{}, but credential-bearing journal cleanup failed for: {}. Run `codegen discard`.",
                path.map(|value| format!(" to {value}"))
                    .unwrap_or_default(),
                remaining
                    .iter()
                    .map(|value| value.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

pub fn codegen_discard(state: &mut CodegenState, session_id: &str) -> Result<Value, String> {
    let journal_path = state
        .sidecar_path
        .clone()
        .unwrap_or_else(|| sidecar::path_for_session(session_id));
    let existing = sidecar::existing_known_paths(&journal_path);
    if state.status == CodegenStatus::Inactive && existing.is_empty() {
        return Err("No codegen recording exists to discard.".to_string());
    }
    if state.is_active() {
        state.append_record(sidecar::JournalRecord::DiscardRequested)?;
    }
    match sidecar::remove_known_files(&journal_path) {
        Ok(()) => {
            let removed = existing
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            state.reset();
            Ok(json!({
                "discarded": true,
                "state": "inactive",
                "active": false,
                "removedPaths": removed,
            }))
        }
        Err(remaining) => {
            state.status = CodegenStatus::CleanupPending;
            state.cleanup_paths = remaining.clone();
            Err(format!(
                "Codegen cleanup is still pending for: {}",
                remaining
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(tab_id: u32, target_id: &str, session_id: &str, url: &str) -> PageInfo {
        PageInfo {
            tab_id,
            label: None,
            target_id: target_id.to_string(),
            opener_id: None,
            session_id: session_id.to_string(),
            url: url.to_string(),
            title: String::new(),
            target_type: "page".to_string(),
        }
    }

    #[test]
    fn private_element_capture_round_trips_without_public_fields() {
        let mut data = json!({ "filled": "#password" });
        attach_private_capture(
            &mut data,
            Some(ElementCapture {
                probe: Some(probe::Probe {
                    input_type: Some("password".to_string()),
                    ..probe::Probe::default()
                }),
                ..ElementCapture::default()
            }),
        );

        let capture = take_private_capture(&mut data).unwrap();

        assert_eq!(
            capture.probe.unwrap().input_type.as_deref(),
            Some("password")
        );
        assert_eq!(data, json!({ "filled": "#password" }));
    }

    fn restored_state(directory: &tempfile::TempDir) -> CodegenState {
        let path = directory.path().join("flow.codegen.jsonl");
        std::fs::write(&path, "").unwrap();
        sidecar::append(
            &path,
            1,
            &sidecar::JournalRecord::Start {
                title: "flow".to_string(),
                last_url: None,
            },
        )
        .unwrap();
        CodegenState::from_recovered(path.clone(), sidecar::recover(&path).unwrap())
    }

    #[test]
    fn restores_an_in_progress_journal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        std::fs::write(&path, "").unwrap();
        sidecar::append(
            &path,
            1,
            &sidecar::JournalRecord::Start {
                title: "checkout".to_string(),
                last_url: Some("https://example.com/cart".to_string()),
            },
        )
        .unwrap();
        sidecar::append(
            &path,
            2,
            &sidecar::JournalRecord::Action(sidecar::CapturedAction {
                action_id: 1,
                action: "viewport".to_string(),
                steps: vec![sidecar::CapturedStep {
                    step_id: 1,
                    step: Step::SetViewport {
                        width: 1280,
                        height: 720,
                        device_scale_factor: 1.0,
                        is_mobile: false,
                    },
                }],
            }),
        )
        .unwrap();

        let state = CodegenState::from_recovered(path.clone(), sidecar::recover(&path).unwrap());

        assert_eq!(state.status, CodegenStatus::Restored);
        assert_eq!(state.title, "checkout");
        assert!(state.viewport_emitted);
        assert_eq!(state.last_url.as_deref(), Some("https://example.com/cart"));
        assert_eq!(state.sidecar_path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn stop_emits_navigation_and_password_warning() {
        let directory = tempfile::tempdir().unwrap();
        let target = Target {
            selectors: vec![SelectorKind::Css {
                value: "#password".to_string(),
            }],
            input_type: Some("password".to_string()),
            verified: true,
        };
        let mut state = restored_state(&directory);
        state.title = "login".to_string();
        state.capture_action(
            "login",
            vec![
                Step::Click {
                    target: target.clone(),
                    count: 1,
                    kind: ClickKind::Click,
                    opens_popup: false,
                    scope: Scope::default(),
                    asserted_url: Some("https://example.com/account".to_string()),
                },
                Step::Change {
                    target,
                    value: "secret".to_string(),
                    is_select: false,
                    scope: Scope::default(),
                    asserted_url: None,
                },
            ],
        );

        let result = codegen_stop(&mut state, None, "json").unwrap();

        assert!(result["warning"].as_str().unwrap().contains("credentials"));
        assert!(result["output"].as_str().unwrap().contains("account"));
        assert!(!directory.path().join("flow.codegen.jsonl").exists());
    }

    #[test]
    fn output_failure_keeps_the_recording_retryable() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        state.capture_action(
            "viewport",
            vec![Step::SetViewport {
                width: 800,
                height: 600,
                device_scale_factor: 1.0,
                is_mobile: false,
            }],
        );
        let missing_parent = directory.path().join("missing/flow.json");

        assert!(codegen_stop(&mut state, missing_parent.to_str(), "json").is_err());
        assert!(state.is_active());
        let output = directory.path().join("flow.json");
        assert!(codegen_stop(&mut state, output.to_str(), "json").is_ok());
        assert!(output.exists());
    }

    #[test]
    fn cleanup_failure_after_output_enters_cleanup_pending() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        state.capture_action(
            "viewport",
            vec![Step::SetViewport {
                width: 800,
                height: 600,
                device_scale_factor: 1.0,
                is_mobile: false,
            }],
        );
        let journal = state.sidecar_path.clone().unwrap();
        let blocker = sidecar::legacy_metadata_paths(&journal)[0].clone();
        std::fs::create_dir(&blocker).unwrap();
        let output = directory.path().join("flow.json");

        let error = codegen_stop(&mut state, output.to_str(), "json").unwrap_err();

        assert!(error.contains("cleanup failed"));
        assert_eq!(state.status, CodegenStatus::CleanupPending);
        assert!(output.exists());
        std::fs::remove_dir(&blocker).unwrap();
        codegen_discard(&mut state, "unused").unwrap();
    }

    #[test]
    fn discard_retries_cleanup_after_a_terminal_record() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        let journal = state.sidecar_path.clone().unwrap();
        let blocker = sidecar::legacy_metadata_paths(&journal)[0].clone();
        std::fs::create_dir(&blocker).unwrap();

        assert!(codegen_discard(&mut state, "unused").is_err());
        assert_eq!(state.status, CodegenStatus::CleanupPending);
        std::fs::remove_dir(&blocker).unwrap();
        let retry = codegen_discard(&mut state, "unused").unwrap();

        assert_eq!(retry["discarded"], true);
        assert_eq!(state.status, CodegenStatus::Inactive);
    }

    #[test]
    fn append_failure_latches_a_degraded_state() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        std::fs::remove_file(state.sidecar_path.as_ref().unwrap()).unwrap();

        state.capture_action(
            "viewport",
            vec![Step::SetViewport {
                width: 800,
                height: 600,
                device_scale_factor: 1.0,
                is_mobile: false,
            }],
        );

        assert_eq!(state.status, CodegenStatus::Degraded);
        assert_eq!(state.capture_errors.len(), 1);
    }

    #[test]
    fn status_exposes_recovery_state() {
        let mut state = CodegenState::new();
        state.status = CodegenStatus::RecoveryError;
        state.capture_errors.push("bad journal".to_string());

        let status = codegen_status(&state);

        assert_eq!(status["state"], "recovery-error");
        assert_eq!(status["active"], false);
        assert_eq!(status["warningCount"], 1);
    }

    #[test]
    fn status_reports_all_states_and_derived_activity() {
        for (state_value, name, active) in [
            (CodegenStatus::Inactive, "inactive", false),
            (CodegenStatus::Active, "active", true),
            (CodegenStatus::Restored, "restored", true),
            (CodegenStatus::Degraded, "degraded", true),
            (CodegenStatus::RecoveryError, "recovery-error", false),
            (CodegenStatus::CleanupPending, "cleanup-pending", false),
        ] {
            let mut state = CodegenState::new();
            state.status = state_value;
            let status = codegen_status(&state);
            assert_eq!(status["state"], name);
            assert_eq!(status["active"], active);
        }
    }

    #[test]
    fn production_renderer_matches_shared_recorder_fixture() {
        let target = Target {
            selectors: vec![SelectorKind::Css {
                value: "#submit".to_string(),
            }],
            input_type: None,
            verified: true,
        };
        let steps = vec![
            Step::ScopedViewport {
                width: 1280,
                height: 720,
                device_scale_factor: 1.0,
                is_mobile: false,
                scope: Scope {
                    target: "p1".to_string(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::ScopedNavigation {
                kind: NavigationKind::Goto,
                url: "https://example.com/start".to_string(),
                scope: Scope {
                    target: "p1".to_string(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::Pointer {
                target: target.clone(),
                kind: ClickKind::Click,
                pointer: PointerKind::Mouse,
                button: "right".to_string(),
                count: 1,
                position: Some((4.0, 8.0)),
                opens_popup: false,
                popup_page: None,
                scope: Scope {
                    target: "p1".to_string(),
                    frame: Vec::new(),
                    page_url: None,
                },
                asserted_url: Some("https://example.com/done".to_string()),
            },
            Step::Fill {
                target,
                value: "line one\nline two".to_string(),
                scope: Scope {
                    target: "p2".to_string(),
                    frame: vec![0, 1],
                    page_url: None,
                },
                asserted_url: None,
            },
            // A snapshot ref: the primary capture path, and the only one that
            // produces an accessible-name selector.
            Step::Hover {
                target: Target {
                    selectors: vec![
                        SelectorKind::Role {
                            role: "button".to_string(),
                            name: "Save \"now\"".to_string(),
                            nth: None,
                        },
                        SelectorKind::Css {
                            value: "#save".to_string(),
                        },
                    ],
                    input_type: None,
                    verified: true,
                },
                scope: Scope {
                    target: "p1".to_string(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
            Step::ClosePage {
                scope: Scope {
                    target: "p2".to_string(),
                    frame: Vec::new(),
                    page_url: None,
                },
            },
        ];
        let pages = vec![
            sidecar::PersistedPage {
                page_id: "p1".to_string(),
                target_id: None,
                opener_target_id: None,
                popup_attributed: true,
                url: "https://example.com/start".to_string(),
                closed: false,
                url_unrecorded: false,
            },
            sidecar::PersistedPage {
                page_id: "p2".to_string(),
                target_id: None,
                opener_target_id: None,
                popup_attributed: true,
                url: "https://example.com/other".to_string(),
                closed: false,
                url_unrecorded: false,
            },
        ];
        let rendered = render_recorder("hostile \"flow\"\nname", &steps, &pages);
        let output = serde_json::to_string_pretty(&rendered.flow).unwrap();
        assert_eq!(output, include_str!("test-fixtures/flow.json").trim_end());
    }

    #[test]
    fn logical_pages_are_monotonic_and_do_not_use_urls_or_tab_ids() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        let pages = vec![
            page(9, "target-a", "session-a", "https://example.com/same"),
            page(9, "target-b", "session-b", "https://example.com/same"),
        ];

        let first = state
            .sync_runtime_pages(&pages, Some("target-b"), false)
            .unwrap();

        assert_eq!(first.page_id, "p1");
        assert_eq!(state.page_for_target("target-a").as_deref(), Some("p2"));
        assert_ne!(
            state.page_for_target("target-a"),
            state.page_for_target("target-b")
        );
    }

    #[test]
    fn selected_format_reports_grouped_omissions_and_counts() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        state.capture_action(
            "type",
            vec![Step::Type {
                target: Target {
                    selectors: vec![SelectorKind::Css {
                        value: "#field".to_string(),
                    }],
                    input_type: None,
                    verified: true,
                },
                text: "secret".to_string(),
                clear: false,
                delay_ms: Some(5),
                scope: Scope::default(),
                asserted_url: None,
            }],
        );

        let result = codegen_stop(&mut state, None, "json").unwrap();

        assert_eq!(result["capturedActions"], 1);
        assert_eq!(result["internalSteps"], 1);
        assert_eq!(result["emittedSteps"], 0);
        assert_eq!(result["omittedSteps"], 1);
        assert_eq!(result["lossySteps"], 0);
        assert_eq!(result["warnings"][0]["code"], "recorder-type-omitted");
        assert_eq!(result["warnings"][0]["affectedActionIds"], json!([1]));
        assert_eq!(result["securityWarningCount"], 1);
    }

    #[test]
    fn initial_page_state_is_scoped_to_p1() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        let runtime = page(1, "target-a", "session-a", "https://example.com/start");
        let identity = state
            .sync_runtime_pages(&[runtime], Some("target-a"), false)
            .unwrap();

        state.ensure_initial_page_state(&identity, Some((900, 700, 2.0, true)));

        assert!(matches!(
            &state.steps[0],
            Step::ScopedViewport { scope, width: 900, .. } if scope.target == "p1"
        ));
        assert!(matches!(
            &state.steps[1],
            Step::ScopedNavigation { scope, url, .. }
                if scope.target == "p1" && url == "https://example.com/start"
        ));
    }

    #[test]
    fn local_relaunch_rebinds_only_the_last_active_page() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        let first_pages = vec![
            page(1, "target-a", "session-a", "https://example.com/a"),
            page(2, "target-b", "session-b", "https://example.com/b"),
        ];
        state.sync_runtime_pages(&first_pages, Some("target-a"), false);
        state.sync_runtime_pages(&first_pages, Some("target-b"), false);
        state.clear_runtime_bindings(false);

        let relaunched = state
            .sync_runtime_pages(
                &[page(1, "target-c", "session-c", "about:blank")],
                Some("target-c"),
                true,
            )
            .unwrap();

        assert_eq!(relaunched.page_id, "p2");
        assert_eq!(state.page_for_target("target-c").as_deref(), Some("p2"));
        assert_eq!(state.pages[0].target_id, None);
    }

    #[test]
    fn external_reconnect_reuses_only_an_unchanged_target_id() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        state.sync_runtime_pages(
            &[page(1, "target-a", "session-a", "https://example.com")],
            Some("target-a"),
            false,
        );
        state.clear_runtime_bindings(true);

        let reused = state
            .sync_runtime_pages(
                &[page(7, "target-a", "session-new", "https://example.com")],
                Some("target-a"),
                false,
            )
            .unwrap();
        let different = state
            .sync_runtime_pages(
                &[page(1, "target-b", "session-b", "https://example.com")],
                Some("target-b"),
                false,
            )
            .unwrap();

        assert_eq!(reused.page_id, "p1");
        assert_eq!(different.page_id, "p2");
    }

    #[test]
    fn popup_binding_uses_the_reported_opener_and_keeps_unique_page_ids() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        let opener = page(1, "opener", "session-a", "https://example.com");
        let identity = state
            .sync_runtime_pages(std::slice::from_ref(&opener), Some("opener"), false)
            .unwrap();
        state.initial_state_captured = true;
        let action_id = state.capture_action(
            "click",
            vec![Step::Pointer {
                target: Target {
                    selectors: vec![SelectorKind::Css {
                        value: "#open".to_string(),
                    }],
                    input_type: None,
                    verified: true,
                },
                kind: ClickKind::Click,
                pointer: PointerKind::Mouse,
                button: "left".to_string(),
                count: 1,
                position: None,
                opens_popup: false,
                popup_page: None,
                scope: CodegenState::scope_for_page(&identity),
                asserted_url: None,
            }],
        );
        state.register_action_intent(action_id, "p1");
        let mut popup = page(2, "popup-a", "session-b", "https://example.com/same");
        popup.opener_id = Some("opener".to_string());
        state.sync_runtime_pages(&[opener, popup], Some("opener"), false);
        state.bind_unattributed_popups();

        assert!(matches!(
            state.steps.last(),
            Some(Step::Pointer { popup_page: Some(page_id), .. }) if page_id == "p2"
        ));

        let action_id = state.capture_action(
            "click",
            vec![Step::Pointer {
                target: Target {
                    selectors: vec![SelectorKind::Css {
                        value: "#open-again".to_string(),
                    }],
                    input_type: None,
                    verified: true,
                },
                kind: ClickKind::Click,
                pointer: PointerKind::Mouse,
                button: "left".to_string(),
                count: 1,
                position: None,
                opens_popup: false,
                popup_page: None,
                scope: CodegenState::scope_for_page(&identity),
                asserted_url: None,
            }],
        );
        state.register_action_intent(action_id, "p1");
        let mut second_popup = page(3, "popup-b", "session-c", "https://example.com/same");
        second_popup.opener_id = Some("opener".to_string());
        state.sync_runtime_pages(
            &[
                page(1, "opener", "session-a", "https://example.com"),
                page(2, "popup-a", "session-b", "https://example.com/same"),
                second_popup,
            ],
            Some("opener"),
            false,
        );
        state.bind_unattributed_popups();

        assert!(matches!(
            state.steps.last(),
            Some(Step::Pointer { popup_page: Some(page_id), .. }) if page_id == "p3"
        ));
    }

    #[test]
    fn ambiguous_popup_origin_is_warned_and_not_guessed() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        let identity = state
            .sync_runtime_pages(
                &[page(1, "opener", "session-a", "https://example.com")],
                Some("opener"),
                false,
            )
            .unwrap();
        state.initial_state_captured = true;
        for selector in ["#one", "#two"] {
            let action_id = state.capture_action(
                "click",
                vec![Step::Pointer {
                    target: Target {
                        selectors: vec![SelectorKind::Css {
                            value: selector.to_string(),
                        }],
                        input_type: None,
                        verified: true,
                    },
                    kind: ClickKind::Click,
                    pointer: PointerKind::Mouse,
                    button: "left".to_string(),
                    count: 1,
                    position: None,
                    opens_popup: false,
                    popup_page: None,
                    scope: CodegenState::scope_for_page(&identity),
                    asserted_url: None,
                }],
            );
            state.register_action_intent(action_id, "p1");
        }
        let mut popup = page(2, "popup", "session-b", "about:blank");
        popup.opener_id = Some("opener".to_string());
        state.sync_runtime_pages(
            &[page(1, "opener", "session-a", "https://example.com"), popup],
            Some("opener"),
            false,
        );
        state.bind_unattributed_popups();

        assert!(state
            .capture_errors
            .iter()
            .any(|warning| warning.starts_with("ambiguous-popup-origin:")));
        assert!(state.steps.iter().all(|step| !matches!(
            step,
            Step::Pointer {
                popup_page: Some(_),
                ..
            }
        )));
    }

    #[test]
    fn main_frame_and_same_document_events_update_a_press_assertion() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        let identity = state
            .sync_runtime_pages(
                &[page(1, "target-a", "session-a", "https://example.com")],
                Some("target-a"),
                false,
            )
            .unwrap();
        let action_id = state.capture_action(
            "press",
            vec![Step::Press {
                modifiers: Vec::new(),
                key: "Enter".to_string(),
                scope: CodegenState::scope_for_page(&identity),
                asserted_url: None,
            }],
        );
        state.register_action_intent(action_id, "p1");

        assert!(state.observe_cdp_navigation(
            "session-a",
            "main-frame",
            "https://example.com/next",
            true,
        ));
        assert!(state.observe_cdp_navigation(
            "session-a",
            "main-frame",
            "https://example.com/next#done",
            false,
        ));
        assert!(!state.observe_cdp_navigation(
            "session-a",
            "child-frame",
            "https://example.com/child",
            false,
        ));
        assert!(matches!(
            state.steps.last(),
            Some(Step::Press { asserted_url: Some(url), .. })
                if url == "https://example.com/next#done"
        ));
    }

    #[test]
    fn recorder_prefers_the_captured_url_and_falls_back_to_the_final_url() {
        let pages = vec![sidecar::PersistedPage {
            page_id: "p2".to_string(),
            target_id: None,
            opener_target_id: None,
            popup_attributed: true,
            url: "https://example.com/final".to_string(),
            closed: false,
            url_unrecorded: false,
        }];
        let scope = |page_url: Option<&str>| Scope {
            target: "p2".to_string(),
            frame: Vec::new(),
            page_url: page_url.map(str::to_string),
        };

        let captured = render_recorder(
            "flow",
            &[Step::ClosePage {
                scope: scope(Some("https://example.com/at-capture")),
            }],
            &pages,
        );
        assert_eq!(
            captured.flow["steps"][0]["target"],
            "https://example.com/at-capture"
        );

        // A journal written before capture stored the URL has no value here.
        let older = render_recorder("flow", &[Step::ClosePage { scope: scope(None) }], &pages);
        assert_eq!(
            older.flow["steps"][0]["target"],
            "https://example.com/final"
        );
    }

    #[test]
    fn an_unrecorded_page_move_warns_and_forces_the_next_navigate() {
        assert!(steps::action_is_omitted("webmcp_invoke"));
        assert!(!steps::action_is_omitted("webmcp_list"));
        assert!(!steps::action_is_omitted("webmcp_result"));

        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        state.viewport_emitted = true;
        state.sync_runtime_pages(
            &[page(
                1,
                "target-a",
                "session-a",
                "https://example.com/start",
            )],
            Some("target-a"),
            false,
        );

        state.observe_unrecorded_navigation("p1", "https://example.com/moved", "webmcp_invoke");
        assert!(state.page_url_is_unrecorded("p1"));
        assert_eq!(state.page_url("p1"), Some("https://example.com/moved"));

        let warning = state
            .capture_errors
            .iter()
            .find(|warning| warning.starts_with("unrecorded-navigation:"))
            .expect("the move should warn");
        assert!(
            !warning.contains("https://example.com/moved"),
            "the warning must not carry the URL: {warning}"
        );

        // The flow has no step that reaches the page, so an equal URL is not a
        // reason to drop the navigate.
        record_action(
            "navigate",
            &json!({ "url": "https://example.com/moved" }),
            &json!({}),
            &crate::native::element::RefMap::new(),
            None,
            Scope {
                target: "p1".to_string(),
                frame: Vec::new(),
                page_url: None,
            },
            None,
            &mut state,
        )
        .unwrap();
        assert!(matches!(
            state.steps.last(),
            Some(Step::ScopedNavigation { url, .. }) if url == "https://example.com/moved"
        ));
        assert!(!state.page_url_is_unrecorded("p1"));
    }

    #[test]
    fn history_commands_do_not_clear_an_unrecorded_page_url() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        state.viewport_emitted = true;
        state.sync_runtime_pages(
            &[page(
                1,
                "target-a",
                "session-a",
                "https://example.com/start",
            )],
            Some("target-a"),
            false,
        );
        state.observe_unrecorded_navigation("p1", "https://example.com/moved", "evaluate");

        for action in ["back", "forward", "reload"] {
            record_action(
                action,
                &json!({}),
                &json!({ "url": "https://example.com/moved" }),
                &crate::native::element::RefMap::new(),
                None,
                Scope {
                    target: "p1".to_string(),
                    frame: Vec::new(),
                    page_url: None,
                },
                None,
                &mut state,
            )
            .unwrap();
            assert!(
                state.page_url_is_unrecorded("p1"),
                "{action} must not prove that the flow reached the page"
            );
        }
    }

    #[test]
    fn a_detached_page_call_marks_the_next_unattributed_move() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = restored_state(&directory);
        state.sync_runtime_pages(
            &[page(
                1,
                "target-a",
                "session-a",
                "https://example.com/start",
            )],
            Some("target-a"),
            false,
        );

        // Without the mark, an unattributed URL change is ordinary page motion.
        state.observe_navigation("p1", "https://example.com/one");
        assert!(!state.page_url_is_unrecorded("p1"));

        state.expect_unattributed_page_effect("p1");
        state.observe_navigation("p1", "https://example.com/two");
        assert!(state.page_url_is_unrecorded("p1"));
        assert_eq!(
            state
                .capture_errors
                .iter()
                .filter(|warning| warning.starts_with("unrecorded-navigation:"))
                .count(),
            1,
            "the mark is consumed once"
        );
    }

    #[test]
    fn a_journal_without_the_unrecorded_field_recovers_as_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        std::fs::write(&path, "").unwrap();
        sidecar::append(
            &path,
            1,
            &sidecar::JournalRecord::Start {
                title: "flow".to_string(),
                last_url: None,
            },
        )
        .unwrap();
        // An older development journal has no `urlUnrecorded` field.
        let record = sidecar::JournalRecord::Pages(sidecar::PersistedPageState {
            pages: vec![sidecar::PersistedPage {
                page_id: "p1".to_string(),
                target_id: Some("target-a".to_string()),
                opener_target_id: None,
                popup_attributed: true,
                url: "https://example.com/start".to_string(),
                closed: false,
                url_unrecorded: true,
            }],
            next_page_id: 2,
            start_page_id: Some("p1".to_string()),
            last_active_page_id: Some("p1".to_string()),
            initial_state_captured: true,
        });
        let mut envelope = serde_json::to_value(sidecar::JournalEnvelope {
            version: 1,
            sequence: 2,
            record,
        })
        .unwrap();
        assert!(envelope["data"]["pages"][0]
            .as_object_mut()
            .unwrap()
            .remove("urlUnrecorded")
            .is_some());
        let mut line = serde_json::to_string(&envelope).unwrap();
        line.push('\n');
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();

        let state = CodegenState::from_recovered(path.clone(), sidecar::recover(&path).unwrap());
        assert_eq!(state.page_url("p1"), Some("https://example.com/start"));
        assert!(!state.page_url_is_unrecorded("p1"));
    }
}
