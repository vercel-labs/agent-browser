use super::Step;
use crate::connection::get_socket_dir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const JOURNAL_VERSION: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapturedStep {
    pub step_id: u64,
    pub step: Step,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapturedAction {
    pub action_id: u64,
    pub action: String,
    pub steps: Vec<CapturedStep>,
}

/// Durable identity for one top-level page in a codegen flow. Runtime tab IDs
/// are intentionally not stored because a browser relaunch can reuse them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedPage {
    pub page_id: String,
    pub target_id: Option<String>,
    #[serde(default)]
    pub opener_target_id: Option<String>,
    #[serde(default)]
    pub popup_attributed: bool,
    pub url: String,
    pub closed: bool,
    /// A command that codegen cannot express moved this page. The flow does not
    /// contain a step that reaches the current URL, so the next explicit
    /// navigate must emit a step even when the URL looks unchanged.
    #[serde(default)]
    pub url_unrecorded: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedPageState {
    pub pages: Vec<PersistedPage>,
    pub next_page_id: u64,
    pub start_page_id: Option<String>,
    pub last_active_page_id: Option<String>,
    pub initial_state_captured: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum JournalRecord {
    Start {
        title: String,
        last_url: Option<String>,
    },
    Action(CapturedAction),
    UpdateAction(CapturedAction),
    State {
        last_url: Option<String>,
    },
    Pages(PersistedPageState),
    Degraded {
        message: String,
    },
    Warning {
        warning_id: u64,
        code: String,
        message: String,
        action_id: Option<u64>,
        step_id: Option<u64>,
    },
    ResolveWarning {
        warning_id: u64,
    },
    OutputWritten {
        format: String,
        path: Option<String>,
    },
    DiscardRequested,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JournalEnvelope {
    pub version: u32,
    pub sequence: u64,
    #[serde(flatten)]
    pub record: JournalRecord,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TerminalRecord {
    OutputWritten {
        format: String,
        path: Option<String>,
    },
    DiscardRequested,
}

#[derive(Clone, Debug)]
pub struct RecoveredJournal {
    pub title: String,
    pub last_url: Option<String>,
    pub page_state: PersistedPageState,
    pub actions: Vec<CapturedAction>,
    pub next_sequence: u64,
    pub next_warning_id: u64,
    pub degraded_messages: Vec<String>,
    pub warnings: Vec<String>,
    pub terminal: Option<TerminalRecord>,
    /// Byte offset just after the last record that recovery accepted. An
    /// unusable suffix can follow it, and the last record can lack its final
    /// newline, so `repair` must run before the next append.
    pub valid_bytes: u64,
}

pub fn path_for_session(session_id: &str) -> PathBuf {
    get_socket_dir().join(format!("{session_id}.codegen.jsonl"))
}

pub fn legacy_metadata_paths(path: &Path) -> [PathBuf; 2] {
    let session_path = path.with_extension("");
    [
        path.with_extension("codegen.meta.json"),
        session_path.with_extension("codegen.meta.json"),
    ]
}

fn ensure_regular_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "Failed to inspect codegen journal {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!(
            "Codegen journal is not a regular file: {}",
            path.display()
        ));
    }
    Ok(())
}

pub fn create(session_id: &str, title: &str) -> Result<(PathBuf, u64), String> {
    let directory = get_socket_dir();
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Failed to create codegen journal directory: {error}"))?;
    let path = path_for_session(session_id);
    if known_paths(&path)
        .iter()
        .any(|candidate| candidate.exists())
    {
        return Err("A codegen recording already exists for this session. Run `codegen status`, `codegen stop`, or `codegen discard`.".to_string());
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("Failed to create codegen journal: {error}"))?;
    ensure_regular_file(&path)?;
    drop(file);

    if let Err(error) = append(
        &path,
        1,
        &JournalRecord::Start {
            title: title.to_string(),
            last_url: None,
        },
    ) {
        return match fs::remove_file(&path) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}. The new journal also could not be removed: {cleanup_error}"
            )),
        };
    }
    Ok((path, 2))
}

pub fn append(path: &Path, sequence: u64, record: &JournalRecord) -> Result<(), String> {
    ensure_regular_file(path)?;
    let envelope = JournalEnvelope {
        version: JOURNAL_VERSION,
        sequence,
        record: record.clone(),
    };
    let line = serde_json::to_string(&envelope)
        .map_err(|error| format!("Failed to encode codegen journal record: {error}"))?;
    let mut options = OpenOptions::new();
    options.append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("Failed to open codegen journal: {error}"))?;
    writeln!(file, "{line}")
        .and_then(|_| file.flush())
        .map_err(|error| format!("Failed to append codegen journal record: {error}"))
}

/// Make a recovered journal safe to append to again.
///
/// `recover` accepts one unusable final line, but it cannot change the file.
/// Without this repair the next append joins its record to that suffix, and
/// every later recovery fails, including the one that `codegen stop` performs.
/// The repair only removes bytes that recovery already refused, and it never
/// rewrites an accepted record.
pub fn repair(path: &Path, valid_bytes: u64) -> Result<(), String> {
    ensure_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("Failed to open codegen journal: {error}"))?;
    let length = file
        .metadata()
        .map_err(|error| format!("Failed to inspect codegen journal: {error}"))?
        .len();
    if length > valid_bytes {
        file.set_len(valid_bytes)
            .map_err(|error| format!("Failed to repair codegen journal: {error}"))?;
    }
    if valid_bytes == 0 {
        return Ok(());
    }
    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(valid_bytes - 1))
        .and_then(|_| file.read_exact(&mut last))
        .map_err(|error| format!("Failed to read the codegen journal end: {error}"))?;
    if last[0] == b'\n' {
        return Ok(());
    }
    file.seek(SeekFrom::End(0))
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.flush())
        .map_err(|error| format!("Failed to repair codegen journal: {error}"))
}

pub fn recover(path: &Path) -> Result<RecoveredJournal, String> {
    ensure_regular_file(path)?;
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Failed to inspect codegen journal: {error}"))?;
    if metadata.len() > MAX_JOURNAL_BYTES {
        return Err(format!(
            "Codegen journal exceeds the 64 MiB recovery limit: {}",
            path.display()
        ));
    }
    // Read bytes, not text. Captured values are stored verbatim, so a crash
    // inside a multi-byte sequence must not make every earlier record
    // unreadable.
    let content =
        fs::read(path).map_err(|error| format!("Failed to read codegen journal: {error}"))?;
    let has_final_newline = content.last() == Some(&b'\n');
    let raw_lines: Vec<&[u8]> = content.split(|byte| *byte == b'\n').collect();
    let mut expected_sequence = 1u64;
    let mut title = None;
    let mut last_url = None;
    let mut page_state = PersistedPageState {
        next_page_id: 1,
        ..PersistedPageState::default()
    };
    let mut actions = BTreeMap::<u64, CapturedAction>::new();
    let mut degraded_messages = Vec::new();
    let mut warnings = BTreeMap::<u64, String>::new();
    let mut next_warning_id = 1u64;
    let mut terminal = None;

    let mut valid_bytes = 0u64;
    let mut cursor = 0u64;

    for (index, line) in raw_lines.iter().enumerate() {
        // `split` drops one newline for every element except the last, so the
        // last element is the only one that can lack its terminator.
        let is_last_content_line = index == raw_lines.len() - 1;
        let record_start = cursor;
        let record_bytes = line.len() as u64 + u64::from(!is_last_content_line);
        cursor += record_bytes;
        if line.is_empty() {
            continue;
        }
        let line = match std::str::from_utf8(line) {
            Ok(line) => line,
            // The same rule as a partial JSON record: one unterminated final
            // line can be incomplete, and anything else is corrupt.
            Err(_) if is_last_content_line && !has_final_newline => break,
            Err(error) => {
                return Err(format!(
                    "Invalid complete codegen journal record at line {}: {error}",
                    index + 1
                ))
            }
        };
        let envelope: JournalEnvelope = match serde_json::from_str(line) {
            Ok(envelope) => envelope,
            Err(_error) if is_last_content_line && !has_final_newline => break,
            Err(error) if expected_sequence == 1 => {
                return Err(format!(
                    "Unsupported codegen journal from an earlier development build: {error}"
                ))
            }
            Err(error) => {
                return Err(format!(
                    "Invalid complete codegen journal record at line {}: {error}",
                    index + 1
                ))
            }
        };
        if envelope.version != JOURNAL_VERSION {
            return Err(format!(
                "Unsupported codegen journal version {}. Expected version {}.",
                envelope.version, JOURNAL_VERSION
            ));
        }
        if envelope.sequence != expected_sequence {
            return Err(format!(
                "Invalid codegen journal sequence at line {}. Expected {}, found {}.",
                index + 1,
                expected_sequence,
                envelope.sequence
            ));
        }
        if terminal.is_some() {
            return Err(format!(
                "Codegen journal contains a record after its terminal record at line {}.",
                index + 1
            ));
        }
        match envelope.record {
            JournalRecord::Start {
                title: start_title,
                last_url: start_url,
            } => {
                if expected_sequence != 1 {
                    return Err("Codegen journal contains more than one start record.".to_string());
                }
                title = Some(start_title);
                last_url = start_url;
            }
            JournalRecord::Action(action) => {
                if title.is_none() {
                    return Err("Codegen journal does not start with a start record.".to_string());
                }
                if actions.insert(action.action_id, action).is_some() {
                    return Err("Codegen journal contains a duplicate action ID.".to_string());
                }
            }
            JournalRecord::UpdateAction(action) => {
                let Some(existing) = actions.get_mut(&action.action_id) else {
                    return Err(format!(
                        "Codegen journal updates unknown action {}.",
                        action.action_id
                    ));
                };
                let existing_step_ids = existing
                    .steps
                    .iter()
                    .map(|step| step.step_id)
                    .collect::<Vec<_>>();
                let updated_step_ids = action
                    .steps
                    .iter()
                    .map(|step| step.step_id)
                    .collect::<Vec<_>>();
                if existing_step_ids != updated_step_ids {
                    return Err(format!(
                        "Codegen journal update for action {} changes or references unknown step IDs.",
                        action.action_id
                    ));
                }
                *existing = action;
            }
            JournalRecord::State {
                last_url: updated_url,
            } => last_url = updated_url,
            JournalRecord::Pages(updated) => page_state = updated,
            JournalRecord::Degraded { message } => degraded_messages.push(message),
            JournalRecord::Warning {
                warning_id,
                code,
                message,
                ..
            } => {
                next_warning_id = next_warning_id.max(warning_id + 1);
                if warnings
                    .insert(warning_id, format!("{code}: {message}"))
                    .is_some()
                {
                    return Err(format!(
                        "Codegen journal contains duplicate warning ID {warning_id}."
                    ));
                }
            }
            JournalRecord::ResolveWarning { warning_id } => {
                next_warning_id = next_warning_id.max(warning_id + 1);
                if warnings.remove(&warning_id).is_none() {
                    return Err(format!(
                        "Codegen journal resolves unknown warning {warning_id}."
                    ));
                }
            }
            JournalRecord::OutputWritten { format, path } => {
                terminal = Some(TerminalRecord::OutputWritten { format, path });
            }
            JournalRecord::DiscardRequested => {
                terminal = Some(TerminalRecord::DiscardRequested);
            }
        }
        expected_sequence += 1;
        valid_bytes = record_start + record_bytes;
    }

    let title = title.ok_or_else(|| "Codegen journal is missing its start record.".to_string())?;
    Ok(RecoveredJournal {
        title,
        last_url,
        page_state,
        actions: actions.into_values().collect(),
        next_sequence: expected_sequence,
        next_warning_id,
        degraded_messages,
        warnings: warnings.into_values().collect(),
        terminal,
        valid_bytes,
    })
}

pub fn known_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf()];
    paths.extend(legacy_metadata_paths(path));
    paths.sort();
    paths.dedup();
    paths
}

pub fn existing_known_paths(path: &Path) -> Vec<PathBuf> {
    known_paths(path)
        .into_iter()
        .filter(|candidate| fs::symlink_metadata(candidate).is_ok())
        .collect()
}

pub fn remove_known_files(path: &Path) -> Result<(), Vec<PathBuf>> {
    let mut remaining = Vec::new();
    for candidate in known_paths(path) {
        match fs::symlink_metadata(&candidate) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                if fs::remove_file(&candidate).is_err() {
                    remaining.push(candidate);
                }
            }
            Ok(_) => remaining.push(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => remaining.push(candidate),
        }
    }
    if remaining.is_empty() {
        Ok(())
    } else {
        Err(remaining)
    }
}

pub fn write_output_atomic(path: &Path, output: &str) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "Codegen output path must have a valid file name.".to_string())?;
    let existing_permissions = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            Some(metadata.permissions())
        }
        Ok(_) => {
            return Err(format!(
                "Codegen output is not a regular file: {}",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Failed to inspect codegen output: {error}")),
    };
    let mut temporary_path = None;
    let mut temporary_file = None;
    for attempt in 0..100u32 {
        let candidate = parent.join(format!(
            ".{file_name}.agent-browser-codegen-{}-{attempt}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(&candidate) {
            Ok(file) => {
                temporary_path = Some(candidate);
                temporary_file = Some(file);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Failed to create codegen output: {error}")),
        }
    }
    let temporary_path = temporary_path
        .ok_or_else(|| "Failed to allocate a temporary codegen output path.".to_string())?;
    let mut temporary_file = temporary_file.expect("temporary file accompanies its path");
    let write_result = (|| {
        temporary_file
            .write_all(output.as_bytes())
            .and_then(|_| temporary_file.flush())
            .map_err(|error| format!("Failed to write codegen output: {error}"))?;
        if let Some(permissions) = existing_permissions {
            fs::set_permissions(&temporary_path, permissions)
                .map_err(|error| format!("Failed to preserve output permissions: {error}"))?;
        }
        drop(temporary_file);
        fs::rename(&temporary_path, path)
            .map_err(|error| format!("Failed to install codegen output: {error}"))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::codegen::Scope;

    fn viewport() -> Step {
        Step::SetViewport {
            width: 1280,
            height: 720,
            device_scale_factor: 1.0,
            is_mobile: false,
        }
    }

    fn captured(step_id: u64, step: Step) -> CapturedStep {
        CapturedStep { step_id, step }
    }

    fn envelope(sequence: u64, record: JournalRecord) -> String {
        serde_json::to_string(&JournalEnvelope {
            version: JOURNAL_VERSION,
            sequence,
            record,
        })
        .unwrap()
    }

    fn start() -> JournalRecord {
        JournalRecord::Start {
            title: "flow".to_string(),
            last_url: None,
        }
    }

    #[test]
    fn recovers_actions_and_latest_updates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        append(
            &path,
            1,
            &JournalRecord::Start {
                title: "checkout".to_string(),
                last_url: None,
            },
        )
        .unwrap();
        append(
            &path,
            2,
            &JournalRecord::Action(CapturedAction {
                action_id: 1,
                action: "viewport".to_string(),
                steps: vec![captured(1, viewport())],
            }),
        )
        .unwrap();
        append(
            &path,
            3,
            &JournalRecord::UpdateAction(CapturedAction {
                action_id: 1,
                action: "viewport".to_string(),
                steps: vec![captured(
                    1,
                    Step::KeyDown {
                        key: "Enter".to_string(),
                        scope: Scope::default(),
                    },
                )],
            }),
        )
        .unwrap();

        let recovered = recover(&path).unwrap();

        assert_eq!(recovered.title, "checkout");
        assert_eq!(recovered.next_sequence, 4);
        assert_eq!(recovered.actions.len(), 1);
        assert_eq!(recovered.actions[0].steps.len(), 1);
        assert!(matches!(
            recovered.actions[0].steps[0].step,
            Step::KeyDown { .. }
        ));
    }

    #[test]
    fn recovers_the_latest_logical_page_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        append(&path, 1, &start()).unwrap();
        append(
            &path,
            2,
            &JournalRecord::Pages(PersistedPageState {
                pages: vec![PersistedPage {
                    page_id: "p1".to_string(),
                    target_id: Some("target-a".to_string()),
                    opener_target_id: None,
                    popup_attributed: true,
                    url: "https://example.com".to_string(),
                    closed: false,
                    url_unrecorded: false,
                }],
                next_page_id: 2,
                start_page_id: Some("p1".to_string()),
                last_active_page_id: Some("p1".to_string()),
                initial_state_captured: true,
            }),
        )
        .unwrap();

        let recovered = recover(&path).unwrap();

        assert_eq!(recovered.page_state.pages[0].page_id, "p1");
        assert_eq!(recovered.page_state.next_page_id, 2);
        assert!(recovered.page_state.initial_state_captured);
    }

    #[test]
    fn ignores_only_an_incomplete_final_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        fs::write(
            &path,
            format!(
                "{}\n{{\"version\":1",
                serde_json::to_string(&JournalEnvelope {
                    version: JOURNAL_VERSION,
                    sequence: 1,
                    record: JournalRecord::Start {
                        title: "flow".to_string(),
                        last_url: None,
                    },
                })
                .unwrap()
            ),
        )
        .unwrap();

        let recovered = recover(&path).unwrap();

        assert_eq!(recovered.next_sequence, 2);
    }

    fn journal_with_suffix(directory: &tempfile::TempDir, suffix: &str) -> PathBuf {
        let path = directory.path().join("flow.codegen.jsonl");
        let start = serde_json::to_string(&JournalEnvelope {
            version: JOURNAL_VERSION,
            sequence: 1,
            record: JournalRecord::Start {
                title: "flow".to_string(),
                last_url: None,
            },
        })
        .unwrap();
        fs::write(&path, format!("{start}\n{suffix}")).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn the_repair_refuses_a_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let target = journal_with_suffix(&directory, "{\"version\":1");
        let link = directory.path().join("link.codegen.jsonl");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let before = fs::read_to_string(&target).unwrap();

        let error = repair(&link, 0).unwrap_err();

        assert!(error.contains("not a regular file"), "{error}");
        assert_eq!(fs::read_to_string(&target).unwrap(), before);
    }

    #[test]
    fn a_torn_multi_byte_write_keeps_every_earlier_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        let start = serde_json::to_string(&JournalEnvelope {
            version: JOURNAL_VERSION,
            sequence: 1,
            record: JournalRecord::Start {
                title: "flow".to_string(),
                last_url: None,
            },
        })
        .unwrap();
        // A crash inside a UTF-8 sequence. Captured values are stored verbatim,
        // so a journal can hold any text.
        let mut bytes = format!("{start}\n{{\"version\":1,\"data\":\"\u{20ac}").into_bytes();
        bytes.truncate(bytes.len() - 1);
        fs::write(&path, &bytes).unwrap();

        let recovered = recover(&path).unwrap();
        assert_eq!(recovered.next_sequence, 2);
        repair(&path, recovered.valid_bytes).unwrap();
        append(&path, 2, &JournalRecord::DiscardRequested).unwrap();

        let again = recover(&path).unwrap();
        assert_eq!(again.next_sequence, 3);
    }

    #[test]
    fn a_complete_record_that_is_not_utf8_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        let start = serde_json::to_string(&JournalEnvelope {
            version: JOURNAL_VERSION,
            sequence: 1,
            record: JournalRecord::Start {
                title: "flow".to_string(),
                last_url: None,
            },
        })
        .unwrap();
        let mut bytes = format!("{start}\n").into_bytes();
        bytes.extend_from_slice(b"{\"version\":1,\"data\":\"\xE2\x82\"}\n");
        fs::write(&path, &bytes).unwrap();

        let error = recover(&path).unwrap_err();

        assert!(error.contains("line 2"), "{error}");
    }

    #[test]
    fn a_partial_final_record_survives_the_next_append() {
        let directory = tempfile::tempdir().unwrap();
        let path = journal_with_suffix(&directory, "{\"version\":1");

        let recovered = recover(&path).unwrap();
        assert_eq!(recovered.next_sequence, 2);
        repair(&path, recovered.valid_bytes).unwrap();
        append(&path, 2, &JournalRecord::DiscardRequested).unwrap();

        // Without the repair, the appended record joins the partial record and
        // the journal becomes unreadable, including for `codegen stop`.
        let again = recover(&path).unwrap();
        assert_eq!(again.next_sequence, 3);
        assert!(matches!(
            again.terminal,
            Some(TerminalRecord::DiscardRequested)
        ));
    }

    #[test]
    fn a_complete_final_record_without_a_newline_survives_the_next_append() {
        let directory = tempfile::tempdir().unwrap();
        let complete = serde_json::to_string(&JournalEnvelope {
            version: JOURNAL_VERSION,
            sequence: 2,
            record: JournalRecord::State { last_url: None },
        })
        .unwrap();
        let path = journal_with_suffix(&directory, &complete);

        let recovered = recover(&path).unwrap();
        assert_eq!(recovered.next_sequence, 3);
        repair(&path, recovered.valid_bytes).unwrap();
        append(&path, 3, &JournalRecord::DiscardRequested).unwrap();

        let again = recover(&path).unwrap();
        assert_eq!(again.next_sequence, 4);
        assert!(matches!(
            again.terminal,
            Some(TerminalRecord::DiscardRequested)
        ));
    }

    #[test]
    fn the_repair_keeps_every_valid_record_and_the_journal_mode() {
        let directory = tempfile::tempdir().unwrap();
        let path = journal_with_suffix(&directory, "{\"version\":1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let before = fs::read_to_string(&path).unwrap();
        let first_line = before.lines().next().unwrap().to_string();

        let recovered = recover(&path).unwrap();
        repair(&path, recovered.valid_bytes).unwrap();

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, format!("{first_line}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn the_repair_leaves_a_whole_journal_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        append(&path, 1, &start()).unwrap();
        append(&path, 2, &JournalRecord::State { last_url: None }).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        let recovered = recover(&path).unwrap();
        repair(&path, recovered.valid_bytes).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn rejects_a_malformed_complete_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        fs::write(
            &path,
            format!(
                "{}\nnot-json\n",
                serde_json::to_string(&JournalEnvelope {
                    version: JOURNAL_VERSION,
                    sequence: 1,
                    record: JournalRecord::Start {
                        title: "flow".to_string(),
                        last_url: None,
                    },
                })
                .unwrap()
            ),
        )
        .unwrap();

        assert!(recover(&path)
            .unwrap_err()
            .contains("Invalid complete codegen journal record"));
    }

    #[test]
    fn rejects_an_update_for_an_unknown_action() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.codegen.jsonl");
        fs::write(&path, "").unwrap();
        append(
            &path,
            1,
            &JournalRecord::Start {
                title: "flow".to_string(),
                last_url: None,
            },
        )
        .unwrap();
        append(
            &path,
            2,
            &JournalRecord::UpdateAction(CapturedAction {
                action_id: 7,
                action: "click".to_string(),
                steps: vec![],
            }),
        )
        .unwrap();

        assert!(recover(&path)
            .unwrap_err()
            .contains("updates unknown action 7"));
    }

    #[test]
    fn rejects_missing_and_duplicate_sequences() {
        let directory = tempfile::tempdir().unwrap();
        for (name, second_sequence) in [("missing", 3), ("duplicate", 1)] {
            let path = directory.path().join(format!("{name}.jsonl"));
            fs::write(
                &path,
                format!(
                    "{}\n{}\n",
                    envelope(1, start()),
                    envelope(second_sequence, JournalRecord::State { last_url: None })
                ),
            )
            .unwrap();
            assert!(recover(&path).unwrap_err().contains("sequence"));
        }
    }

    #[test]
    fn rejects_unknown_versions_and_step_ids() {
        let directory = tempfile::tempdir().unwrap();
        let version_path = directory.path().join("version.jsonl");
        fs::write(
            &version_path,
            format!(
                "{}\n",
                serde_json::to_string(&JournalEnvelope {
                    version: JOURNAL_VERSION + 1,
                    sequence: 1,
                    record: start(),
                })
                .unwrap()
            ),
        )
        .unwrap();
        assert!(recover(&version_path).unwrap_err().contains("version"));

        let step_path = directory.path().join("step.jsonl");
        fs::write(&step_path, "").unwrap();
        append(&step_path, 1, &start()).unwrap();
        append(
            &step_path,
            2,
            &JournalRecord::Action(CapturedAction {
                action_id: 1,
                action: "viewport".to_string(),
                steps: vec![captured(1, viewport())],
            }),
        )
        .unwrap();
        append(
            &step_path,
            3,
            &JournalRecord::UpdateAction(CapturedAction {
                action_id: 1,
                action: "viewport".to_string(),
                steps: vec![captured(2, viewport())],
            }),
        )
        .unwrap();
        assert!(recover(&step_path)
            .unwrap_err()
            .contains("unknown step IDs"));
    }

    #[test]
    fn rejects_oversized_and_old_development_files_without_value_leaks() {
        let directory = tempfile::tempdir().unwrap();
        let oversized = directory.path().join("oversized.jsonl");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&oversized)
            .unwrap();
        file.set_len(MAX_JOURNAL_BYTES + 1).unwrap();
        assert!(recover(&oversized).unwrap_err().contains("64 MiB"));

        let old = directory.path().join("old.jsonl");
        fs::write(&old, "{\"type\":\"change\",\"value\":\"TOP_SECRET\"}\n").unwrap();
        let error = recover(&old).unwrap_err();
        assert!(error.contains("earlier development build"));
        assert!(!error.contains("TOP_SECRET"));
    }

    #[test]
    fn restores_only_unresolved_warnings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("warnings.jsonl");
        fs::write(&path, "").unwrap();
        append(&path, 1, &start()).unwrap();
        append(
            &path,
            2,
            &JournalRecord::Warning {
                warning_id: 1,
                code: "omitted-action".to_string(),
                message: "One action was omitted.".to_string(),
                action_id: Some(2),
                step_id: None,
            },
        )
        .unwrap();
        append(&path, 3, &JournalRecord::ResolveWarning { warning_id: 1 }).unwrap();

        assert!(recover(&path).unwrap().warnings.is_empty());
    }

    #[test]
    fn refuses_directories_as_journals_and_outputs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("directory");
        fs::create_dir(&path).unwrap();

        assert!(recover(&path).unwrap_err().contains("regular file"));
        assert!(write_output_atomic(&path, "flow")
            .unwrap_err()
            .contains("regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_remove_a_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("flow.codegen.jsonl");
        fs::write(&target, "secret").unwrap();
        symlink(&target, &link).unwrap();

        let remaining = remove_known_files(&link).unwrap_err();

        assert_eq!(remaining, vec![link]);
        assert_eq!(fs::read_to_string(target).unwrap(), "secret");
    }

    #[test]
    fn atomically_writes_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.json");

        write_output_atomic(&path, "first").unwrap();
        write_output_atomic(&path, "second").unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "second");
    }

    #[cfg(unix)]
    #[test]
    fn creates_private_files_and_preserves_replacement_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flow.json");
        write_output_atomic(&path, "first").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        write_output_atomic(&path, "second").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn creates_a_private_journal_and_rejects_an_existing_symlink() {
        use crate::test_utils::EnvGuard;
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::time::{SystemTime, UNIX_EPOCH};

        let directory = tempfile::tempdir().unwrap();
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR"]);
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            directory.path().to_str().unwrap(),
        );
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let session = format!("codegen-sidecar-test-{}-{suffix}", std::process::id());
        let (path, _) = create(&session, "flow").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        remove_known_files(&path).unwrap();

        let target = directory.path().join("target");
        fs::write(&target, "safe").unwrap();
        symlink(&target, &path).unwrap();
        let error = create(&session, "flow").unwrap_err();
        assert!(error.contains("already exists") || error.contains("create codegen journal"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "safe");
        fs::remove_file(path).unwrap();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn rejects_a_special_journal_file() {
        use std::os::unix::net::UnixListener;

        let path = std::env::temp_dir().join(format!(
            "ab-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _listener = UnixListener::bind(&path).unwrap();

        assert!(recover(&path).unwrap_err().contains("regular file"));
        drop(_listener);
        fs::remove_file(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_replace_an_output_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("flow.json");
        fs::write(&target, "secret").unwrap();
        symlink(&target, &link).unwrap();

        assert!(write_output_atomic(&link, "flow")
            .unwrap_err()
            .contains("regular file"));
        assert_eq!(fs::read_to_string(target).unwrap(), "secret");
    }
}
