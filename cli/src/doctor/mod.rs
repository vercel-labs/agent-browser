//! Diagnose an agent-browser installation.
//!
//! Runs a battery of checks across environment, Chrome install, daemon
//! state, config files, encryption, providers, network reachability, and
//! a live headless browser launch test.
//!
//! Auto-cleans stale daemon socket/pid/version sidecar files. Destructive
//! repairs (reinstalling Chrome, purging old state files, generating a
//! missing encryption key) are gated behind `--fix`.

mod camoufox;
mod chrome;
mod config;
mod daemon;
mod environment;
mod fix;
mod helpers;
mod launch;
mod network;
mod providers;
mod security;

use serde_json::{json, Value};

use crate::color;

#[derive(Default, Clone, Copy)]
pub struct DoctorOptions {
    pub offline: bool,
    pub quick: bool,
    pub fix: bool,
    pub json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Status {
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
pub(crate) struct Check {
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

    environment::check(&mut checks);
    chrome::check(&mut checks);
    camoufox::check(&mut checks);
    daemon::check(&mut checks);
    config::check(&mut checks);
    security::check(&mut checks);
    providers::check(&mut checks);

    if !opts.offline {
        network::check(&mut checks);
    }

    if !opts.quick {
        launch::check(&mut checks);
    }

    if opts.fix {
        fix::run(&mut checks, &mut fixed);
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_summary_zeroes_when_only_info() {
        let checks = vec![Check::new("a", "Cat", Status::Info, "ignored")];
        let s = summarize(&checks);
        assert_eq!(s.pass, 0);
        assert_eq!(s.warn, 0);
        assert_eq!(s.fail, 0);
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
}
