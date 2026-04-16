//! Diagnose an agent-browser installation.
//!
//! Runs a battery of checks across environment, Chrome install, daemon
//! state, config files, encryption, providers, network reachability, and
//! a live headless browser launch test.
//!
//! Auto-cleans stale daemon socket/pid/version sidecar files. Destructive
//! repairs (reinstalling Chrome, purging old state files, generating a
//! missing encryption key) are gated behind `--fix`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::color;
use crate::connection::{
    cleanup_stale_files, ensure_daemon, get_socket_dir, send_command, DaemonOptions,
};
use crate::native::state::{get_sessions_dir, get_state_dir};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(windows)]
use windows_sys::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

#[derive(Default, Clone, Copy)]
pub struct DoctorOptions {
    pub offline: bool,
    pub quick: bool,
    pub fix: bool,
    pub json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Info,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Info => "info",
        }
    }

    fn label(&self) -> String {
        match self {
            Status::Pass => color::green("pass"),
            Status::Warn => color::yellow("warn"),
            Status::Fail => color::red("fail"),
            Status::Info => color::dim("info"),
        }
    }
}

#[derive(Clone)]
pub struct Check {
    pub id: String,
    pub category: &'static str,
    pub status: Status,
    pub message: String,
    pub fix: Option<String>,
}

impl Check {
    fn new(
        id: impl Into<String>,
        category: &'static str,
        status: Status,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            status,
            message: message.into(),
            fix: None,
        }
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Run the doctor command. Returns the process exit code.
pub fn run_doctor(opts: DoctorOptions) -> i32 {
    let mut checks: Vec<Check> = Vec::new();
    let mut fixed: Vec<String> = Vec::new();

    check_environment(&mut checks);
    check_chrome(&mut checks);
    check_daemons(&mut checks);
    check_config(&mut checks);
    check_security(&mut checks);
    check_providers(&mut checks);

    if !opts.offline {
        check_network(&mut checks);
    }

    if !opts.quick {
        check_launch(&mut checks);
    }

    if opts.fix {
        run_fixes(&mut checks, &mut fixed);
    }

    let summary = summarize(&checks);
    let exit_code = if summary.fail > 0 { 1 } else { 0 };

    if opts.json {
        print_json(&checks, &summary, &fixed, exit_code == 0);
    } else {
        print_text(&checks, &summary, &fixed, opts.fix);
    }

    exit_code
}

struct Summary {
    pass: usize,
    warn: usize,
    fail: usize,
}

fn summarize(checks: &[Check]) -> Summary {
    let mut s = Summary {
        pass: 0,
        warn: 0,
        fail: 0,
    };
    for c in checks {
        match c.status {
            Status::Pass => s.pass += 1,
            Status::Warn => s.warn += 1,
            Status::Fail => s.fail += 1,
            Status::Info => {}
        }
    }
    s
}

fn print_text(checks: &[Check], summary: &Summary, fixed: &[String], fix_ran: bool) {
    println!("{}", color::bold("agent-browser doctor"));

    let mut current_category = "";
    for c in checks {
        if c.category != current_category {
            current_category = c.category;
            println!();
            println!("{}", color::bold(current_category));
        }
        println!("  {}  {}", c.status.label(), c.message);
        if let Some(fix) = &c.fix {
            println!("        {} {}", color::dim("fix:"), fix);
        }
    }

    if !fixed.is_empty() {
        println!();
        println!("{}", color::bold("Fixed"));
        for line in fixed {
            println!("  {}  {}", color::green("done"), line);
        }
    }

    println!();
    let line = format!(
        "Summary: {} pass, {} warn, {} fail",
        summary.pass, summary.warn, summary.fail
    );
    if summary.fail > 0 {
        println!("{}", color::red(&line));
    } else if summary.warn > 0 {
        println!("{}", color::yellow(&line));
    } else {
        println!("{}", color::green(&line));
    }

    if !fix_ran && checks.iter().any(|c| c.fix.is_some()) {
        println!();
        println!(
            "{} Run with {} to attempt repairs.",
            color::dim("tip:"),
            color::bold("--fix")
        );
    }
}

fn print_json(checks: &[Check], summary: &Summary, fixed: &[String], success: bool) {
    let checks_json: Vec<Value> = checks
        .iter()
        .map(|c| {
            let mut obj = json!({
                "id": c.id,
                "category": c.category,
                "status": c.status.as_str(),
                "message": c.message,
            });
            if let Some(fix) = &c.fix {
                obj["fix"] = json!(fix);
            }
            obj
        })
        .collect();

    let payload = json!({
        "success": success,
        "summary": {
            "pass": summary.pass,
            "warn": summary.warn,
            "fail": summary.fail,
        },
        "checks": checks_json,
        "fixed": fixed,
    });
    println!("{}", payload);
}

// ---------- Environment ----------

fn push_dir_check(
    checks: &mut Vec<Check>,
    id: &'static str,
    category: &'static str,
    label: &str,
    dir: &Path,
) {
    if dir.exists() {
        if is_writable_dir(dir) {
            checks.push(Check::new(
                id,
                category,
                Status::Pass,
                format!("{} {}", label, dir.display()),
            ));
        } else {
            checks.push(
                Check::new(
                    id,
                    category,
                    Status::Fail,
                    format!("{} not writable: {}", label, dir.display()),
                )
                .with_fix(format!("chmod u+rwx {}", dir.display())),
            );
        }
    } else {
        checks.push(Check::new(
            id,
            category,
            Status::Info,
            format!(
                "{} does not exist yet (will be created on first use): {}",
                label,
                dir.display()
            ),
        ));
    }
}

fn check_environment(checks: &mut Vec<Check>) {
    let category = "Environment";

    let version = env!("CARGO_PKG_VERSION");
    let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH,);
    checks.push(Check::new(
        "env.version",
        category,
        Status::Pass,
        format!("CLI version {} ({})", version, platform),
    ));

    match dirs::home_dir() {
        Some(home) => checks.push(Check::new(
            "env.home",
            category,
            Status::Pass,
            format!("Home directory {}", home.display()),
        )),
        None => checks.push(Check::new(
            "env.home",
            category,
            Status::Fail,
            "Could not determine home directory",
        )),
    }

    let state_dir = get_state_dir();
    let socket_dir = get_socket_dir();

    // Under the default setup, state and socket dirs are the same
    // (~/.agent-browser). Collapse to a single line when they match;
    // split when XDG_RUNTIME_DIR or AGENT_BROWSER_SOCKET_DIR diverts
    // sockets elsewhere.
    if state_dir == socket_dir {
        push_dir_check(
            checks,
            "env.state_dir",
            category,
            "State and socket directory",
            &state_dir,
        );
    } else {
        push_dir_check(
            checks,
            "env.state_dir",
            category,
            "State directory",
            &state_dir,
        );
        push_dir_check(
            checks,
            "env.socket_dir",
            category,
            "Socket directory",
            &socket_dir,
        );
    }

    match disk_free_bytes(&state_dir) {
        Some(bytes) => {
            let mb = bytes / (1024 * 1024);
            let human = human_size(bytes);
            if mb < 500 {
                checks.push(
                    Check::new(
                        "env.disk_free",
                        category,
                        Status::Warn,
                        format!("Low disk space at state dir: {} free", human),
                    )
                    .with_fix("free up disk space; Chrome installs require ~500 MB"),
                );
            } else {
                checks.push(Check::new(
                    "env.disk_free",
                    category,
                    Status::Pass,
                    format!("{} free at state dir", human),
                ));
            }
        }
        None => checks.push(Check::new(
            "env.disk_free",
            category,
            Status::Info,
            "Disk free check unavailable on this platform",
        )),
    }
}

// ---------- Chrome ----------

fn check_chrome(checks: &mut Vec<Check>) {
    let category = "Chrome";

    let chrome = crate::native::cdp::chrome::find_chrome();
    match chrome {
        Some(path) => {
            let label = path.display().to_string();
            match query_chrome_version(&path) {
                Some(version) => checks.push(Check::new(
                    "chrome.installed",
                    category,
                    Status::Pass,
                    format!("{} at {}", version, label),
                )),
                None => checks.push(Check::new(
                    "chrome.installed",
                    category,
                    Status::Pass,
                    format!("Chrome at {} (version unknown)", label),
                )),
            }
        }
        None => checks.push(
            Check::new(
                "chrome.installed",
                category,
                Status::Fail,
                "No Chrome binary found",
            )
            .with_fix("agent-browser install"),
        ),
    }

    let cache_dir = crate::install::get_browsers_dir();
    if cache_dir.exists() {
        checks.push(Check::new(
            "chrome.cache_dir",
            category,
            Status::Info,
            format!("Cache dir {}", cache_dir.display()),
        ));
    }

    if let Some(puppeteer_dir) = puppeteer_cache_dir() {
        if puppeteer_dir.exists() {
            checks.push(Check::new(
                "chrome.puppeteer_cache",
                category,
                Status::Info,
                format!(
                    "Puppeteer cache also present: {} (will be used as a fallback)",
                    puppeteer_dir.display()
                ),
            ));
        }
    }

    if let Some(user_data_dir) = crate::native::cdp::chrome::find_chrome_user_data_dir() {
        let profiles = crate::native::cdp::chrome::list_chrome_profiles(&user_data_dir);
        let count = profiles.len();
        let dir_label = user_data_dir.display().to_string();
        if count == 0 {
            checks.push(Check::new(
                "chrome.user_data_dir",
                category,
                Status::Info,
                format!(
                    "Chrome user data dir found ({}), no profiles parsed",
                    dir_label
                ),
            ));
        } else {
            checks.push(Check::new(
                "chrome.user_data_dir",
                category,
                Status::Info,
                format!("{} Chrome profile(s) at {}", count, dir_label),
            ));
        }
    }

    if let Ok(engine) = env::var("AGENT_BROWSER_ENGINE") {
        if engine == "lightpanda" {
            // Best-effort PATH lookup; absence is FAIL only when the user
            // explicitly opted into the lightpanda engine.
            if which_exists("lightpanda") {
                checks.push(Check::new(
                    "chrome.engine_lightpanda",
                    category,
                    Status::Pass,
                    "Lightpanda binary on PATH",
                ));
            } else {
                checks.push(
                    Check::new(
                        "chrome.engine_lightpanda",
                        category,
                        Status::Fail,
                        "AGENT_BROWSER_ENGINE=lightpanda but no lightpanda binary on PATH",
                    )
                    .with_fix("install lightpanda or unset AGENT_BROWSER_ENGINE"),
                );
            }
        }
    }
}

fn query_chrome_version(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---------- Daemons ----------

fn check_daemons(checks: &mut Vec<Check>) {
    let category = "Daemons";

    let socket_dir = get_socket_dir();
    let entries = match fs::read_dir(&socket_dir) {
        Ok(e) => e,
        Err(_) => {
            checks.push(Check::new(
                "daemon.none",
                category,
                Status::Pass,
                "No active daemons",
            ));
            return;
        }
    };

    let mut sessions: Vec<(String, u32, bool)> = Vec::new();
    let mut cleaned: Vec<(String, String)> = Vec::new();
    let mut dashboard_pid: Option<u32> = None;
    let mut dashboard_alive = false;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();

        if name == "dashboard.pid" {
            if let Ok(s) = fs::read_to_string(entry.path()) {
                if let Ok(pid) = s.trim().parse::<u32>() {
                    dashboard_pid = Some(pid);
                    dashboard_alive = is_pid_alive(pid);
                    if !dashboard_alive {
                        let _ = fs::remove_file(entry.path());
                        cleaned.push(("dashboard".to_string(), "process gone".to_string()));
                    }
                }
            }
            continue;
        }

        let session_name = match name.strip_suffix(".pid") {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        let pid_path = entry.path();
        let pid = match fs::read_to_string(&pid_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
        {
            Some(p) => p,
            None => {
                cleanup_stale_files(&session_name);
                cleaned.push((session_name, "unreadable pid file".to_string()));
                continue;
            }
        };

        if !is_pid_alive(pid) {
            cleanup_stale_files(&session_name);
            cleaned.push((session_name, "process gone".to_string()));
            continue;
        }

        let version_match = read_session_version(&session_name)
            .map(|v| v == env!("CARGO_PKG_VERSION"))
            .unwrap_or(false);

        sessions.push((session_name, pid, version_match));
    }

    // Also walk for orphaned .sock files without a corresponding .pid file.
    #[cfg(unix)]
    if let Ok(entries) = fs::read_dir(&socket_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(session_name) = name.strip_suffix(".sock") {
                if session_name.is_empty() {
                    continue;
                }
                let pid_path = socket_dir.join(format!("{}.pid", session_name));
                if !pid_path.exists() {
                    cleanup_stale_files(session_name);
                    cleaned.push((session_name.to_string(), "orphaned socket".to_string()));
                }
            }
        }
    }

    for (session, reason) in &cleaned {
        checks.push(Check::new(
            format!("daemon.cleaned.{}", session),
            category,
            Status::Warn,
            format!("Cleaned stale files: {} ({})", session, reason),
        ));
    }

    if sessions.is_empty() {
        checks.push(Check::new(
            "daemon.active",
            category,
            Status::Pass,
            "No active daemons",
        ));
    } else {
        for (session, pid, version_match) in &sessions {
            let status = if *version_match {
                Status::Pass
            } else {
                Status::Warn
            };
            let suffix = if *version_match {
                String::new()
            } else {
                format!(" (version mismatch with CLI {})", env!("CARGO_PKG_VERSION"))
            };
            let mut check = Check::new(
                format!("daemon.session.{}", session),
                category,
                status,
                format!("Session {} (pid {}){}", session, pid, suffix),
            );
            if !version_match {
                check = check.with_fix(format!("agent-browser --session {} close", session));
            }
            checks.push(check);
        }
    }

    if let Some(pid) = dashboard_pid {
        if dashboard_alive {
            checks.push(Check::new(
                "daemon.dashboard",
                category,
                Status::Pass,
                format!("Dashboard server running (pid {})", pid),
            ));
        }
    }
}

fn read_session_version(session: &str) -> Option<String> {
    let path = get_socket_dir().join(format!("{}.version", session));
    fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------- Config ----------

fn check_config(checks: &mut Vec<Check>) {
    let category = "Config";

    let user_path = dirs::home_dir().map(|d| d.join(".agent-browser").join("config.json"));
    if let Some(p) = user_path {
        if p.exists() {
            match parse_json_file(&p) {
                Ok(_) => checks.push(Check::new(
                    "config.user",
                    category,
                    Status::Pass,
                    format!("{} (valid JSON)", p.display()),
                )),
                Err(e) => checks.push(
                    Check::new(
                        "config.user",
                        category,
                        Status::Fail,
                        format!("{}: {}", p.display(), e),
                    )
                    .with_fix(format!("edit {}", p.display())),
                ),
            }
        }
    }

    let project_path = PathBuf::from("agent-browser.json");
    if project_path.exists() {
        match parse_json_file(&project_path) {
            Ok(_) => checks.push(Check::new(
                "config.project",
                category,
                Status::Pass,
                format!("{} (valid JSON)", project_path.display()),
            )),
            Err(e) => checks.push(
                Check::new(
                    "config.project",
                    category,
                    Status::Fail,
                    format!("{}: {}", project_path.display(), e),
                )
                .with_fix(format!("edit {}", project_path.display())),
            ),
        }
    }

    if let Ok(custom) = env::var("AGENT_BROWSER_CONFIG") {
        let p = PathBuf::from(&custom);
        if !p.exists() {
            checks.push(
                Check::new(
                    "config.custom",
                    category,
                    Status::Fail,
                    format!("AGENT_BROWSER_CONFIG points to missing file: {}", custom),
                )
                .with_fix("update or unset AGENT_BROWSER_CONFIG"),
            );
        } else {
            match parse_json_file(&p) {
                Ok(_) => checks.push(Check::new(
                    "config.custom",
                    category,
                    Status::Pass,
                    format!("AGENT_BROWSER_CONFIG: {} (valid JSON)", custom),
                )),
                Err(e) => checks.push(
                    Check::new(
                        "config.custom",
                        category,
                        Status::Fail,
                        format!("AGENT_BROWSER_CONFIG: {}: {}", custom, e),
                    )
                    .with_fix(format!("edit {}", custom)),
                ),
            }
        }
    }
}

fn parse_json_file(path: &Path) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read failed: {}", e))?;
    serde_json::from_str::<Value>(&content).map_err(|e| format!("invalid JSON: {}", e))?;
    Ok(())
}

// ---------- Security ----------

fn check_security(checks: &mut Vec<Check>) {
    let category = "Security";

    let key_env = env::var("AGENT_BROWSER_ENCRYPTION_KEY").ok();
    let key_file = get_state_dir().join(".encryption-key");
    if let Some(hex) = &key_env {
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            checks.push(Check::new(
                "security.encryption_key",
                category,
                Status::Pass,
                "AGENT_BROWSER_ENCRYPTION_KEY set (64-char hex)",
            ));
        } else {
            checks.push(
                Check::new(
                    "security.encryption_key",
                    category,
                    Status::Fail,
                    "AGENT_BROWSER_ENCRYPTION_KEY is not a 64-char hex string",
                )
                .with_fix("export AGENT_BROWSER_ENCRYPTION_KEY=$(openssl rand -hex 32)"),
            );
        }
    } else if key_file.exists() {
        let mut msg = format!("Encryption key file present: {}", key_file.display());
        let mut status = Status::Pass;
        let mut fix: Option<String> = None;
        #[cfg(unix)]
        if let Ok(meta) = fs::metadata(&key_file) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                status = Status::Warn;
                msg = format!(
                    "Encryption key file is too permissive ({:o}): {}",
                    mode,
                    key_file.display()
                );
                fix = Some(format!("chmod 600 {}", key_file.display()));
            }
        }
        let mut check = Check::new("security.encryption_key", category, status, msg);
        if let Some(f) = fix {
            check = check.with_fix(f);
        }
        checks.push(check);
    } else {
        checks.push(
            Check::new(
                "security.encryption_key",
                category,
                Status::Info,
                "No encryption key set (will be auto-generated on first auth save)",
            )
            .with_fix("export AGENT_BROWSER_ENCRYPTION_KEY=$(openssl rand -hex 32)"),
        );
    }

    let sessions_dir = get_sessions_dir();
    if sessions_dir.exists() {
        let expire_days = env::var("AGENT_BROWSER_STATE_EXPIRE_DAYS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(30);
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(expire_days * 86_400))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut total = 0usize;
        let mut old = 0usize;
        if let Ok(entries) = fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    total += 1;
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if modified < cutoff {
                                old += 1;
                            }
                        }
                    }
                }
            }
        }
        if total == 0 {
            checks.push(Check::new(
                "security.state_count",
                category,
                Status::Info,
                "No saved state files",
            ));
        } else if old > 0 {
            checks.push(
                Check::new(
                    "security.state_count",
                    category,
                    Status::Warn,
                    format!(
                        "{} state file(s) older than {} days ({} total)",
                        old, expire_days, total
                    ),
                )
                .with_fix(format!(
                    "agent-browser state clean --older-than {}",
                    expire_days
                )),
            );
        } else {
            checks.push(Check::new(
                "security.state_count",
                category,
                Status::Pass,
                format!("{} saved state file(s)", total),
            ));
        }
    }

    if let Ok(policy_path) = env::var("AGENT_BROWSER_ACTION_POLICY") {
        let p = PathBuf::from(&policy_path);
        if !p.exists() {
            checks.push(
                Check::new(
                    "security.action_policy",
                    category,
                    Status::Fail,
                    format!(
                        "AGENT_BROWSER_ACTION_POLICY points to missing file: {}",
                        policy_path
                    ),
                )
                .with_fix("update or unset AGENT_BROWSER_ACTION_POLICY"),
            );
        } else {
            match parse_json_file(&p) {
                Ok(_) => checks.push(Check::new(
                    "security.action_policy",
                    category,
                    Status::Pass,
                    format!("Action policy: {}", policy_path),
                )),
                Err(e) => checks.push(
                    Check::new(
                        "security.action_policy",
                        category,
                        Status::Fail,
                        format!("Action policy: {}: {}", policy_path, e),
                    )
                    .with_fix(format!("edit {}", policy_path)),
                ),
            }
        }
    }
}

// ---------- Providers ----------

fn check_providers(checks: &mut Vec<Check>) {
    let category = "Providers";

    let active = env::var("AGENT_BROWSER_PROVIDER").ok();
    let normalized = active
        .as_ref()
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    let active_status = |provider: &str, ok: bool| -> Status {
        if normalized == provider {
            if ok {
                Status::Pass
            } else {
                Status::Fail
            }
        } else {
            Status::Info
        }
    };

    let providers: &[(&str, &[&str], &str)] = &[
        ("browserless", &["BROWSERLESS_API_KEY"], "Browserless"),
        ("browserbase", &["BROWSERBASE_API_KEY"], "Browserbase"),
        ("browseruse", &["BROWSER_USE_API_KEY"], "Browser Use"),
        ("kernel", &["KERNEL_API_KEY"], "Kernel"),
    ];

    for (id, env_keys, label) in providers {
        let present = env_keys.iter().any(|k| env::var(k).is_ok());
        let provider_id = *id;
        let status = active_status(provider_id, present);
        let msg = if present {
            format!("{}: API key present", label)
        } else {
            format!("{}: {} not set", label, env_keys.to_vec().join(" / "))
        };
        let mut check = Check::new(format!("providers.{}", provider_id), category, status, msg);
        if status == Status::Fail {
            check = check.with_fix(format!(
                "set {} (or unset AGENT_BROWSER_PROVIDER={})",
                env_keys.first().copied().unwrap_or(""),
                provider_id
            ));
        }
        checks.push(check);
    }

    let aws_present = env::var("AWS_ACCESS_KEY_ID").is_ok()
        || env::var("AWS_PROFILE").is_ok()
        || env::var("AWS_SESSION_TOKEN").is_ok();
    let agentcore_status = active_status("agentcore", aws_present);
    let mut agentcore_check = Check::new(
        "providers.agentcore",
        category,
        agentcore_status,
        if aws_present {
            "AgentCore: AWS credentials resolvable".to_string()
        } else {
            "AgentCore: no AWS credentials in env (AWS_ACCESS_KEY_ID / AWS_PROFILE)".to_string()
        },
    );
    if agentcore_status == Status::Fail {
        agentcore_check = agentcore_check
            .with_fix("export AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or AWS_PROFILE");
    }
    checks.push(agentcore_check);

    if normalized == "ios" {
        if which_exists("appium") {
            checks.push(Check::new(
                "providers.ios",
                category,
                Status::Pass,
                "iOS: appium binary on PATH",
            ));
        } else {
            checks.push(
                Check::new(
                    "providers.ios",
                    category,
                    Status::Fail,
                    "iOS: appium binary not found on PATH",
                )
                .with_fix("npm install -g appium && appium driver install xcuitest"),
            );
        }
    }

    let chat_key_present = env::var("AI_GATEWAY_API_KEY").is_ok();
    if chat_key_present {
        checks.push(Check::new(
            "providers.chat",
            category,
            Status::Info,
            "AI_GATEWAY_API_KEY present (chat enabled)",
        ));
    } else {
        checks.push(
            Check::new(
                "providers.chat",
                category,
                Status::Info,
                "AI_GATEWAY_API_KEY not set (chat command disabled)",
            )
            .with_fix("export AI_GATEWAY_API_KEY=gw_..."),
        );
    }

    if let Some(active) = active {
        checks.push(Check::new(
            "providers.active",
            category,
            Status::Info,
            format!("AGENT_BROWSER_PROVIDER = {}", active),
        ));
    }
}

// ---------- Network ----------

fn check_network(checks: &mut Vec<Check>) {
    let category = "Network";

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            checks.push(Check::new(
                "net.runtime",
                category,
                Status::Fail,
                format!("Could not start tokio runtime for probes: {}", e),
            ));
            return;
        }
    };

    let client = match reqwest::Client::builder()
        .user_agent(format!("agent-browser/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            checks.push(Check::new(
                "net.client",
                category,
                Status::Fail,
                format!("Could not build HTTP client: {}", e),
            ));
            return;
        }
    };

    let chrome_url =
        "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";
    probe_url(
        &rt,
        &client,
        checks,
        category,
        "net.chrome_cdn",
        chrome_url,
        "Chrome for Testing CDN",
    );

    if env::var("AI_GATEWAY_API_KEY").is_ok() {
        let url = env::var("AI_GATEWAY_URL")
            .unwrap_or_else(|_| "https://ai-gateway.vercel.sh".to_string());
        probe_url(
            &rt,
            &client,
            checks,
            category,
            "net.ai_gateway",
            &url,
            "AI Gateway",
        );
    }

    if let Ok(provider) = env::var("AGENT_BROWSER_PROVIDER") {
        let url: Option<String> = match provider.to_lowercase().as_str() {
            "browserbase" => Some("https://api.browserbase.com".to_string()),
            "browserless" => Some(
                env::var("BROWSERLESS_API_URL")
                    .unwrap_or_else(|_| "https://production-sfo.browserless.io".to_string()),
            ),
            "browseruse" | "browser-use" => Some("https://api.browser-use.com".to_string()),
            "kernel" => Some(
                env::var("KERNEL_ENDPOINT")
                    .unwrap_or_else(|_| "https://api.onkernel.com".to_string()),
            ),
            _ => None,
        };
        if let Some(url) = url {
            probe_url(
                &rt,
                &client,
                checks,
                category,
                "net.provider",
                &url,
                &format!("Provider {}", provider),
            );
        }
    }
}

fn probe_url(
    rt: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    checks: &mut Vec<Check>,
    category: &'static str,
    id: &'static str,
    url: &str,
    label: &str,
) {
    let started = Instant::now();
    let result = rt.block_on(async { client.head(url).send().await });
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.is_redirection() || status.as_u16() == 405 {
                checks.push(Check::new(
                    id,
                    category,
                    Status::Pass,
                    format!(
                        "{} reachable ({}ms, HTTP {})",
                        label,
                        elapsed_ms,
                        status.as_u16()
                    ),
                ));
            } else {
                checks.push(Check::new(
                    id,
                    category,
                    Status::Warn,
                    format!(
                        "{} returned HTTP {} after {}ms",
                        label,
                        status.as_u16(),
                        elapsed_ms
                    ),
                ));
            }
        }
        Err(e) => {
            checks.push(
                Check::new(
                    id,
                    category,
                    Status::Fail,
                    format!("{} unreachable after {}ms: {}", label, elapsed_ms, e),
                )
                .with_fix("check network connectivity / firewall / proxy settings"),
            );
        }
    }
}

// ---------- Launch test ----------

fn check_launch(checks: &mut Vec<Check>) {
    let category = "Launch test";

    if env::var("AGENT_BROWSER_PROVIDER").is_ok() {
        checks.push(Check::new(
            "launch.skipped.provider",
            category,
            Status::Info,
            "Skipped (AGENT_BROWSER_PROVIDER is set; would consume cloud quota)",
        ));
        return;
    }
    if env::var("AGENT_BROWSER_CDP").is_ok() {
        checks.push(Check::new(
            "launch.skipped.cdp",
            category,
            Status::Info,
            "Skipped (AGENT_BROWSER_CDP is set; would attach to a real browser)",
        ));
        return;
    }

    let session = format!(
        "doctor-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );

    // Armed after `ensure_daemon` succeeds so we don't send a stray `close`
    // or delete sidecar files for a daemon that never started. On every early
    // return past the `Some(...)` assignment below, Drop runs one close and
    // one `cleanup_stale_files`.
    let mut _guard: Option<LaunchGuard> = None;

    let opts = DaemonOptions {
        headed: false,
        debug: false,
        executable_path: None,
        extensions: &[],
        args: None,
        user_agent: None,
        proxy: None,
        proxy_bypass: None,
        proxy_username: None,
        proxy_password: None,
        ignore_https_errors: false,
        allow_file_access: false,
        profile: None,
        state: None,
        provider: None,
        device: None,
        session_name: None,
        download_path: None,
        allowed_domains: None,
        action_policy: None,
        confirm_actions: None,
        engine: None,
        auto_connect: false,
        idle_timeout: None,
        default_timeout: None,
        cdp: None,
        no_auto_dialog: false,
    };

    let started = Instant::now();
    if let Err(e) = ensure_daemon(&session, &opts) {
        checks.push(
            Check::new(
                "launch.daemon",
                category,
                Status::Fail,
                format!("Could not start daemon: {}", e),
            )
            .with_fix("check Chrome install and re-run with --debug"),
        );
        return;
    }
    _guard = Some(LaunchGuard {
        session: session.clone(),
    });

    let launch_cmd = json!({
        "id": new_id(),
        "action": "launch",
        "headless": true,
    });
    if let Err(e) = send_json(launch_cmd, &session) {
        checks.push(
            Check::new(
                "launch.launch",
                category,
                Status::Fail,
                format!("Browser launch failed: {}", e),
            )
            .with_fix("agent-browser install   # or check --debug output"),
        );
        return;
    }

    let open_cmd = json!({
        "id": new_id(),
        "action": "navigate",
        "url": "about:blank",
    });
    if let Err(e) = send_json(open_cmd, &session) {
        checks.push(
            Check::new(
                "launch.navigate",
                category,
                Status::Fail,
                format!("Navigation to about:blank failed: {}", e),
            )
            .with_fix("re-run with --debug for full launch logs"),
        );
        return;
    }

    // Close + stale-file cleanup happen exactly once via LaunchGuard::drop at
    // end of scope; no explicit close here.
    let elapsed = started.elapsed();
    let secs = elapsed.as_secs_f64();
    if elapsed > Duration::from_secs(5) {
        checks.push(Check::new(
            "launch.elapsed",
            category,
            Status::Warn,
            format!(
                "Headless launch + about:blank in {:.2}s (slow; expected < 5s)",
                secs
            ),
        ));
    } else {
        checks.push(Check::new(
            "launch.elapsed",
            category,
            Status::Pass,
            format!("Headless launch + about:blank in {:.2}s", secs),
        ));
    }
}

fn send_json(cmd: Value, session: &str) -> Result<(), String> {
    match send_command(cmd, session) {
        Ok(resp) => {
            if resp.success {
                Ok(())
            } else {
                Err(resp.error.unwrap_or_else(|| "unknown error".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

fn new_id() -> String {
    format!(
        "doctor-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0)
    )
}

/// Best-effort cleanup when the launch test panics or returns early.
struct LaunchGuard {
    session: String,
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        let close_cmd = json!({ "id": new_id(), "action": "close" });
        let _ = send_command(close_cmd, &self.session);
        cleanup_stale_files(&self.session);
    }
}

// ---------- Fixes ----------

fn run_fixes(checks: &mut [Check], fixed: &mut Vec<String>) {
    // `close_all_sessions` is expensive and closes every session at once, so
    // only fire it on the first daemon.session.* Warn we encounter. Subsequent
    // daemon.session.* Warn checks piggy-back on the same result.
    let mut daemons_closed: Option<usize> = None;

    for c in checks.iter_mut() {
        match c.id.as_str() {
            "chrome.installed" if c.status == Status::Fail => {
                if attempt_chrome_install() {
                    fixed.push("Reinstalled Chrome".to_string());
                    c.status = Status::Pass;
                    c.message = format!("{} (fixed by --fix)", c.message);
                    c.fix = None;
                }
            }
            id if id.starts_with("daemon.session.") && c.status == Status::Warn => {
                let killed = *daemons_closed.get_or_insert_with(|| {
                    let n = close_all_sessions();
                    if n > 0 {
                        fixed.push(format!("Closed {} version-mismatched daemon(s)", n));
                    }
                    n
                });
                if killed > 0 {
                    c.status = Status::Pass;
                    c.message = format!("{} (fixed by --fix)", c.message);
                    c.fix = None;
                }
            }
            "security.state_count" if c.status == Status::Warn => {
                let removed = purge_old_state();
                if removed > 0 {
                    fixed.push(format!("Deleted {} expired state file(s)", removed));
                    c.status = Status::Pass;
                    c.message = format!("{} (fixed by --fix)", c.message);
                    c.fix = None;
                }
            }
            "security.encryption_key" if c.status == Status::Info => {
                if create_encryption_key() {
                    fixed.push("Generated encryption key".to_string());
                    c.status = Status::Pass;
                    c.message = format!("{} (fixed by --fix)", c.message);
                    c.fix = None;
                }
            }
            _ => {}
        }
    }
}

fn attempt_chrome_install() -> bool {
    // run_install() uses process::exit on failure, so we shell out to ourselves
    // to avoid taking down the doctor process if the install fails.
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    std::process::Command::new(exe)
        .arg("install")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn close_all_sessions() -> usize {
    let socket_dir = get_socket_dir();
    let entries = match fs::read_dir(&socket_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut killed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(session) = name.strip_suffix(".pid") {
            if session.is_empty() || session == "dashboard" {
                continue;
            }
            let cmd = json!({ "id": new_id(), "action": "close" });
            if send_command(cmd, session).is_ok() {
                killed += 1;
            }
            cleanup_stale_files(session);
        }
    }
    killed
}

fn purge_old_state() -> usize {
    let dir = get_sessions_dir();
    let expire_days = env::var("AGENT_BROWSER_STATE_EXPIRE_DAYS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(expire_days * 86_400))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = 0;
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if modified < cutoff && fs::remove_file(entry.path()).is_ok() {
                            removed += 1;
                        }
                    }
                }
            }
        }
    }
    removed
}

fn create_encryption_key() -> bool {
    create_encryption_key_at(&get_state_dir())
}

fn create_encryption_key_at(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let path = dir.join(".encryption-key");
    if path.exists() {
        return false;
    }
    let mut buf = [0u8; 32];
    if getrandom::getrandom(&mut buf).is_err() {
        return false;
    }
    let hex: String = buf.iter().map(|b| format!("{:02x}", b)).collect();
    if fs::write(&path, format!("{}\n", hex)).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    true
}

// ---------- Helpers ----------

fn puppeteer_cache_dir() -> Option<PathBuf> {
    if let Ok(p) = env::var("PUPPETEER_CACHE_DIR") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".cache").join("puppeteer"))
}

fn is_writable_dir(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false)
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(unix)]
fn disk_free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // Walk up to the first existing ancestor (for fresh installs where the
    // state dir hasn't been created yet).
    let mut probe: PathBuf = path.to_path_buf();
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => return None,
        }
    }
    let c_path = CString::new(probe.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize)
}

#[cfg(windows)]
fn disk_free_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(not(any(unix, windows)))]
fn disk_free_bytes(_path: &Path) -> Option<u64> {
    None
}

fn which_exists(name: &str) -> bool {
    let probe = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    std::process::Command::new(probe)
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle != 0 {
                CloseHandle(handle);
                true
            } else {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_summary_counts_each_status() {
        let checks = vec![
            Check::new("a", "Cat", Status::Pass, "ok"),
            Check::new("b", "Cat", Status::Pass, "ok"),
            Check::new("c", "Cat", Status::Warn, "meh"),
            Check::new("d", "Cat", Status::Fail, "no"),
            Check::new("e", "Cat", Status::Info, "fyi"),
        ];
        let s = summarize(&checks);
        assert_eq!(s.pass, 2);
        assert_eq!(s.warn, 1);
        assert_eq!(s.fail, 1);
    }

    #[test]
    fn test_human_size_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GB");
        // Multi-unit scaling
        assert_eq!(human_size(1_500_000), "1.4 MB");
    }

    #[test]
    fn test_parse_json_file_valid_and_invalid() {
        let dir = TempDir::new().unwrap();
        let valid = dir.path().join("ok.json");
        fs::write(&valid, r#"{"k": 1}"#).unwrap();
        assert!(parse_json_file(&valid).is_ok());

        let invalid = dir.path().join("bad.json");
        fs::write(&invalid, "{not json}").unwrap();
        let err = parse_json_file(&invalid).unwrap_err();
        assert!(err.contains("invalid JSON"));

        let missing = dir.path().join("nope.json");
        let err = parse_json_file(&missing).unwrap_err();
        assert!(err.contains("read failed"));
    }

    #[test]
    fn test_parse_json_file_accepts_arrays() {
        // The config parser rejects arrays at the Config type level, but
        // doctor only checks syntactic JSON validity so it should accept
        // both arrays and objects.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("arr.json");
        fs::write(&path, r#"[1, 2, 3]"#).unwrap();
        assert!(parse_json_file(&path).is_ok());
    }

    #[test]
    fn test_disk_free_walks_up_to_existing_ancestor() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a/b/c/d");
        let bytes = disk_free_bytes(&nested);
        if cfg!(unix) {
            assert!(bytes.is_some());
            assert!(bytes.unwrap() > 0);
        }
    }

    #[test]
    fn test_status_label_does_not_panic() {
        for s in &[Status::Pass, Status::Warn, Status::Fail, Status::Info] {
            assert!(!s.label().is_empty());
            assert!(!s.as_str().is_empty());
        }
    }

    #[test]
    fn test_status_as_str_values() {
        assert_eq!(Status::Pass.as_str(), "pass");
        assert_eq!(Status::Warn.as_str(), "warn");
        assert_eq!(Status::Fail.as_str(), "fail");
        assert_eq!(Status::Info.as_str(), "info");
    }

    #[test]
    fn test_check_new_and_with_fix() {
        let c = Check::new("id", "cat", Status::Warn, "msg").with_fix("do thing");
        assert_eq!(c.id, "id");
        assert_eq!(c.category, "cat");
        assert_eq!(c.status, Status::Warn);
        assert_eq!(c.message, "msg");
        assert_eq!(c.fix.as_deref(), Some("do thing"));
    }

    #[test]
    fn test_check_new_no_fix_by_default() {
        let c = Check::new("id", "cat", Status::Pass, "msg");
        assert!(c.fix.is_none());
    }

    #[test]
    fn test_summary_zeroes_when_only_info() {
        let checks = vec![Check::new("a", "Cat", Status::Info, "ignored")];
        let s = summarize(&checks);
        assert_eq!(s.pass, 0);
        assert_eq!(s.warn, 0);
        assert_eq!(s.fail, 0);
    }

    #[test]
    fn test_is_writable_dir_matches_metadata() {
        let dir = TempDir::new().unwrap();
        assert!(is_writable_dir(dir.path()));

        let missing = dir.path().join("does-not-exist");
        assert!(!is_writable_dir(&missing));
    }

    #[test]
    fn test_puppeteer_cache_dir_returns_sensible_default() {
        // When PUPPETEER_CACHE_DIR is unset, we fall back to
        // ~/.cache/puppeteer. Mutating env vars here would race with other
        // tests, so just verify the fallback path is shaped correctly.
        if env::var("PUPPETEER_CACHE_DIR").is_err() {
            let dir = puppeteer_cache_dir().expect("home dir should resolve in tests");
            let s = dir.to_string_lossy();
            assert!(s.contains(".cache"));
            assert!(s.ends_with("puppeteer"));
        }
    }

    #[test]
    fn test_new_id_is_unique_per_call() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(a.starts_with("doctor-"));
    }

    #[test]
    fn test_which_exists_matches_common_binaries() {
        // `sh` exists on every unix; `where` / `cmd` exists on windows.
        let probe = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };
        assert!(which_exists(probe));
        assert!(!which_exists(
            "agent-browser-this-does-not-exist-please-dont-install-it"
        ));
    }

    #[test]
    fn test_create_encryption_key_at_writes_64_char_hex_key() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("state");

        assert!(create_encryption_key_at(&dir));

        let key = dir.join(".encryption-key");
        assert!(key.exists(), "key file should be created");

        let contents = fs::read_to_string(&key).unwrap();
        let trimmed = contents.trim();
        assert_eq!(trimmed.len(), 64, "key should be 64 hex chars");
        assert!(
            trimmed.chars().all(|c| c.is_ascii_hexdigit()),
            "key should be all hex digits"
        );
    }

    #[test]
    fn test_create_encryption_key_at_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("state");
        assert!(create_encryption_key_at(&dir));

        let original = fs::read_to_string(dir.join(".encryption-key")).unwrap();

        // Second call returns false and must not overwrite the existing key.
        assert!(!create_encryption_key_at(&dir));
        let after = fs::read_to_string(dir.join(".encryption-key")).unwrap();
        assert_eq!(original, after);
    }

    #[cfg(unix)]
    #[test]
    fn test_create_encryption_key_at_sets_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("state");

        assert!(create_encryption_key_at(&dir));

        let key = dir.join(".encryption-key");
        let mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file should be 0600, got {:o}", mode);
    }

    #[cfg(unix)]
    #[test]
    fn test_run_fixes_generates_missing_encryption_key() {
        // Reaches the Info-status arm in run_fixes that was previously
        // unreachable due to an early-continue guard. Overrides HOME so
        // get_state_dir() resolves under a temp dir.
        let guard = crate::test_utils::EnvGuard::new(&["HOME"]);
        let tmp = TempDir::new().unwrap();
        guard.set("HOME", tmp.path().to_str().unwrap());

        let mut checks = vec![Check::new(
            "security.encryption_key",
            "Security",
            Status::Info,
            "No encryption key set",
        )
        .with_fix("export AGENT_BROWSER_ENCRYPTION_KEY=...")];
        let mut fixed = Vec::new();

        run_fixes(&mut checks, &mut fixed);

        assert_eq!(
            checks[0].status,
            Status::Pass,
            "Info check should transition to Pass after --fix"
        );
        assert!(
            checks[0].fix.is_none(),
            "fix hint should be cleared after repair"
        );
        assert!(
            fixed.iter().any(|s| s.contains("encryption key")),
            "fixed summary should mention the key generation"
        );
        assert!(
            tmp.path().join(".agent-browser/.encryption-key").exists(),
            "key file should exist at ~/.agent-browser/.encryption-key"
        );
    }
}
