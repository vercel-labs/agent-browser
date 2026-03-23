// These three commands have no `valid_options` declared in commands.rs (their parsers use default/fallthrough
// logic), so we define them here as the source of truth for completion coverage.
#[cfg(test)]
pub(crate) const COOKIES_SUBCOMMANDS: &[&str] = &["get", "set", "clear"];
#[cfg(test)]
pub(crate) const TAB_SUBCOMMANDS: &[&str] = &["new", "list", "close"];
#[cfg(test)]
pub(crate) const SCROLL_SUBCOMMANDS: &[&str] = &["up", "down", "left", "right"];

pub fn run_completion(shell: &str) {
    match shell {
        "bash" => print!("{}", include_str!("../completions/agent-browser.bash")),
        "zsh" => print!("{}", include_str!("../completions/_agent_browser")),
        "" => {
            eprintln!("Usage: agent-browser completion <shell>");
            eprintln!("Supported shells: bash, zsh");
            std::process::exit(1);
        }
        other => {
            eprintln!("Unsupported shell: {other}");
            eprintln!("Supported shells: bash, zsh");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{
        all_known_commands, parse_command, ParseError, AUTH_SUBCOMMANDS, CLIPBOARD_SUBCOMMANDS,
        DIALOG_SUBCOMMANDS, DIFF_SUBCOMMANDS, FIND_SUBCOMMANDS, GET_SUBCOMMANDS, HAR_SUBCOMMANDS,
        IS_SUBCOMMANDS, KEYBOARD_SUBCOMMANDS, MOUSE_SUBCOMMANDS, NETWORK_SUBCOMMANDS,
        PROFILER_SUBCOMMANDS, RECORD_SUBCOMMANDS, SESSION_SUBCOMMANDS, SET_SUBCOMMANDS,
        STATE_SUBCOMMANDS, STORAGE_SUBCOMMANDS, TRACE_SUBCOMMANDS, WINDOW_SUBCOMMANDS,
    };
    use crate::flags::Flags;

    fn default_flags() -> Flags {
        Flags {
            session: "test".to_string(),
            json: false,
            headed: false,
            debug: false,
            headers: None,
            executable_path: None,
            extensions: Vec::new(),
            cdp: None,
            profile: None,
            state: None,
            proxy: None,
            proxy_bypass: None,
            args: None,
            user_agent: None,
            provider: None,
            ignore_https_errors: false,
            allow_file_access: false,
            device: None,
            auto_connect: false,
            session_name: None,
            cli_executable_path: false,
            cli_extensions: false,
            cli_profile: false,
            cli_state: false,
            cli_args: false,
            cli_user_agent: false,
            cli_proxy: false,
            cli_proxy_bypass: false,
            cli_allow_file_access: false,
            cli_annotate: false,
            cli_download_path: false,
            cli_headed: false,
            annotate: false,
            color_scheme: None,
            download_path: None,
            content_boundaries: false,
            max_output: None,
            allowed_domains: None,
            action_policy: None,
            confirm_actions: None,
            confirm_interactive: false,
            engine: None,
            screenshot_dir: None,
            screenshot_quality: None,
            screenshot_format: None,
            idle_timeout: None,
        }
    }

    // Global flag coverage

    #[test]
    fn bash_contains_all_global_flags() {
        let bash = include_str!("../completions/agent-browser.bash");
        let failures: Vec<_> = crate::flags::GLOBAL_BOOL_FLAGS
            .iter()
            .chain(crate::flags::GLOBAL_FLAGS_WITH_VALUE.iter())
            .filter(|flag| !bash.contains(*flag))
            .map(|flag| format!("bash missing global flag: {flag}"))
            .collect();
        if !failures.is_empty() {
            panic!("{}", failures.join("\n"));
        }
    }

    #[test]
    fn zsh_contains_all_global_flags() {
        let zsh = include_str!("../completions/_agent_browser");
        let failures: Vec<_> = crate::flags::GLOBAL_BOOL_FLAGS
            .iter()
            .chain(crate::flags::GLOBAL_FLAGS_WITH_VALUE.iter())
            .filter(|flag| !zsh.contains(*flag))
            .map(|flag| format!("zsh missing global flag: {flag}"))
            .collect();
        if !failures.is_empty() {
            panic!("{}", failures.join("\n"));
        }
    }

    // Shell autocompletion script coverage

    #[test]
    fn bash_contains_all_top_level_commands() {
        let script = include_str!("../completions/agent-browser.bash");
        for cmd in all_known_commands() {
            assert!(
                script.contains(cmd),
                "bash completion missing command: {cmd}"
            );
        }
    }

    #[test]
    fn zsh_contains_all_top_level_commands() {
        let script = include_str!("../completions/_agent_browser");
        for cmd in all_known_commands() {
            assert!(
                script.contains(cmd),
                "zsh completion missing command: {cmd}"
            );
        }
    }

    // Validity check: every command in all_known_commands() is a real CLI command

    /// Every parse_command-handled entry in all_known_commands() must be recognized by the CLI.
    /// Catches typos in all_known_commands().
    #[test]
    fn completion_commands_are_all_valid_cli_commands() {
        let flags = default_flags();
        // These commands are handled before parse_command in main.rs
        let pre_dispatch = ["completion", "install", "upgrade", "session"];
        for cmd in all_known_commands() {
            if pre_dispatch.contains(cmd) {
                continue;
            }
            let args = vec![cmd.to_string()];
            match parse_command(&args, &flags) {
                Err(ParseError::UnknownCommand { .. }) => {
                    panic!("'{cmd}' is in all_known_commands() but not recognized by CLI");
                }
                _ => {} // MissingArguments / Ok / other errors are fine — command exists
            }
        }
    }

    // Subcommand lists must appear in all autocompletion scripts

    macro_rules! assert_subcommands_in_scripts {
        ($list:expr, $label:expr) => {
            let bash = include_str!("../completions/agent-browser.bash");
            let zsh = include_str!("../completions/_agent_browser");
            let mut failures = Vec::new();
            for sub in $list {
                if !bash.contains(sub) {
                    failures.push(format!("bash missing {} subcommand: {}", $label, sub));
                }
                if !zsh.contains(sub) {
                    failures.push(format!("zsh missing {} subcommand: {}", $label, sub));
                }
            }
            if !failures.is_empty() {
                panic!("{}", failures.join("\n"));
            }
        };
    }

    #[test]
    fn auth_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(AUTH_SUBCOMMANDS, "auth");
    }
    #[test]
    fn cookies_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(COOKIES_SUBCOMMANDS, "cookies");
    }
    #[test]
    fn network_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(NETWORK_SUBCOMMANDS, "network");
    }
    #[test]
    fn get_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(GET_SUBCOMMANDS, "get");
    }
    #[test]
    fn is_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(IS_SUBCOMMANDS, "is");
    }
    #[test]
    fn find_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(FIND_SUBCOMMANDS, "find");
    }
    #[test]
    fn set_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(SET_SUBCOMMANDS, "set");
    }
    #[test]
    fn mouse_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(MOUSE_SUBCOMMANDS, "mouse");
    }
    #[test]
    fn tab_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(TAB_SUBCOMMANDS, "tab");
    }
    #[test]
    fn keyboard_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(KEYBOARD_SUBCOMMANDS, "keyboard");
    }
    #[test]
    fn diff_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(DIFF_SUBCOMMANDS, "diff");
    }
    #[test]
    fn clipboard_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(CLIPBOARD_SUBCOMMANDS, "clipboard");
    }
    #[test]
    fn scroll_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(SCROLL_SUBCOMMANDS, "scroll");
    }
    #[test]
    fn storage_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(STORAGE_SUBCOMMANDS, "storage");
    }
    #[test]
    fn state_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(STATE_SUBCOMMANDS, "state");
    }
    #[test]
    fn dialog_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(DIALOG_SUBCOMMANDS, "dialog");
    }
    #[test]
    fn session_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(SESSION_SUBCOMMANDS, "session");
    }
    #[test]
    fn window_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(WINDOW_SUBCOMMANDS, "window");
    }
    #[test]
    fn har_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(HAR_SUBCOMMANDS, "har");
    }
    #[test]
    fn trace_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(TRACE_SUBCOMMANDS, "trace");
    }
    #[test]
    fn profiler_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(PROFILER_SUBCOMMANDS, "profiler");
    }
    #[test]
    fn record_subcommands_in_scripts() {
        assert_subcommands_in_scripts!(RECORD_SUBCOMMANDS, "record");
    }
}
