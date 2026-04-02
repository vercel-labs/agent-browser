use crate::color;
use std::path::Path;
use std::process::{exit, Command, Stdio};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/agent-browser/latest";

enum InstallMethod {
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Homebrew,
    Cargo,
    Unknown,
}

async fn fetch_latest_version() -> Result<String, String> {
    let resp = reqwest::get(NPM_REGISTRY_URL)
        .await
        .map_err(|e| format!("Failed to fetch version info: {}", e))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse version info: {}", e))?;

    body.get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No version field in registry response".to_string())
}

/// Parse the `.install-method` marker written by postinstall.js.
fn read_install_method_marker(exe_dir: &Path) -> Option<InstallMethod> {
    let contents = std::fs::read_to_string(exe_dir.join(".install-method")).ok()?;
    match contents.trim() {
        "npm" => Some(InstallMethod::Npm),
        "pnpm" => Some(InstallMethod::Pnpm),
        "yarn" => Some(InstallMethod::Yarn),
        "bun" => Some(InstallMethod::Bun),
        _ => None,
    }
}

fn detect_install_method() -> InstallMethod {
    if let Ok(exe) = std::env::current_exe() {
        // Resolve symlinks to find the real binary location
        let real_path = exe.canonicalize().unwrap_or(exe);

        // Preferred: read the marker file written at install time
        if let Some(dir) = real_path.parent() {
            if let Some(method) = read_install_method_marker(dir) {
                return method;
            }
        }

        // Fallback: infer from executable path
        let path_str = real_path.to_string_lossy();

        if path_str.contains("/.cargo/bin/") || path_str.contains("\\.cargo\\bin\\") {
            return InstallMethod::Cargo;
        }

        if path_str.contains("/Cellar/agent-browser/")
            || path_str.contains("/homebrew/")
            || path_str.contains("/linuxbrew/")
        {
            return InstallMethod::Homebrew;
        }

        if path_str.contains("/pnpm/") || path_str.contains("/pnpm-global/") {
            return InstallMethod::Pnpm;
        }

        if path_str.contains("/.yarn/") || path_str.contains("/yarn/global/") {
            return InstallMethod::Yarn;
        }

        if path_str.contains("/.bun/") {
            return InstallMethod::Bun;
        }

        if path_str.contains("node_modules/agent-browser")
            || path_str.contains("node_modules\\agent-browser")
        {
            return InstallMethod::Npm;
        }
    }

    // Last resort: probe package managers via subprocess

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if command_succeeds("brew", &["list", "agent-browser"]) {
            return InstallMethod::Homebrew;
        }
    }

    if command_output_contains(
        "pnpm",
        &["list", "-g", "agent-browser", "--depth=0"],
        "agent-browser",
    ) {
        return InstallMethod::Pnpm;
    }

    if command_output_contains("yarn", &["global", "list", "--depth=0"], "agent-browser") {
        return InstallMethod::Yarn;
    }

    if command_output_contains("bun", &["pm", "ls", "-g"], "agent-browser") {
        return InstallMethod::Bun;
    }

    if command_succeeds("npm", &["list", "-g", "agent-browser", "--depth=0"]) {
        return InstallMethod::Npm;
    }

    InstallMethod::Unknown
}

fn command_succeeds(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn command_output_contains(cmd: &str, args: &[&str], needle: &str) -> bool {
    Command::new(cmd)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains(needle))
        .unwrap_or(false)
}

fn run_upgrade_command(method: &InstallMethod) -> bool {
    let (cmd, args, display): (&str, &[&str], &str) = match method {
        InstallMethod::Npm => (
            "npm",
            &["install", "-g", "agent-browser@latest"],
            "npm install -g agent-browser@latest",
        ),
        InstallMethod::Pnpm => (
            "pnpm",
            &["add", "-g", "agent-browser@latest"],
            "pnpm add -g agent-browser@latest",
        ),
        // NOTE: `yarn global` is Yarn Classic (v1) only; Yarn Berry (v2+) removed it.
        // Users on Yarn v2+ won't reach this path — detection falls through to Unknown.
        InstallMethod::Yarn => (
            "yarn",
            &["global", "add", "agent-browser@latest"],
            "yarn global add agent-browser@latest",
        ),
        InstallMethod::Bun => (
            "bun",
            &["install", "-g", "agent-browser@latest"],
            "bun install -g agent-browser@latest",
        ),
        InstallMethod::Homebrew => (
            "brew",
            &["upgrade", "agent-browser"],
            "brew upgrade agent-browser",
        ),
        InstallMethod::Cargo => (
            "cargo",
            &["install", "agent-browser", "--force"],
            "cargo install agent-browser --force",
        ),
        InstallMethod::Unknown => return false,
    };

    println!("Running: {}", display);
    Command::new(cmd)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn run_upgrade() {
    let current = CURRENT_VERSION;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!(
                "{} Failed to create runtime: {}",
                color::error_indicator(),
                e
            );
            exit(1);
        });

    let latest = match rt.block_on(fetch_latest_version()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "{} Could not check latest version: {}",
                color::warning_indicator(),
                e
            );
            String::new()
        }
    };

    if !latest.is_empty() && current == latest.as_str() {
        println!(
            "{} agent-browser is already at the latest version (v{})",
            color::success_indicator(),
            current
        );
        return;
    }

    let method = detect_install_method();

    let method_name = match &method {
        InstallMethod::Npm => "npm",
        InstallMethod::Pnpm => "pnpm",
        InstallMethod::Yarn => "yarn",
        InstallMethod::Bun => "bun",
        InstallMethod::Homebrew => "Homebrew",
        InstallMethod::Cargo => "Cargo",
        InstallMethod::Unknown => "",
    };

    if matches!(method, InstallMethod::Unknown) {
        eprintln!(
            "{} Could not detect installation method.",
            color::error_indicator()
        );
        eprintln!("  To update manually, run one of:");
        eprintln!("    npm install -g agent-browser@latest       # npm");
        eprintln!("    pnpm add -g agent-browser@latest          # pnpm");
        eprintln!("    yarn global add agent-browser@latest       # yarn");
        eprintln!("    bun install -g agent-browser@latest        # bun");
        eprintln!("    brew upgrade agent-browser                 # Homebrew");
        eprintln!("    cargo install agent-browser --force        # Cargo");
        exit(1);
    }

    println!("Detected installation via {}.", method_name);

    if !latest.is_empty() {
        println!(
            "{}",
            color::cyan(&format!(
                "Upgrading agent-browser... v{} → v{}",
                current, latest
            ))
        );
    } else {
        println!(
            "{}",
            color::cyan(&format!("Upgrading agent-browser (v{})...", current))
        );
    }

    let success = run_upgrade_command(&method);

    if success {
        if !latest.is_empty() {
            println!(
                "{} Done! v{} → v{}",
                color::success_indicator(),
                current,
                latest
            );
        } else {
            println!("{} Done!", color::success_indicator());
        }
    } else {
        eprintln!("{} Upgrade failed.", color::error_indicator());
        exit(1);
    }
}
