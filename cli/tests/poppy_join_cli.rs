//! `poppy join` against a fixture Company (fixtures/poppy/company.mjs) that
//! follows the browser join of the Personal Agent Protocol 1.0 (section 6).
//! The authorizer (fixtures/poppy/authorizer.mjs) performs step 2.
//!
//! The join tests launch Chrome and need node:
//!   cargo test e2e_poppy -- --ignored --test-threads=1

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const TOKEN: &str = "valid-token";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/poppy")
        .join(name)
}

struct Company {
    port: u16,
    child: Child,
}

impl Company {
    fn start(mode: &str) -> Self {
        let mut child = Command::new("node")
            .arg(fixture("company.mjs"))
            .arg(mode)
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to start the fixture Company");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().expect("the fixture printed no port"),
            child,
        }
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn codes(&self) -> Vec<String> {
        let output = Command::new("curl")
            .args(["-s", &format!("{}/_codes", self.origin())])
            .output()
            .unwrap();
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn counter(&self, name: &str) -> u64 {
        let output = Command::new("curl")
            .args(["-s", &format!("{}/_{}", self.origin(), name)])
            .output()
            .unwrap();
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn submits(&self) -> u64 {
        self.counter("submits")
    }

    fn redirects(&self) -> u64 {
        self.counter("redirects")
    }
}

impl Drop for Company {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Session {
    dir: TempDir,
}

impl Session {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir.path())
            .env("NO_COLOR", "1")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_ALLOWED_DOMAINS")
            .current_dir(self.dir.path())
            .args(["--session", "poppy-join", "--json"]);
        command
    }

    fn join(&self, company: &Company, env: &[(&str, &str)]) -> Output {
        self.command()
            .args(["poppy", "join", &company.origin(), "--authorizer"])
            .arg(format!("node \"{}\"", fixture("authorizer.mjs").display()))
            .envs(env.iter().copied())
            .output()
            .expect("failed to run agent-browser")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("failed to run agent-browser")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.command().arg("close").output();
    }
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON: {e}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_failure(output: &Output, code: &str) {
    let value = stdout_json(output);
    assert_eq!(output.status.code(), Some(1), "{value}");
    assert_eq!(value["success"], false, "{value}");
    assert_eq!(value["code"], code, "{value}");
    assert_eq!(value["data"]["experimental"], true, "{value}");
    assert_eq!(value["data"]["joined"], false, "{value}");
}

fn assert_blank(session: &Session) {
    let url = stdout_json(&session.run(&["get", "url"]));
    assert_eq!(url["data"]["url"], "about:blank", "{url}");
}

fn assert_code_absent(company: &Company, output: &Output) {
    let codes = company.codes();
    assert!(!codes.is_empty(), "the fixture issued no code");
    let all = text(output);
    for code in codes {
        assert!(!all.contains(&code), "the code reached the output: {all}");
    }
}

#[test]
fn poppy_join_requires_an_authorizer() {
    let session = Session::new();
    let output = session
        .command()
        .args(["poppy", "join", "https://example.com"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output).contains("poppy join"), "{}", text(&output));
}

#[test]
#[ignore]
fn e2e_poppy_join_signed_out() {
    let company = Company::start("normal");
    let session = Session::new();
    let output = session.join(&company, &[]);
    let value = stdout_json(&output);
    assert_eq!(output.status.code(), Some(0), "{value}");
    assert_eq!(value["success"], true, "{value}");
    let data = &value["data"];
    assert_eq!(data["experimental"], true, "{value}");
    assert_eq!(data["joined"], true, "{value}");
    assert_eq!(data["signed_in"], false, "{value}");
    assert_eq!(data["url"], format!("{}/", company.origin()), "{value}");
    assert_code_absent(&company, &output);

    let snapshot = session.run(&["snapshot"]);
    assert!(
        text(&snapshot).contains("signed out session"),
        "{}",
        text(&snapshot)
    );
}

#[test]
#[ignore]
fn e2e_poppy_join_signed_in() {
    let company = Company::start("normal");
    let session = Session::new();
    let output = session.join(&company, &[("POPPY_TEST_TOKEN", TOKEN)]);
    let value = stdout_json(&output);
    assert_eq!(output.status.code(), Some(0), "{value}");
    assert_eq!(value["data"]["joined"], true, "{value}");
    assert_eq!(value["data"]["signed_in"], true, "{value}");
    assert_code_absent(&company, &output);

    let snapshot = session.run(&["snapshot"]);
    assert!(
        text(&snapshot).contains("signed in as user_1"),
        "{}",
        text(&snapshot)
    );
}

#[test]
#[ignore]
fn e2e_poppy_join_passes_authorizer_errors_through() {
    for (mode, token, code) in [
        ("scope", TOKEN, "insufficient_scope"),
        ("normal", "wrong", "invalid_token"),
    ] {
        let company = Company::start(mode);
        let session = Session::new();
        let output = session.join(&company, &[("POPPY_TEST_TOKEN", token)]);
        assert_failure(&output, code);
        assert_code_absent(&company, &output);
        assert_blank(&session);
    }
}

#[test]
#[ignore]
fn e2e_poppy_join_reports_a_rejected_form() {
    let company = Company::start("reject");
    let session = Session::new();
    let output = session.join(&company, &[]);
    assert_failure(&output, "join_failed");
    assert_eq!(company.submits(), 1, "the form was never submitted");
    assert_code_absent(&company, &output);
    assert_blank(&session);
}

#[test]
#[ignore]
fn e2e_poppy_join_keeps_the_code_out_of_the_landing_url() {
    let company = Company::start("codeurl");
    let session = Session::new();
    let output = session.join(&company, &[]);
    let value = stdout_json(&output);
    assert_eq!(value["data"]["joined"], true, "{value}");
    assert!(
        value["data"]["url"]
            .as_str()
            .unwrap()
            .contains("[redacted]"),
        "{value}"
    );
    assert_code_absent(&company, &output);
}

#[test]
#[ignore]
fn e2e_poppy_join_fails_when_the_redirect_leaves_the_allowed_domains() {
    let company = Company::start("offsite");
    let session = Session::new();
    let output = session.join(&company, &[("AGENT_BROWSER_ALLOWED_DOMAINS", "127.0.0.1")]);
    assert_failure(&output, "join_failed");
    assert_eq!(company.redirects(), 1, "the site never redirected the join");
    assert!(
        text(&output).contains("outside the allowed domains"),
        "{}",
        text(&output)
    );
    assert_code_absent(&company, &output);
    assert_blank(&session);
}

#[test]
#[ignore]
fn e2e_poppy_join_fails_when_the_redirect_target_does_not_load() {
    let company = Company::start("deadport");
    let session = Session::new();
    let output = session.join(&company, &[]);
    assert_failure(&output, "join_failed");
    assert_eq!(company.redirects(), 1, "the site never redirected the join");
    assert!(text(&output).contains("did not load"), "{}", text(&output));
    assert_code_absent(&company, &output);
    assert_blank(&session);
}
