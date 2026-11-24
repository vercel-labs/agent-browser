//! MCP Elicitation (form mode) across both protocol eras:
//! - `2026-07-28`: Multi Round-Trip Requests (`InputRequiredResult` with
//!   HMAC-SHA256-signed single-use `requestState`) and mid-flight task
//!   `input_required` via `tasks/update`.
//! - `2025-11-25` / `2025-06-18`: Server-initiated `elicitation/create`
//!   requests over stdio (with `_meta["io.modelcontextprotocol/related-task"]`
//!   when invoked inside a task).
//!
//! Form mode is strictly limited to explicit confirmations and non-sensitive
//! metadata; passwords, API keys, tokens, and payment credentials are never
//! requested or echoed in form-mode prompts.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::mcp_transport::ProtocolError;

type HmacSha256 = Hmac<Sha256>;

pub(crate) const ELICITATION_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const MAX_CHAINED_CONFIRMATIONS: usize = 8;
const REQUEST_STATE_TTL_SECS: u64 = 60;

/// Result of classifying a tool call (including `agent_browser_batch` subcommands)
/// against enabled `--elicit` categories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreflightMatch {
    pub(crate) category: ElicitCategory,
    pub(crate) message: String,
}

/// Confirmation categories supported by `--elicit` / `AGENT_BROWSER_MCP_ELICIT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ElicitCategory {
    Download,
    Upload,
    Eval,
    Auth,
    Submit,
    State,
}

impl ElicitCategory {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Upload => "upload",
            Self::Eval => "eval",
            Self::Auth => "auth",
            Self::Submit => "submit",
            Self::State => "state",
        }
    }

    pub(crate) fn parse_list(raw: &str) -> Result<BTreeSet<Self>, String> {
        let mut out = BTreeSet::new();
        for token in raw.split(',') {
            let name = token.trim().to_ascii_lowercase();
            if name.is_empty() {
                continue;
            }
            match name.as_str() {
                "high-risk" => {
                    out.insert(Self::Submit);
                    out.insert(Self::Download);
                    out.insert(Self::Upload);
                    out.insert(Self::Eval);
                    out.insert(Self::Auth);
                }
                "download" => {
                    out.insert(Self::Download);
                }
                "upload" => {
                    out.insert(Self::Upload);
                }
                "eval" => {
                    out.insert(Self::Eval);
                }
                "auth" => {
                    out.insert(Self::Auth);
                }
                "submit" => {
                    out.insert(Self::Submit);
                }
                "state" => {
                    out.insert(Self::State);
                }
                _ => {
                    return Err(format!(
                        "Unknown --elicit category: {}\nValid categories: high-risk, submit, download, upload, eval, auth, state",
                        token.trim()
                    ));
                }
            }
        }
        if out.is_empty() {
            return Err("Missing value for --elicit".to_string());
        }
        Ok(out)
    }
}

/// High-impact submit/purchase/destructive keywords for the `submit` category.
const SUBMIT_KEYWORDS: &[&str] = &[
    "pay",
    "purchase",
    "buy",
    "checkout",
    "place order",
    "place-order",
    "place_order",
    "submit",
    "delete",
    "remove",
    "send",
    "transfer",
    "confirm",
    "authorize",
    "sign out",
    "sign-out",
    "signout",
];

/// Classify a tool call (including every subcommand inside `agent_browser_batch`)
/// against enabled `--elicit` categories, returning the first matched category
/// and a safe confirmation prompt covering the whole call/batch.
pub(crate) fn classify_preflight_gate(
    enabled: &BTreeSet<ElicitCategory>,
    tool_name: &str,
    arguments: &Value,
) -> Option<PreflightMatch> {
    if enabled.is_empty() {
        return None;
    }
    if tool_name == "agent_browser_batch" {
        return classify_batch_preflight(enabled, arguments);
    }
    let category = classify_preflight_category(enabled, tool_name, arguments)?;
    let message = preflight_prompt_message(category, tool_name, arguments);
    Some(PreflightMatch { category, message })
}

fn classify_batch_preflight(
    enabled: &BTreeSet<ElicitCategory>,
    arguments: &Value,
) -> Option<PreflightMatch> {
    let commands = arguments.get("commands").and_then(Value::as_array)?;
    let flags = crate::flags::parse_flags(&[]);
    let mut matches: Vec<(usize, ElicitCategory, String)> = Vec::new();

    for (index, item) in commands.iter().enumerate() {
        let Some(tokens) = item.as_array() else {
            continue;
        };
        let sub_args: Vec<String> = tokens
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect();
        if sub_args.is_empty() {
            continue;
        }
        if let Some((cat, summary)) = classify_batch_subcommand(enabled, &sub_args, &flags) {
            matches.push((index + 1, cat, summary));
        }
    }

    if matches.is_empty() {
        return None;
    }

    let first_category = matches[0].1;
    let listed: Vec<String> = matches
        .iter()
        .take(5)
        .map(|(step, cat, summary)| format!("#{} [{}] {}", step, cat.as_str(), summary))
        .collect();
    let extra = if matches.len() > 5 {
        format!(" (+{} more)", matches.len() - 5)
    } else {
        String::new()
    };
    let message = format!(
        "Allow agent-browser batch ({} command(s)) containing gated operation(s): {}{}?",
        commands.len(),
        listed.join("; "),
        extra
    );
    Some(PreflightMatch {
        category: first_category,
        message,
    })
}

fn classify_batch_subcommand(
    enabled: &BTreeSet<ElicitCategory>,
    sub_args: &[String],
    flags: &crate::flags::Flags,
) -> Option<(ElicitCategory, String)> {
    // 1. Canonical CLI batch parser classification.
    if let Ok(parsed) = crate::commands::parse_batch_command(sub_args, flags) {
        let action = parsed.get("action").and_then(Value::as_str).unwrap_or("");
        let matched = match action {
            "download" | "waitfordownload" => Some((ElicitCategory::Download, action.to_string())),
            "upload" => Some((ElicitCategory::Upload, "upload".to_string())),
            "evaluate" | "evalhandle" | "addscript" | "addinitscript" | "addstyle" | "expose"
            | "setcontent" => {
                let preview = parsed
                    .get("script")
                    .and_then(Value::as_str)
                    .map(|s| format!("{} `{}`", action, truncate_preview(s, 48)))
                    .unwrap_or_else(|| action.to_string());
                Some((ElicitCategory::Eval, preview))
            }
            "auth_login" | "auth_save" | "auth_delete" | "credentials" => {
                // Never include passwords or secret fields in batch summaries.
                let name = parsed
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|n| format!(" '{}'", truncate_preview(n, 32)))
                    .unwrap_or_default();
                Some((ElicitCategory::Auth, format!("{}{}", action, name)))
            }
            "cookies_set" | "storage_set" | "state_load" => {
                Some((ElicitCategory::State, action.to_string()))
            }
            "click" | "dblclick" | "tap" => {
                let sel = parsed.get("selector").and_then(Value::as_str);
                if selector_looks_like_submit(sel) {
                    Some((
                        ElicitCategory::Submit,
                        format!("{} '{}'", action, truncate_preview(sel.unwrap_or(""), 48)),
                    ))
                } else {
                    None
                }
            }
            "press" | "key" => {
                let is_enter = parsed.get("key").and_then(Value::as_str).is_some_and(|k| {
                    k.eq_ignore_ascii_case("enter") || k.eq_ignore_ascii_case("return")
                });
                if is_enter {
                    Some((ElicitCategory::Submit, "press Enter".to_string()))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some((cat, summary)) = matched {
            if enabled.contains(&cat) {
                return Some((cat, summary));
            }
            return None;
        }
        return None;
    }

    // 2. Fallback token classification for any subcommand form the static parser
    // did not parse (e.g. missing optional flags or aliases) so nothing bypasses.
    let first = sub_args[0].to_ascii_lowercase();
    let second = sub_args
        .get(1)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let fallback = match (first.as_str(), second.as_str()) {
        ("download", _) => Some((ElicitCategory::Download, "download".to_string())),
        ("wait", _) if sub_args.iter().any(|a| a == "--download") => {
            Some((ElicitCategory::Download, "wait --download".to_string()))
        }
        ("upload", _) => Some((ElicitCategory::Upload, "upload".to_string())),
        ("eval" | "evaluate" | "evalhandle" | "addscript" | "addinitscript" | "setcontent", _) => {
            Some((ElicitCategory::Eval, first))
        }
        ("auth", "login" | "save" | "delete") => {
            Some((ElicitCategory::Auth, format!("auth {}", second)))
        }
        ("set", "credentials") => Some((ElicitCategory::Auth, "set credentials".to_string())),
        ("cookies", "set") | ("storage", "set") | ("state", "load") => {
            Some((ElicitCategory::State, format!("{} {}", first, second)))
        }
        ("click" | "dblclick" | "tap", _)
            if sub_args
                .iter()
                .skip(1)
                .any(|a| selector_looks_like_submit(Some(a))) =>
        {
            Some((ElicitCategory::Submit, first))
        }
        ("press" | "key", _)
            if sub_args
                .iter()
                .skip(1)
                .any(|a| a.eq_ignore_ascii_case("enter") || a.eq_ignore_ascii_case("return")) =>
        {
            Some((ElicitCategory::Submit, "press Enter".to_string()))
        }
        _ => None,
    };
    fallback.filter(|(cat, _)| enabled.contains(cat))
}

/// Determine whether a single non-batch tool call matches an enabled `--elicit` category.
pub(crate) fn classify_preflight_category(
    enabled: &BTreeSet<ElicitCategory>,
    tool_name: &str,
    arguments: &Value,
) -> Option<ElicitCategory> {
    if enabled.is_empty() {
        return None;
    }
    let candidate = match tool_name {
        "agent_browser_download" | "agent_browser_wait_for_download" => {
            Some(ElicitCategory::Download)
        }
        "agent_browser_upload" => Some(ElicitCategory::Upload),
        "agent_browser_eval" => Some(ElicitCategory::Eval),
        "agent_browser_auth_login"
        | "agent_browser_auth_save"
        | "agent_browser_auth_delete"
        | "agent_browser_set_credentials" => Some(ElicitCategory::Auth),
        "agent_browser_cookies_set"
        | "agent_browser_cookies_set_curl"
        | "agent_browser_storage_set"
        | "agent_browser_state_load" => Some(ElicitCategory::State),
        "agent_browser_click" | "agent_browser_dblclick" | "agent_browser_tap" => {
            if enabled.contains(&ElicitCategory::Submit)
                && selector_looks_like_submit(arguments.get("selector").and_then(Value::as_str))
            {
                Some(ElicitCategory::Submit)
            } else {
                None
            }
        }
        "agent_browser_press" => {
            let is_enter = arguments
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|k| {
                    k.eq_ignore_ascii_case("enter") || k.eq_ignore_ascii_case("return")
                });
            if enabled.contains(&ElicitCategory::Submit) && is_enter {
                Some(ElicitCategory::Submit)
            } else {
                None
            }
        }
        _ => None,
    };
    candidate.filter(|c| enabled.contains(c))
}

pub(crate) fn selector_looks_like_submit(selector: Option<&str>) -> bool {
    let Some(raw) = selector else {
        return false;
    };
    let lower = raw.to_ascii_lowercase();
    SUBMIT_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Build a safe, human-readable confirmation message for a pre-flight gate.
/// Never includes passwords, tokens, or secrets.
pub(crate) fn preflight_prompt_message(
    category: ElicitCategory,
    tool_name: &str,
    arguments: &Value,
) -> String {
    match category {
        ElicitCategory::Download => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("(default download path)");
            let selector = arguments
                .get("selector")
                .and_then(Value::as_str)
                .map(|s| format!(" via '{}'", truncate_preview(s, 60)))
                .unwrap_or_default();
            format!(
                "Allow agent-browser to download a file{} to '{}'?",
                selector,
                truncate_preview(path, 120)
            )
        }
        ElicitCategory::Upload => {
            let selector = arguments
                .get("selector")
                .and_then(Value::as_str)
                .unwrap_or("(element)");
            let files = arguments
                .get("files")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "(files)".to_string());
            format!(
                "Allow agent-browser to upload [{}] to '{}'?",
                truncate_preview(&files, 120),
                truncate_preview(selector, 60)
            )
        }
        ElicitCategory::Eval => {
            let script = arguments
                .get("script")
                .and_then(Value::as_str)
                .unwrap_or("");
            format!(
                "Allow agent-browser to evaluate JavaScript in the page: `{}`?",
                truncate_preview(script, 120)
            )
        }
        ElicitCategory::Auth => {
            // Never include password or secret fields in form-mode prompts.
            let profile = arguments
                .get("name")
                .and_then(Value::as_str)
                .map(|n| format!(" profile '{}'", truncate_preview(n, 60)))
                .unwrap_or_default();
            let url = arguments
                .get("url")
                .and_then(Value::as_str)
                .map(|u| format!(" for {}", truncate_preview(u, 80)))
                .unwrap_or_default();
            let user = arguments
                .get("username")
                .and_then(Value::as_str)
                .map(|u| format!(" (username '{}')", truncate_preview(u, 60)))
                .unwrap_or_default();
            format!(
                "Allow agent-browser to run '{}'{}?{}{}",
                tool_name, profile, url, user
            )
        }
        ElicitCategory::Submit => {
            if tool_name == "agent_browser_press" {
                "Allow agent-browser to press Enter (possible form submission)?".to_string()
            } else {
                let selector = arguments
                    .get("selector")
                    .and_then(Value::as_str)
                    .unwrap_or("(element)");
                format!(
                    "Allow agent-browser to activate '{}' (possible form submission or purchase)?",
                    truncate_preview(selector, 80)
                )
            }
        }
        ElicitCategory::State => {
            format!(
                "Allow agent-browser to modify browser state via '{}'?",
                tool_name
            )
        }
    }
}

/// Build a safe, human-readable message for a parked daemon confirmation.
pub(crate) fn daemon_confirmation_message(
    action: &str,
    tool_name: &str,
    confirmation_id: &str,
) -> String {
    format!(
        "Allow agent-browser to perform '{}' via '{}' (confirmation {})?",
        truncate_preview(action, 80),
        tool_name,
        truncate_preview(confirmation_id, 40)
    )
}

/// Extract `(confirmation_id, action)` if a `CallToolResult` contains a parked
/// daemon confirmation, either at `structuredContent.response.data` or nested
/// inside a confirmed replay's `data.result.data` (for chained confirmations).
pub(crate) fn extract_daemon_confirmation(call_result: &Value) -> Option<(String, String)> {
    let mut current = call_result
        .get("structuredContent")?
        .get("response")?
        .get("data")?;
    for _ in 0..4 {
        if current
            .get("confirmation_required")
            .and_then(Value::as_bool)
            == Some(true)
        {
            let id = current
                .get("confirmation_id")
                .and_then(Value::as_str)?
                .to_string();
            let action = current
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("action")
                .to_string();
            return Some((id, action));
        }
        let Some(nested_data) = current.get("result").and_then(|r| r.get("data")) else {
            break;
        };
        current = nested_data;
    }
    None
}

/// Construct the flat `elicitation/create` params object (form mode).
pub(crate) fn build_confirmation_elicit_params(message: &str) -> Value {
    json!({
        "mode": "form",
        "message": message,
        "requestedSchema": {
            "type": "object",
            "properties": {
                "approve": {
                    "type": "boolean",
                    "title": "Approve action",
                    "description": "Allow this browser action to proceed.",
                    "default": false
                }
            },
            "required": ["approve"]
        }
    })
}

/// Construct an `InputRequests` entry (`{ method: "elicitation/create", params }`).
pub(crate) fn build_confirmation_input_request(message: &str) -> Value {
    json!({
        "method": "elicitation/create",
        "params": build_confirmation_elicit_params(message)
    })
}

/// Parsed decision from an `ElicitResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElicitDecision {
    Approved,
    Declined,
    Cancelled,
}

/// Evaluate an `ElicitResult` JSON object (`{ action, content? }`).
pub(crate) fn parse_elicit_decision(result: &Value) -> Result<ElicitDecision, ProtocolError> {
    let action = result
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| ProtocolError::invalid_params("Elicitation result missing string action"))?;
    match action {
        "accept" => {
            let approved = result
                .get("content")
                .and_then(Value::as_object)
                .and_then(|c| c.get("approve"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if approved {
                Ok(ElicitDecision::Approved)
            } else {
                Ok(ElicitDecision::Declined)
            }
        }
        "decline" => Ok(ElicitDecision::Declined),
        "cancel" => Ok(ElicitDecision::Cancelled),
        other => Err(ProtocolError::invalid_params(format!(
            "Invalid elicitation action: {}",
            other
        ))),
    }
}

pub(crate) fn missing_elicitation_capability_error() -> ProtocolError {
    ProtocolError::missing_required_client_capability(
        "Client must declare form-mode elicitation capability to approve this action",
        json!({
            "elicitation": {
                "form": {}
            }
        }),
    )
}

pub(crate) fn refusal_tool_result(reason: &str, decision: &str) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": reason,
        }],
        "structuredContent": {
            "elicitation": {
                "decision": decision,
                "reason": reason,
            }
        },
        "isError": true,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RequestStateKind {
    Preflight {
        category: String,
    },
    DaemonConfirm {
        confirmation_id: String,
        action: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SignedStatePayload {
    pub(crate) key: String,
    pub(crate) tool_name: String,
    pub(crate) args_sha256: String,
    pub(crate) session: String,
    pub(crate) namespace: Option<String>,
    pub(crate) expires_at_secs: u64,
    pub(crate) nonce: String,
    pub(crate) kind: RequestStateKind,
}

/// Resolve the effective `(session, namespace)` binding for a tool call so an
/// approval token cannot be replayed against a different session or namespace.
pub(crate) fn effective_session_binding(arguments: &Value) -> (String, Option<String>) {
    let session = arguments
        .get("session")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            std::env::var("AGENT_BROWSER_SESSION")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "default".to_string());
    let namespace = arguments
        .get("namespace")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            std::env::var("AGENT_BROWSER_NAMESPACE")
                .ok()
                .filter(|s| !s.is_empty())
        });
    (session, namespace)
}

/// Per-process HMAC-SHA256 signer and single-use nonce tracker for MRTR `requestState`.
pub(crate) struct ElicitationState {
    hmac_key: [u8; 32],
    /// Maps unconsumed `nonce -> expires_at_secs` to guarantee single-use.
    pending_nonces: Mutex<HashMap<String, u64>>,
}

impl ElicitationState {
    pub(crate) fn new() -> Self {
        let mut key = [0u8; 32];
        if getrandom::getrandom(&mut key).is_err() {
            // Fallback entropy from UUID v4 bytes if OS RNG ever fails.
            let a = Uuid::new_v4();
            let b = Uuid::new_v4();
            key[..16].copy_from_slice(a.as_bytes());
            key[16..].copy_from_slice(b.as_bytes());
        }
        Self {
            hmac_key: key,
            pending_nonces: Mutex::new(HashMap::new()),
        }
    }

    /// Create an MRTR `InputRequiredResult` with an HMAC-SHA256-protected,
    /// single-use `requestState` token bound to `(tool_name, arguments)`.
    pub(crate) fn issue_input_required(
        &self,
        tool_name: &str,
        arguments: &Value,
        message: &str,
        kind: RequestStateKind,
    ) -> Value {
        let key = format!("confirm_{}", Uuid::new_v4().simple());
        let nonce = Uuid::new_v4().simple().to_string();
        let expires_at_secs = unix_now_secs().saturating_add(REQUEST_STATE_TTL_SECS);
        let (session, namespace) = effective_session_binding(arguments);
        let payload = SignedStatePayload {
            key: key.clone(),
            tool_name: tool_name.to_string(),
            args_sha256: digest_json(arguments),
            session,
            namespace,
            expires_at_secs,
            nonce: nonce.clone(),
            kind,
        };

        {
            let mut nonces = self
                .pending_nonces
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let now = unix_now_secs();
            nonces.retain(|_, exp| *exp > now);
            nonces.insert(nonce, expires_at_secs);
        }

        let token = self.sign_payload(&payload);
        json!({
            "resultType": "input_required",
            "inputRequests": {
                key: build_confirmation_input_request(message)
            },
            "requestState": token,
        })
    }

    /// Verify and consume a client-supplied MRTR `requestState` token.
    pub(crate) fn verify_and_consume(
        &self,
        token: &str,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<SignedStatePayload, ProtocolError> {
        let Some((payload_b64, sig_b64)) = token.split_once('.') else {
            return Err(ProtocolError::invalid_params("Invalid requestState format"));
        };
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| ProtocolError::invalid_params("Invalid requestState encoding"))?;
        let sig_bytes = URL_SAFE_NO_PAD.decode(sig_b64).map_err(|_| {
            ProtocolError::invalid_params("Invalid requestState signature encoding")
        })?;

        let mut mac =
            HmacSha256::new_from_slice(&self.hmac_key).expect("HMAC accepts keys of any size");
        mac.update(&payload_bytes);
        mac.verify_slice(&sig_bytes)
            .map_err(|_| ProtocolError::invalid_params("Invalid requestState signature"))?;

        let payload: SignedStatePayload = serde_json::from_slice(&payload_bytes)
            .map_err(|_| ProtocolError::invalid_params("Malformed requestState payload"))?;

        if unix_now_secs() > payload.expires_at_secs {
            return Err(ProtocolError::invalid_params("requestState has expired"));
        }
        let (current_session, current_namespace) = effective_session_binding(arguments);
        if payload.tool_name != tool_name
            || payload.args_sha256 != digest_json(arguments)
            || payload.session != current_session
            || payload.namespace != current_namespace
        {
            return Err(ProtocolError::invalid_params(
                "requestState does not match the retried tool call or session",
            ));
        }

        let mut nonces = self
            .pending_nonces
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if nonces.remove(&payload.nonce).is_none() {
            return Err(ProtocolError::invalid_params(
                "requestState was already used or is unknown",
            ));
        }

        Ok(payload)
    }

    fn sign_payload(&self, payload: &SignedStatePayload) -> String {
        let payload_bytes = serde_json::to_vec(payload).expect("serializable payload");
        let mut mac =
            HmacSha256::new_from_slice(&self.hmac_key).expect("HMAC accepts keys of any size");
        mac.update(&payload_bytes);
        let sig = mac.finalize().into_bytes();
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload_bytes),
            URL_SAFE_NO_PAD.encode(sig)
        )
    }
}

pub(crate) fn digest_json(value: &Value) -> String {
    let canonical = canonical_json_string(value);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex::encode(hasher.finalize())
}

fn canonical_json_string(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json_string(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(canonical_json_string).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    if clean.chars().count() <= max_chars {
        clean
    } else {
        let prefix: String = clean.chars().take(max_chars).collect();
        format!("{}...", prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_elicit_categories_expands_high_risk_and_rejects_unknown() {
        let cats = ElicitCategory::parse_list("high-risk").unwrap();
        assert!(cats.contains(&ElicitCategory::Submit));
        assert!(cats.contains(&ElicitCategory::Download));
        assert!(cats.contains(&ElicitCategory::Upload));
        assert!(cats.contains(&ElicitCategory::Eval));
        assert!(cats.contains(&ElicitCategory::Auth));
        assert!(!cats.contains(&ElicitCategory::State));

        let err = ElicitCategory::parse_list("bogus").unwrap_err();
        assert!(err.contains("Unknown --elicit category"));
    }

    #[test]
    fn preflight_classification_and_prompt_never_leak_passwords() {
        let cats = ElicitCategory::parse_list("high-risk").unwrap();
        assert_eq!(
            classify_preflight_category(&cats, "agent_browser_download", &json!({})),
            Some(ElicitCategory::Download)
        );
        assert_eq!(
            classify_preflight_category(
                &cats,
                "agent_browser_click",
                &json!({ "selector": "button#checkout-buy" })
            ),
            Some(ElicitCategory::Submit)
        );
        assert_eq!(
            classify_preflight_category(
                &cats,
                "agent_browser_click",
                &json!({ "selector": "a.nav-docs" })
            ),
            None
        );

        let auth_prompt = preflight_prompt_message(
            ElicitCategory::Auth,
            "agent_browser_auth_save",
            &json!({
                "name": "github",
                "url": "https://github.com/login",
                "username": "octocat",
                "password": "SUPER_SECRET_PASSWORD_123"
            }),
        );
        assert!(auth_prompt.contains("github"));
        assert!(auth_prompt.contains("octocat"));
        assert!(!auth_prompt.contains("SUPER_SECRET_PASSWORD_123"));
    }

    #[test]
    fn signed_request_state_rejects_tamper_mismatch_and_replay() {
        let state = ElicitationState::new();
        let args = json!({ "path": "/tmp/a.pdf", "selector": "#dl" });
        let req = state.issue_input_required(
            "agent_browser_download",
            &args,
            "Allow download?",
            RequestStateKind::Preflight {
                category: "download".to_string(),
            },
        );
        assert_eq!(req["resultType"], "input_required");
        let token = req["requestState"].as_str().unwrap().to_string();

        // Tampered token fails HMAC check.
        let mut tampered = token.clone();
        tampered.push('X');
        assert_eq!(
            state
                .verify_and_consume(&tampered, "agent_browser_download", &args)
                .unwrap_err()
                .code,
            -32602
        );

        // Different tool or arguments fails binding check.
        assert_eq!(
            state
                .verify_and_consume(
                    &token,
                    "agent_browser_download",
                    &json!({ "path": "/tmp/other.pdf", "selector": "#dl" })
                )
                .unwrap_err()
                .code,
            -32602
        );

        // Equivalent object key order succeeds once.
        let reordered = json!({ "selector": "#dl", "path": "/tmp/a.pdf" });
        let verified = state
            .verify_and_consume(&token, "agent_browser_download", &reordered)
            .unwrap();
        assert_eq!(verified.tool_name, "agent_browser_download");

        // Replay of the consumed token fails.
        assert_eq!(
            state
                .verify_and_consume(&token, "agent_browser_download", &args)
                .unwrap_err()
                .code,
            -32602
        );
    }

    #[test]
    fn parse_elicit_decision_handles_accept_decline_cancel() {
        assert_eq!(
            parse_elicit_decision(&json!({ "action": "accept", "content": { "approve": true } }))
                .unwrap(),
            ElicitDecision::Approved
        );
        assert_eq!(
            parse_elicit_decision(&json!({ "action": "accept", "content": { "approve": false } }))
                .unwrap(),
            ElicitDecision::Declined
        );
        assert_eq!(
            parse_elicit_decision(&json!({ "action": "decline" })).unwrap(),
            ElicitDecision::Declined
        );
        assert_eq!(
            parse_elicit_decision(&json!({ "action": "cancel" })).unwrap(),
            ElicitDecision::Cancelled
        );
    }

    #[test]
    fn batch_preflight_classifies_gated_subcommands_without_leaking_secrets() {
        let high_risk = ElicitCategory::parse_list("high-risk").unwrap();

        // Safe batch does not trigger elicitation.
        let safe_batch = json!({
            "commands": [
                ["open", "https://example.com"],
                ["snapshot", "-i"]
            ]
        });
        assert!(classify_preflight_gate(&high_risk, "agent_browser_batch", &safe_batch).is_none());

        // Batch containing eval, download, submit click, and auth save triggers elicitation
        // and never leaks passwords in the prompt message.
        let gated_batch = json!({
            "commands": [
                ["open", "https://example.com"],
                ["eval", "document.cookie"],
                ["download", "#dl", "/tmp/report.pdf"],
                ["click", "button#checkout-pay"],
                ["auth", "save", "work", "--url", "https://example.com/login", "--username", "alice", "--password", "SUPER_SECRET_BATCH_PW"]
            ]
        });
        let matched =
            classify_preflight_gate(&high_risk, "agent_browser_batch", &gated_batch).unwrap();
        assert_eq!(matched.category, ElicitCategory::Eval);
        assert!(matched.message.contains("eval"));
        assert!(matched.message.contains("download"));
        assert!(matched.message.contains("submit"));
        assert!(matched.message.contains("auth"));
        assert!(!matched.message.contains("SUPER_SECRET_BATCH_PW"));

        // State category inside batch is classified when `state` is enabled.
        let state_only = ElicitCategory::parse_list("state").unwrap();
        let state_batch = json!({
            "commands": [
                ["cookies", "set", "sid", "value"]
            ]
        });
        let state_match =
            classify_preflight_gate(&state_only, "agent_browser_batch", &state_batch).unwrap();
        assert_eq!(state_match.category, ElicitCategory::State);
    }

    #[test]
    fn extract_daemon_confirmation_finds_nested_chained_confirmations() {
        let nested = json!({
            "structuredContent": {
                "response": {
                    "success": true,
                    "data": {
                        "confirmed": true,
                        "action": "navigate",
                        "result": {
                            "success": true,
                            "data": {
                                "confirmation_required": true,
                                "confirmation_id": "c_second",
                                "action": "plugin:stealth:launch.mutate"
                            }
                        }
                    }
                }
            }
        });
        assert_eq!(
            extract_daemon_confirmation(&nested),
            Some((
                "c_second".to_string(),
                "plugin:stealth:launch.mutate".to_string()
            ))
        );
    }

    #[test]
    fn signed_request_state_binds_session_and_namespace() {
        let state = ElicitationState::new();
        let args = json!({ "script": "1+1", "session": "sess-a", "namespace": "ns-1" });
        let req = state.issue_input_required(
            "agent_browser_eval",
            &args,
            "Allow eval?",
            RequestStateKind::DaemonConfirm {
                confirmation_id: "c_1".to_string(),
                action: "eval".to_string(),
            },
        );
        let token = req["requestState"].as_str().unwrap();

        let wrong_session = json!({ "script": "1+1", "session": "sess-b", "namespace": "ns-1" });
        assert_eq!(
            state
                .verify_and_consume(token, "agent_browser_eval", &wrong_session)
                .unwrap_err()
                .code,
            -32602
        );

        let wrong_ns = json!({ "script": "1+1", "session": "sess-a", "namespace": "ns-2" });
        assert_eq!(
            state
                .verify_and_consume(token, "agent_browser_eval", &wrong_ns)
                .unwrap_err()
                .code,
            -32602
        );

        let ok = state
            .verify_and_consume(token, "agent_browser_eval", &args)
            .unwrap();
        assert_eq!(ok.session, "sess-a");
        assert_eq!(ok.namespace.as_deref(), Some("ns-1"));
    }
}
