use std::env;

pub struct Flags {
    pub json: bool,
    pub full: bool,
    pub headed: bool,
    pub debug: bool,
    pub session: String,
    pub headers: Option<String>,
    pub executable_path: Option<String>,
    pub cdp: Option<String>,
    pub extensions: Vec<String>,
    pub profile: Option<String>,
    pub state: Option<String>,
    pub proxy: Option<String>,
    pub proxy_bypass: Option<String>,
    pub args: Option<String>,
    pub user_agent: Option<String>,
    pub provider: Option<String>,
    pub ignore_https_errors: bool,
    pub allow_file_access: bool,
    pub device: Option<String>,

    // Track which launch-time options were explicitly passed via CLI
    // (as opposed to being set only via environment variables)
    pub cli_executable_path: bool,
    pub cli_extensions: bool,
    pub cli_profile: bool,
    pub cli_state: bool,
    pub cli_args: bool,
    pub cli_user_agent: bool,
    pub cli_proxy: bool,
    pub cli_proxy_bypass: bool,
    pub cli_allow_file_access: bool,
}

pub fn parse_flags(args: &[String]) -> Flags {
    let extensions_env = env::var("AGENT_BROWSER_EXTENSIONS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut flags = Flags {
        json: false,
        full: false,
        headed: false,
        debug: false,
        session: env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string()),
        headers: None,
        executable_path: env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok(),
        cdp: None,
        extensions: extensions_env,
        profile: env::var("AGENT_BROWSER_PROFILE").ok(),
        state: env::var("AGENT_BROWSER_STATE").ok(),
        proxy: env::var("AGENT_BROWSER_PROXY").ok(),
        proxy_bypass: env::var("AGENT_BROWSER_PROXY_BYPASS").ok(),
        args: env::var("AGENT_BROWSER_ARGS").ok(),
        user_agent: env::var("AGENT_BROWSER_USER_AGENT").ok(),
        provider: env::var("AGENT_BROWSER_PROVIDER").ok(),
        ignore_https_errors: false,
        allow_file_access: env::var("AGENT_BROWSER_ALLOW_FILE_ACCESS").is_ok(),
        device: env::var("AGENT_BROWSER_IOS_DEVICE").ok(),
        // Track CLI-passed flags (default false, set to true when flag is passed)
        cli_executable_path: false,
        cli_extensions: false,
        cli_profile: false,
        cli_state: false,
        cli_args: false,
        cli_user_agent: false,
        cli_proxy: false,
        cli_proxy_bypass: false,
        cli_allow_file_access: false,
    };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => flags.json = true,
            "--full" | "-f" => flags.full = true,
            "--headed" => flags.headed = true,
            "--debug" => flags.debug = true,
            "--session" => {
                if let Some(s) = args.get(i + 1) {
                    flags.session = s.clone();
                    i += 1;
                }
            }
            "--headers" => {
                if let Some(h) = args.get(i + 1) {
                    flags.headers = Some(h.clone());
                    i += 1;
                }
            }
            "--executable-path" => {
                if let Some(s) = args.get(i + 1) {
                    flags.executable_path = Some(s.clone());
                    flags.cli_executable_path = true;
                    i += 1;
                }
            }
            "--extension" => {
                if let Some(s) = args.get(i + 1) {
                    flags.extensions.push(s.clone());
                    flags.cli_extensions = true;
                    i += 1;
                }
            }
            "--cdp" => {
                if let Some(s) = args.get(i + 1) {
                    flags.cdp = Some(s.clone());
                    i += 1;
                }
            }
            "--profile" => {
                if let Some(s) = args.get(i + 1) {
                    flags.profile = Some(s.clone());
                    flags.cli_profile = true;
                    i += 1;
                }
            }
            "--state" => {
                if let Some(s) = args.get(i + 1) {
                    flags.state = Some(s.clone());
                    flags.cli_state = true;
                    i += 1;
                }
            }
            "--proxy" => {
                if let Some(p) = args.get(i + 1) {
                    flags.proxy = Some(p.clone());
                    flags.cli_proxy = true;
                    i += 1;
                }
            }
            "--proxy-bypass" => {
                if let Some(s) = args.get(i + 1) {
                    flags.proxy_bypass = Some(s.clone());
                    flags.cli_proxy_bypass = true;
                    i += 1;
                }
            }
            "--args" => {
                if let Some(s) = args.get(i + 1) {
                    flags.args = Some(s.clone());
                    flags.cli_args = true;
                    i += 1;
                }
            }
            "--user-agent" => {
                if let Some(s) = args.get(i + 1) {
                    flags.user_agent = Some(s.clone());
                    flags.cli_user_agent = true;
                    i += 1;
                }
            }
            "-p" | "--provider" => {
                if let Some(p) = args.get(i + 1) {
                    flags.provider = Some(p.clone());
                    i += 1;
                }
            }
            "--ignore-https-errors" => flags.ignore_https_errors = true,
            "--allow-file-access" => {
                flags.allow_file_access = true;
                flags.cli_allow_file_access = true;
            }
            "--device" => {
                if let Some(d) = args.get(i + 1) {
                    flags.device = Some(d.clone());
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    flags
}

pub fn clean_args(args: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut skip_next = false;

    // Global flags that should be stripped from command args
    const GLOBAL_FLAGS: &[&str] = &[
        "--json",
        "--full",
        "--headed",
        "--debug",
        "--ignore-https-errors",
        "--allow-file-access",
    ];
    // Global flags that take a value (need to skip the next arg too)
    const GLOBAL_FLAGS_WITH_VALUE: &[&str] = &[
        "--session",
        "--headers",
        "--executable-path",
        "--cdp",
        "--extension",
        "--profile",
        "--state",
        "--proxy",
        "--proxy-bypass",
        "--args",
        "--user-agent",
        "-p",
        "--provider",
        "--device",
    ];

    for arg in args.iter() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if GLOBAL_FLAGS_WITH_VALUE.contains(&arg.as_str()) {
            skip_next = true;
            continue;
        }
        // Only strip known global flags, not command-specific flags
        if GLOBAL_FLAGS.contains(&arg.as_str()) || arg == "-f" {
            continue;
        }
        result.push(arg.clone());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn test_parse_headers_flag() {
        let flags = parse_flags(&args(r#"open example.com --headers {"Auth":"token"}"#));
        assert_eq!(flags.headers, Some(r#"{"Auth":"token"}"#.to_string()));
    }

    #[test]
    fn test_parse_headers_flag_with_spaces() {
        // Headers JSON is passed as a single quoted argument in shell
        let input: Vec<String> = vec![
            "open".to_string(),
            "example.com".to_string(),
            "--headers".to_string(),
            r#"{"Authorization": "Bearer token"}"#.to_string(),
        ];
        let flags = parse_flags(&input);
        assert_eq!(
            flags.headers,
            Some(r#"{"Authorization": "Bearer token"}"#.to_string())
        );
    }

    #[test]
    fn test_parse_no_headers_flag() {
        let flags = parse_flags(&args("open example.com"));
        assert!(flags.headers.is_none());
    }

    #[test]
    fn test_clean_args_removes_headers() {
        let input: Vec<String> = vec![
            "open".to_string(),
            "example.com".to_string(),
            "--headers".to_string(),
            r#"{"Auth":"token"}"#.to_string(),
        ];
        let clean = clean_args(&input);
        assert_eq!(clean, vec!["open", "example.com"]);
    }

    #[test]
    fn test_clean_args_removes_headers_at_start() {
        let input: Vec<String> = vec![
            "--headers".to_string(),
            r#"{"Auth":"token"}"#.to_string(),
            "open".to_string(),
            "example.com".to_string(),
        ];
        let clean = clean_args(&input);
        assert_eq!(clean, vec!["open", "example.com"]);
    }

    #[test]
    fn test_headers_with_other_flags() {
        let input: Vec<String> = vec![
            "open".to_string(),
            "example.com".to_string(),
            "--headers".to_string(),
            r#"{"Auth":"token"}"#.to_string(),
            "--json".to_string(),
            "--headed".to_string(),
        ];
        let flags = parse_flags(&input);
        assert_eq!(flags.headers, Some(r#"{"Auth":"token"}"#.to_string()));
        assert!(flags.json);
        assert!(flags.headed);

        let clean = clean_args(&input);
        assert_eq!(clean, vec!["open", "example.com"]);
    }

    #[test]
    fn test_parse_executable_path_flag() {
        let flags = parse_flags(&args(
            "--executable-path /path/to/chromium open example.com",
        ));
        assert_eq!(flags.executable_path, Some("/path/to/chromium".to_string()));
    }

    #[test]
    fn test_parse_executable_path_flag_no_value() {
        let flags = parse_flags(&args("--executable-path"));
        assert_eq!(flags.executable_path, None);
    }

    #[test]
    fn test_clean_args_removes_executable_path() {
        let cleaned = clean_args(&args(
            "--executable-path /path/to/chromium open example.com",
        ));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    #[test]
    fn test_clean_args_removes_executable_path_with_other_flags() {
        let cleaned = clean_args(&args(
            "--json --executable-path /path/to/chromium --headed open example.com",
        ));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    #[test]
    fn test_parse_flags_with_session_and_executable_path() {
        let flags = parse_flags(&args(
            "--session test --executable-path /custom/chrome open example.com",
        ));
        assert_eq!(flags.session, "test");
        assert_eq!(flags.executable_path, Some("/custom/chrome".to_string()));
    }

    #[test]
    fn test_cli_executable_path_tracking() {
        // When --executable-path is passed via CLI, cli_executable_path should be true
        let flags = parse_flags(&args("--executable-path /path/to/chrome snapshot"));
        assert!(flags.cli_executable_path);
        assert_eq!(flags.executable_path, Some("/path/to/chrome".to_string()));
    }

    #[test]
    fn test_cli_executable_path_not_set_without_flag() {
        // When no --executable-path is passed, cli_executable_path should be false
        // (even if env var sets executable_path to Some value, which we can't test here)
        let flags = parse_flags(&args("snapshot"));
        assert!(!flags.cli_executable_path);
    }

    #[test]
    fn test_cli_extension_tracking() {
        let flags = parse_flags(&args("--extension /path/to/ext snapshot"));
        assert!(flags.cli_extensions);
    }

    #[test]
    fn test_cli_profile_tracking() {
        let flags = parse_flags(&args("--profile /path/to/profile snapshot"));
        assert!(flags.cli_profile);
    }

    #[test]
    fn test_cli_multiple_flags_tracking() {
        let flags = parse_flags(&args(
            "--executable-path /chrome --profile /profile --proxy http://proxy snapshot",
        ));
        assert!(flags.cli_executable_path);
        assert!(flags.cli_profile);
        assert!(flags.cli_proxy);
        assert!(!flags.cli_extensions);
        assert!(!flags.cli_state);
    }
}
