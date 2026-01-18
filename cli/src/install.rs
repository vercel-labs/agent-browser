use std::process::{exit, Command, Stdio};

#[cfg(windows)]
use crate::connection::find_npx_executable;

pub fn run_install(with_deps: bool) {
    let is_linux = cfg!(target_os = "linux");

    if is_linux {
        if with_deps {
            println!("\x1b[36mInstalling system dependencies...\x1b[0m");

            let (pkg_mgr, deps) = if which_exists("apt-get") {
                (
                    "apt-get",
                    vec![
                        "libxcb-shm0",
                        "libx11-xcb1",
                        "libx11-6",
                        "libxcb1",
                        "libxext6",
                        "libxrandr2",
                        "libxcomposite1",
                        "libxcursor1",
                        "libxdamage1",
                        "libxfixes3",
                        "libxi6",
                        "libgtk-3-0",
                        "libpangocairo-1.0-0",
                        "libpango-1.0-0",
                        "libatk1.0-0",
                        "libcairo-gobject2",
                        "libcairo2",
                        "libgdk-pixbuf-2.0-0",
                        "libxrender1",
                        "libasound2",
                        "libfreetype6",
                        "libfontconfig1",
                        "libdbus-1-3",
                        "libnss3",
                        "libnspr4",
                        "libatk-bridge2.0-0",
                        "libdrm2",
                        "libxkbcommon0",
                        "libatspi2.0-0",
                        "libcups2",
                        "libxshmfence1",
                        "libgbm1",
                    ],
                )
            } else if which_exists("dnf") {
                (
                    "dnf",
                    vec![
                        "nss",
                        "nspr",
                        "atk",
                        "at-spi2-atk",
                        "cups-libs",
                        "libdrm",
                        "libXcomposite",
                        "libXdamage",
                        "libXrandr",
                        "mesa-libgbm",
                        "pango",
                        "alsa-lib",
                        "libxkbcommon",
                        "libxcb",
                        "libX11-xcb",
                        "libX11",
                        "libXext",
                        "libXcursor",
                        "libXfixes",
                        "libXi",
                        "gtk3",
                        "cairo-gobject",
                    ],
                )
            } else if which_exists("yum") {
                (
                    "yum",
                    vec![
                        "nss",
                        "nspr",
                        "atk",
                        "at-spi2-atk",
                        "cups-libs",
                        "libdrm",
                        "libXcomposite",
                        "libXdamage",
                        "libXrandr",
                        "mesa-libgbm",
                        "pango",
                        "alsa-lib",
                        "libxkbcommon",
                    ],
                )
            } else {
                eprintln!("\x1b[31m✗\x1b[0m No supported package manager found (apt-get, dnf, or yum)");
                exit(1);
            };

            let install_cmd = match pkg_mgr {
                "apt-get" => {
                    format!(
                        "sudo apt-get update && sudo apt-get install -y {}",
                        deps.join(" ")
                    )
                }
                _ => format!("sudo {} install -y {}", pkg_mgr, deps.join(" ")),
            };

            println!("Running: {}", install_cmd);
            let status = Command::new("sh").arg("-c").arg(&install_cmd).status();

            match status {
                Ok(s) if s.success() => {
                    println!("\x1b[32m✓\x1b[0m System dependencies installed")
                }
                Ok(_) => eprintln!(
                    "\x1b[33m⚠\x1b[0m Failed to install some dependencies. You may need to run manually with sudo."
                ),
                Err(e) => eprintln!("\x1b[33m⚠\x1b[0m Could not run install command: {}", e),
            }
        } else {
            println!("\x1b[33m⚠\x1b[0m Linux detected. If browser fails to launch, run:");
            println!("  agent-browser install --with-deps");
            println!("  or: npx playwright install-deps chromium");
            println!();
        }
    }

    println!("\x1b[36mInstalling Chromium browser...\x1b[0m");

    // On Windows, find npx executable checking fnm directories
    // This fixes the issue where fnm's temporary shell directories aren't inherited
    #[cfg(windows)]
    let status = {
        match find_npx_executable() {
            Some(npx_path) => {
                Command::new(&npx_path)
                    .args(["playwright", "install", "chromium"])
                    .status()
            }
            None => {
                eprintln!("\x1b[31m✗\x1b[0m npx not found. Please ensure Node.js is installed.");
                eprintln!("If using fnm, make sure a Node.js version is installed.");
                exit(1);
            }
        }
    };

    #[cfg(not(windows))]
    let status = Command::new("npx")
        .args(["playwright", "install", "chromium"])
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("\x1b[32m✓\x1b[0m Chromium installed successfully");
            if is_linux && !with_deps {
                println!();
                println!("\x1b[33mNote:\x1b[0m If you see \"shared library\" errors when running, use:");
                println!("  agent-browser install --with-deps");
            }
        }
        Ok(_) => {
            eprintln!("\x1b[31m✗\x1b[0m Failed to install browser");
            if is_linux {
                println!("\x1b[33mTip:\x1b[0m Try installing system dependencies first:");
                println!("  agent-browser install --with-deps");
            }
            exit(1);
        }
        Err(e) => {
            eprintln!("\x1b[31m✗\x1b[0m Failed to run npx: {}", e);
            eprintln!("Make sure Node.js is installed and npx is in your PATH");
            exit(1);
        }
    }
}

fn which_exists(cmd: &str) -> bool {
    #[cfg(unix)]
    {
        Command::new("which")
            .arg(cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        Command::new("where")
            .arg(cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}
