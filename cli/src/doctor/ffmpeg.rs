//! Check the ffmpeg install that `record` pipes frames into: the binary on
//! PATH, its version, and whether the encoders the recorder selects from the
//! output extension (libvpx for `.webm`, libx264 for `.mp4`) are compiled in.
//!
//! Every outcome here is Pass or Warn, never Fail: recording is the only
//! feature that needs ffmpeg, so a machine without it is still healthy.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::helpers::which_path;
use super::{Check, Status};

/// Encoders `record` picks from the output extension, with the extension
/// each one serves. Both come from the same build flags on every common
/// distribution, so a missing one usually means a stripped-down ffmpeg.
const REQUIRED_ENCODERS: &[(&str, &str)] = &[("libvpx", ".webm"), ("libx264", ".mp4")];

pub(super) fn check(checks: &mut Vec<Check>) {
    let probe = match which_path("ffmpeg") {
        Some(path) => FfmpegProbe::Found {
            version: run_ffmpeg(&path, &["-version"]).and_then(|out| parse_version(&out)),
            encoders: run_ffmpeg(&path, &["-hide_banner", "-encoders"])
                .map(|out| parse_encoders(&out)),
            path,
        },
        None => FfmpegProbe::Missing,
    };
    push_checks(checks, &probe, std::env::consts::OS);
}

/// What the probe learned about ffmpeg, separated from the process calls so
/// the check logic can be exercised without an ffmpeg install.
pub(super) enum FfmpegProbe {
    Missing,
    Found {
        path: PathBuf,
        /// First line of `ffmpeg -version` without the copyright notice.
        version: Option<String>,
        /// Encoder names from `ffmpeg -encoders`; `None` when the listing
        /// could not be read.
        encoders: Option<Vec<String>>,
    },
}

fn push_checks(checks: &mut Vec<Check>, probe: &FfmpegProbe, os: &str) {
    let category = "Recording";

    let (path, version, encoders) = match probe {
        FfmpegProbe::Missing => {
            checks.push(
                Check::new(
                    "recording.ffmpeg",
                    category,
                    Status::Warn,
                    "ffmpeg not found on PATH (only needed for `record`)",
                )
                .with_fix(install_hint(os)),
            );
            return;
        }
        FfmpegProbe::Found {
            path,
            version,
            encoders,
        } => (path, version, encoders),
    };

    let label = path.display();
    let message = match version {
        Some(version) => format!("{} at {}", version, label),
        None => format!("ffmpeg at {} (version unknown)", label),
    };
    checks.push(Check::new(
        "recording.ffmpeg",
        category,
        Status::Pass,
        message,
    ));

    let Some(encoders) = encoders else {
        checks.push(
            Check::new(
                "recording.ffmpeg_encoders",
                category,
                Status::Warn,
                "Could not list ffmpeg encoders (`ffmpeg -encoders` failed)",
            )
            .with_fix(reinstall_hint(os)),
        );
        return;
    };

    let missing: Vec<&(&str, &str)> = REQUIRED_ENCODERS
        .iter()
        .filter(|(name, _)| !encoders.iter().any(|e| e == name))
        .collect();

    if missing.is_empty() {
        let available: Vec<String> = REQUIRED_ENCODERS
            .iter()
            .map(|(name, ext)| format!("{} ({})", name, ext))
            .collect();
        checks.push(Check::new(
            "recording.ffmpeg_encoders",
            category,
            Status::Pass,
            format!("{} encoders available", available.join(" and ")),
        ));
    } else {
        let names: Vec<String> = missing
            .iter()
            .map(|(name, ext)| format!("{} ({} recordings)", name, ext))
            .collect();
        checks.push(
            Check::new(
                "recording.ffmpeg_encoders",
                category,
                Status::Warn,
                format!(
                    "ffmpeg build is missing the {} encoder{}; those recordings will fail",
                    names.join(" and "),
                    if missing.len() == 1 { "" } else { "s" }
                ),
            )
            .with_fix(reinstall_hint(os)),
        );
    }
}

/// Package-manager command for a machine without ffmpeg.
fn install_hint(os: &str) -> String {
    match os {
        "macos" => "brew install ffmpeg".to_string(),
        "linux" => "apt install ffmpeg (Debian/Ubuntu)".to_string(),
        _ => "install ffmpeg from https://ffmpeg.org/download.html and add it to PATH".to_string(),
    }
}

/// Hint for a machine whose ffmpeg is present but lacks an encoder.
fn reinstall_hint(os: &str) -> String {
    format!(
        "reinstall ffmpeg with libvpx and libx264: {}",
        install_hint(os)
    )
}

/// Run `binary` with `args`, returning stdout when it exits successfully.
fn run_ffmpeg(binary: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `ffmpeg version 8.1` from the banner's first line, dropping the
/// copyright notice that follows it.
fn parse_version(stdout: &str) -> Option<String> {
    let first = stdout.lines().next()?.trim();
    let version = first.split(" Copyright").next().unwrap_or(first).trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

/// Encoder names from `ffmpeg -encoders`. Each entry is a flags column such
/// as `V....D` followed by the name; the legend lines above the table pair
/// a flags column with `=` and are skipped.
fn parse_encoders(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            let flags = tokens.next()?;
            let name = tokens.next()?;
            let is_flags =
                flags.len() == 6 && flags.chars().all(|c| c == '.' || c.is_ascii_uppercase());
            if is_flags && name != "=" {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION_OUTPUT: &str = "ffmpeg version 8.1 Copyright (c) 2000-2026 the FFmpeg developers\nbuilt with Apple clang version 21.0.0\nconfiguration: --prefix=/opt/homebrew --enable-libvpx --enable-libx264\n";

    const ENCODERS_OUTPUT: &str = "Encoders:\n V..... = Video\n A..... = Audio\n S..... = Subtitle\n .F.... = Frame-level multithreading\n ..S... = Slice-level multithreading\n ...X.. = Codec is experimental\n ....B. = Supports draw_horiz_band\n .....D = Supports direct rendering method 1\n ------\n V....D a64multi             Multicolor charset for Commodore 64 (codec a64_multi)\n V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)\n V....D libx264rgb           libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 RGB (codec h264)\n V....D libvpx               libvpx VP8 (codec vp8)\n V....D libvpx-vp9           libvpx VP9 (codec vp9)\n A....D aac                  AAC (Advanced Audio Coding)\n";

    fn found(version: Option<&str>, encoders: Option<&[&str]>) -> FfmpegProbe {
        FfmpegProbe::Found {
            path: PathBuf::from("/opt/homebrew/bin/ffmpeg"),
            version: version.map(String::from),
            encoders: encoders.map(|e| e.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn run(probe: &FfmpegProbe, os: &str) -> Vec<Check> {
        let mut checks = Vec::new();
        push_checks(&mut checks, probe, os);
        checks
    }

    #[test]
    fn parse_version_drops_copyright_notice() {
        assert_eq!(
            parse_version(VERSION_OUTPUT).as_deref(),
            Some("ffmpeg version 8.1")
        );
        assert_eq!(
            parse_version("ffmpeg version n7.1.1-4ubuntu1\n").as_deref(),
            Some("ffmpeg version n7.1.1-4ubuntu1")
        );
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("\n\n"), None);
    }

    #[test]
    fn parse_encoders_reads_names_and_skips_legend() {
        let encoders = parse_encoders(ENCODERS_OUTPUT);
        assert_eq!(
            encoders,
            vec![
                "a64multi",
                "libx264",
                "libx264rgb",
                "libvpx",
                "libvpx-vp9",
                "aac"
            ]
        );
        assert!(parse_encoders("").is_empty());
        assert!(parse_encoders("Encoders:\n ------\n").is_empty());
    }

    #[test]
    fn missing_ffmpeg_is_a_warning_with_an_install_hint() {
        let checks = run(&FfmpegProbe::Missing, "macos");
        assert_eq!(checks.len(), 1);
        let c = &checks[0];
        assert_eq!(c.id, "recording.ffmpeg");
        assert_eq!(c.category, "Recording");
        assert_eq!(c.status, Status::Warn);
        assert!(c.message.contains("not found"), "message: {}", c.message);
        assert!(c.message.contains("record"), "message: {}", c.message);
        assert_eq!(c.fix.as_deref(), Some("brew install ffmpeg"));
    }

    #[test]
    fn install_hint_follows_the_platform() {
        assert_eq!(install_hint("macos"), "brew install ffmpeg");
        assert!(install_hint("linux").starts_with("apt install ffmpeg"));
        assert!(install_hint("windows").contains("ffmpeg.org"));
        assert!(reinstall_hint("linux").contains("libvpx and libx264"));
        assert!(reinstall_hint("linux").contains("apt install ffmpeg"));
    }

    #[test]
    fn complete_build_passes_both_checks() {
        let checks = run(
            &found(
                Some("ffmpeg version 8.1"),
                Some(&["libx264", "libvpx", "aac"]),
            ),
            "macos",
        );
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].id, "recording.ffmpeg");
        assert_eq!(checks[0].status, Status::Pass);
        assert_eq!(
            checks[0].message,
            "ffmpeg version 8.1 at /opt/homebrew/bin/ffmpeg"
        );
        assert!(checks[0].fix.is_none());
        assert_eq!(checks[1].id, "recording.ffmpeg_encoders");
        assert_eq!(checks[1].status, Status::Pass);
        assert!(checks[1].message.contains("libvpx (.webm)"));
        assert!(checks[1].message.contains("libx264 (.mp4)"));
        assert!(checks[1].fix.is_none());
    }

    #[test]
    fn unknown_version_still_passes() {
        let checks = run(&found(None, Some(&["libx264", "libvpx"])), "linux");
        assert_eq!(checks[0].status, Status::Pass);
        assert_eq!(
            checks[0].message,
            "ffmpeg at /opt/homebrew/bin/ffmpeg (version unknown)"
        );
    }

    #[test]
    fn missing_encoder_warns_and_names_the_extension_it_breaks() {
        let checks = run(
            &found(Some("ffmpeg version 8.1"), Some(&["libx264"])),
            "linux",
        );
        assert_eq!(checks.len(), 2);
        let c = &checks[1];
        assert_eq!(c.status, Status::Warn);
        assert!(
            c.message.contains("libvpx (.webm recordings)"),
            "{}",
            c.message
        );
        assert!(!c.message.contains("libx264"), "{}", c.message);
        assert!(
            c.message.ends_with("encoder; those recordings will fail"),
            "{}",
            c.message
        );
        assert!(c.fix.as_deref().unwrap().contains("apt install ffmpeg"));

        let both = run(&found(Some("ffmpeg version 8.1"), Some(&[])), "macos");
        assert!(
            both[1]
                .message
                .contains("libvpx (.webm recordings) and libx264 (.mp4 recordings) encoders"),
            "{}",
            both[1].message
        );
    }

    #[test]
    fn unreadable_encoder_list_warns() {
        let checks = run(&found(Some("ffmpeg version 8.1"), None), "macos");
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[1].id, "recording.ffmpeg_encoders");
        assert_eq!(checks[1].status, Status::Warn);
        assert!(checks[1].message.contains("-encoders"));
        assert!(checks[1].fix.is_some());
    }

    #[test]
    fn recording_checks_never_fail() {
        // Recording is optional, so no ffmpeg state may flip doctor's exit
        // code to 1.
        let probes = [
            FfmpegProbe::Missing,
            found(None, None),
            found(None, Some(&[])),
            found(Some("ffmpeg version 8.1"), Some(&["libvpx", "libx264"])),
        ];
        for probe in &probes {
            for os in ["macos", "linux", "windows"] {
                for c in run(probe, os) {
                    assert_ne!(c.status, Status::Fail, "{}: {}", c.id, c.message);
                }
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn run_ffmpeg_returns_stdout_on_success_only() {
        let sh = Path::new("/bin/sh");
        assert_eq!(
            run_ffmpeg(sh, &["-c", "echo 'ffmpeg version 8.1 Copyright'"]).as_deref(),
            Some("ffmpeg version 8.1 Copyright\n")
        );
        assert_eq!(run_ffmpeg(sh, &["-c", "echo nope; exit 1"]), None);
        assert_eq!(
            run_ffmpeg(
                Path::new("/nonexistent/agent-browser-ffmpeg"),
                &["-version"]
            ),
            None
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn run_ffmpeg_parses_a_scripted_encoder_listing() {
        let script = format!("printf '%s' '{}'", ENCODERS_OUTPUT);
        let out = run_ffmpeg(Path::new("/bin/sh"), &["-c", &script]).unwrap();
        let encoders = parse_encoders(&out);
        assert!(encoders.iter().any(|e| e == "libvpx"));
        assert!(encoders.iter().any(|e| e == "libx264"));
    }
}
