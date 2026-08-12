use std::ffi::OsStr;
use std::process::Command;

/// Build a child command that stays invisible when launched by the detached daemon.
pub(crate) fn background_std_command<S: AsRef<OsStr>>(program: S) -> Command {
    let command = Command::new(program);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut command = command;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        command
    }

    #[cfg(not(windows))]
    {
        command
    }
}

pub(crate) fn background_tokio_command<S: AsRef<OsStr>>(program: S) -> tokio::process::Command {
    let command = background_std_command(program);
    tokio::process::Command::from(command)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    const DETACHED_PARENT_ENV: &str = "AGENT_BROWSER_TEST_DETACHED_PARENT";
    const DETACHED_PARENT_TEST: &str =
        "native::process::tests::detached_parent_spawns_background_child";
    const CONSOLE_PROBE_ENV: &str = "AGENT_BROWSER_TEST_CONSOLE_PROBE";
    const CONSOLE_PROBE_TEST: &str = "native::process::tests::console_probe_has_no_console";
    const CHILD_KIND_ENV: &str = "AGENT_BROWSER_TEST_CHILD_KIND";

    #[test]
    fn configured_background_children_have_no_console() {
        for child_kind in ["std", "tokio"] {
            let output = detached_parent(child_kind).output().unwrap();
            assert!(
                output.status.success(),
                "{child_kind} console probe failed:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn detached_parent(child_kind: &str) -> Command {
        use std::os::windows::process::CommandExt;

        let current_exe = std::env::current_exe().unwrap();
        let mut command = Command::new(current_exe);
        command
            .args(["--exact", DETACHED_PARENT_TEST, "--ignored"])
            .env(DETACHED_PARENT_ENV, "1")
            .env(CHILD_KIND_ENV, child_kind)
            .creation_flags(windows_sys::Win32::System::Threading::DETACHED_PROCESS);
        command
    }

    #[test]
    #[ignore]
    fn detached_parent_spawns_background_child() {
        if std::env::var_os(DETACHED_PARENT_ENV).is_none() {
            return;
        }

        // SAFETY: GetConsoleCP has no parameters and only inspects this process's console.
        assert_eq!(
            unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() },
            0,
            "intermediate process was not detached"
        );

        let current_exe = std::env::current_exe().unwrap();
        let output = match std::env::var(CHILD_KIND_ENV).unwrap().as_str() {
            "std" => background_std_command(current_exe)
                .args(["--exact", CONSOLE_PROBE_TEST, "--ignored"])
                .env(CONSOLE_PROBE_ENV, "1")
                .output()
                .unwrap(),
            "tokio" => {
                let mut command = background_tokio_command(current_exe);
                command
                    .args(["--exact", CONSOLE_PROBE_TEST, "--ignored"])
                    .env(CONSOLE_PROBE_ENV, "1");
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(command.output())
                    .unwrap()
            }
            child_kind => panic!("unknown child kind: {child_kind}"),
        };
        assert!(
            output.status.success(),
            "background child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore]
    fn console_probe_has_no_console() {
        if std::env::var_os(CONSOLE_PROBE_ENV).is_none() {
            return;
        }

        // SAFETY: GetConsoleWindow has no parameters and only inspects this process's console.
        let console = unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() };
        assert_eq!(console, 0, "background child created a console window");
    }
}
