#![cfg(unix)]

use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

struct Session {
    dir: TempDir,
}

impl Session {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        let binary = dir.path().join("moli");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$@\" > moli-args\nprintf '%s/%s' \"${NO_PROXY-unset}\" \"${no_proxy-unset}\" > moli-env\nexit 23\n").unwrap();
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir.path())
            .env("NO_COLOR", "1")
            .current_dir(self.dir.path())
            .args(["--config", "config.json", "--session", "m", "--json"])
            .stdin(Stdio::null());
        command
    }

    fn prime(&self) -> String {
        let output = self
            .command()
            .env("NO_PROXY", "stale-bypass")
            .args(["stream", "status"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        std::fs::read_to_string(self.dir.path().join("m.pid")).unwrap()
    }

    fn launch(&self, options: Value) -> Vec<String> {
        let mut config = json!({"engine": "moli", "executablePath": self.dir.path().join("moli")});
        config
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        std::fs::write(self.dir.path().join("config.json"), config.to_string()).unwrap();
        let output = self.command().arg("open").output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            response["error"]
                .as_str()
                .is_some_and(|error| error.contains("Moli exited before CDP became ready")),
            "{response}"
        );
        assert_eq!(
            std::fs::read_to_string(self.dir.path().join("moli-env")).unwrap(),
            "unset/unset"
        );
        std::fs::read_to_string(self.dir.path().join("moli-args"))
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }

    fn close(&self) -> std::io::Result<std::process::Output> {
        // Closing must not try to launch the intentionally failing fixture.
        std::fs::write(self.dir.path().join("config.json"), "{}")?;
        let output = self.command().arg("close").output()?;
        if output.status.success() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while self.dir.path().join("m.pid").exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        Ok(output)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.dir.path().join("m.pid").exists() {
            let _ = self.close();
        }
    }
}

#[test]
fn moli_existing_daemon_uses_current_config_and_clears_stale_bypass() {
    let session = Session::new();
    let pid = session.prime();
    let args = session.launch(json!({
        "profile": "moli profile",
        "args": "--timeout=30,\n--http-cache-dir=Moli cache",
        "proxy": "http://user:p%40ss:w0rd@127.0.0.1:8080", "proxyBypass": "localhost",
        "userAgent": "Adapter probe", "ignoreHttpsErrors": true, "hideScrollbars": false,
    }));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--profile-dir", "moli profile"]),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--http-proxy", "http://user:p%40ss%3Aw0rd@127.0.0.1:8080"]),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--http-no-proxy", "localhost"]),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--user-agent", "Adapter probe"]),
        "{args:?}"
    );
    assert!(args
        .iter()
        .any(|arg| arg == "--insecure-disable-tls-host-verification"));
    assert!(args.iter().any(|arg| arg == "--scrollbars"));
    assert!(args.ends_with(&["--timeout=30".into(), "--http-cache-dir=Moli cache".into(),]));
    for flag in ["--layout", "--image", "--font"] {
        assert!(args.iter().any(|arg| arg == flag), "{args:?}");
    }
    let args = session.launch(json!({}));
    assert!(!args.iter().any(|arg| arg == "--profile-dir"), "{args:?}");
    assert!(!args.iter().any(|arg| arg == "--scrollbars"));
    assert!(!args.iter().any(|arg| arg == "--http-no-proxy"), "{args:?}");
    assert!(!args.iter().any(|arg| arg == "--timeout=30"), "{args:?}");
    assert_eq!(
        std::fs::read_to_string(session.dir.path().join("m.pid")).unwrap(),
        pid
    );
    let closed = session.close().unwrap();
    assert!(closed.status.success(), "{closed:?}");
    let response: Value = serde_json::from_slice(&closed.stdout).unwrap();
    assert_eq!(response["data"]["closed"], true, "{response}");
    assert!(!session.dir.path().join("m.pid").exists());
    assert!(!session.dir.path().join("m.sock").exists());
}
