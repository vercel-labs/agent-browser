use crate::color;
use std::process::{exit, Command, Stdio};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/agent-browser/latest";

enum InstallMethod {
    Npm,
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

fn detect_install_method() -> InstallMethod {
    // Check Homebrew (available on macOS and Linux)
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let brew_check = Command::new("brew")
            .args(["list", "agent-browser"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if brew_check.map(|s| s.success()).unwrap_or(false) {
            return InstallMethod::Homebrew;
        }
    }

    // Check Cargo installation by executable path
    if let Ok(exe) = std::env::current_exe() {
        let path_str = exe.to_string_lossy();
        if path_str.contains("/.cargo/bin/") || path_str.contains("\\.cargo\\bin\\") {
            return InstallMethod::Cargo;
        }
    }

    // Check npm global installation
    let npm_check = Command::new("npm")
        .args(["list", "-g", "agent-browser", "--depth=0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if npm_check.map(|s| s.success()).unwrap_or(false) {
        return InstallMethod::Npm;
    }

    InstallMethod::Unknown
}

fn run_upgrade_command(method: &InstallMethod) -> bool {
    match method {
        InstallMethod::Npm => {
            println!("Running: npm install -g agent-browser@latest");
            Command::new("npm")
                .args(["install", "-g", "agent-browser@latest"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        InstallMethod::Homebrew => {
            println!("Running: brew upgrade agent-browser");
            Command::new("brew")
                .args(["upgrade", "agent-browser"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        InstallMethod::Cargo => {
            println!("Running: cargo install agent-browser --force");
            Command::new("cargo")
                .args(["install", "agent-browser", "--force"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        InstallMethod::Unknown => false,
    }
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
        eprintln!("    npm install -g agent-browser@latest     # npm");
        eprintln!("    brew upgrade agent-browser               # Homebrew");
        eprintln!("    cargo install agent-browser --force      # Cargo");
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
