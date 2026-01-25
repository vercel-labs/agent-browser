use crate::color;
use std::process::{exit, Command, Stdio};

#[cfg(windows)]
use crate::connection::find_npx_executable;

pub fn run_install(with_deps: bool) {
    let is_linux = cfg!(target_os = "linux");

    if is_linux {
        if with_deps {
            println!("{}", color::cyan("Installing system dependencies..."));

            let (pkg_mgr, deps) = if which_exists("apt-get") {
                let libasound = if package_exists_apt("libasound2t64") {
                    "libasound2t64"
                } else {
                    "libasound2"
                };

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
                        libasound,
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
                eprintln!("{} No supported package manager found (apt-get, dnf, or yum)", color::error_indicator());
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
                    println!("{} System dependencies installed", color::success_indicator())
                }
                Ok(_) => eprintln!(
                    "{} Failed to install some dependencies. You may need to run manually with sudo.",
                    color::warning_indicator()
                ),
                Err(e) => eprintln!("{} Could not run install command: {}", color::warning_indicator(), e),
            }
        } else {
            println!("{} Linux detected. If browser fails to launch, run:", color::warning_indicator());
            println!("  agent-browser install --with-deps");
            println!("  or: npx playwright install-deps chromium");
            println!();
        }
    }

    println!("{}", color::cyan("Installing Chromium browser..."));

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
                eprintln!("{} npx not found. Please ensure Node.js is installed.", color::error_indicator());
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
            println!("{} Chromium installed successfully", color::success_indicator());
            if is_linux && !with_deps {
                println!();
                println!("{} If you see \"shared library\" errors when running, use:", color::yellow("Note:"));
                println!("  agent-browser install --with-deps");
            }
        }
        Ok(_) => {
            eprintln!("{} Failed to install browser", color::error_indicator());
            if is_linux {
                println!("{} Try installing system dependencies first:", color::yellow("Tip:"));
                println!("  agent-browser install --with-deps");
            }
            exit(1);
        }
        Err(e) => {
            eprintln!("{} Failed to run npx: {}", color::error_indicator(), e);
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

fn package_exists_apt(pkg: &str) -> bool {
    Command::new("apt-cache")
        .arg("show")
        .arg(pkg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
