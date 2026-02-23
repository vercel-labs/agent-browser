use crate::color;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_DIR: &str = ".agent-browser";
const CONFIG_FILENAME: &str = "config.json";
const PROJECT_CONFIG_FILENAME: &str = "agent-browser.json";

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub headed: Option<bool>,
    pub json: Option<bool>,
    pub full: Option<bool>,
    pub debug: Option<bool>,
    pub session: Option<String>,
    pub session_name: Option<String>,
    pub executable_path: Option<String>,
    pub extensions: Option<Vec<String>>,
    pub profile: Option<String>,
    pub state: Option<String>,
    pub proxy: Option<String>,
    pub proxy_bypass: Option<String>,
    pub args: Option<String>,
    pub user_agent: Option<String>,
    pub provider: Option<String>,
    pub device: Option<String>,
    pub ignore_https_errors: Option<bool>,
    pub allow_file_access: Option<bool>,
    pub cdp: Option<String>,
    pub auto_connect: Option<bool>,
    pub headers: Option<String>,
    pub annotate: Option<bool>,
    pub color_scheme: Option<String>,
}

impl Config {
    fn merge(self, other: Config) -> Config {
        Config {
            headed: other.headed.or(self.headed),
            json: other.json.or(self.json),
            full: other.full.or(self.full),
            debug: other.debug.or(self.debug),
            session: other.session.or(self.session),
            session_name: other.session_name.or(self.session_name),
            executable_path: other.executable_path.or(self.executable_path),
            extensions: match (self.extensions, other.extensions) {
                (Some(mut a), Some(b)) => {
                    a.extend(b);
                    Some(a)
                }
                (a, b) => b.or(a),
            },
            profile: other.profile.or(self.profile),
            state: other.state.or(self.state),
            proxy: other.proxy.or(self.proxy),
            proxy_bypass: other.proxy_bypass.or(self.proxy_bypass),
            args: other.args.or(self.args),
            user_agent: other.user_agent.or(self.user_agent),
            provider: other.provider.or(self.provider),
            device: other.device.or(self.device),
            ignore_https_errors: other.ignore_https_errors.or(self.ignore_https_errors),
            allow_file_access: other.allow_file_access.or(self.allow_file_access),
            cdp: other.cdp.or(self.cdp),
            auto_connect: other.auto_connect.or(self.auto_connect),
            headers: other.headers.or(self.headers),
            annotate: other.annotate.or(self.annotate),
            color_scheme: other.color_scheme.or(self.color_scheme),
        }
    }
}

fn read_config_file(path: &Path) -> Option<Config> {
    let content = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Config>(&content) {
        Ok(config) => Some(config),
        Err(e) => {
            eprintln!(
                "{} invalid config file {}: {}",
                color::warning_indicator(),
                path.display(),
                e
            );
            None
        }
    }
}

/// Check if a boolean environment variable is set to a truthy value.
/// Returns false when unset, empty, or set to "0", "false", or "no" (case-insensitive).
fn env_var_is_truthy(name: &str) -> bool {
    match env::var(name) {
        Ok(val) => !matches!(val.to_lowercase().as_str(), "0" | "false" | "no" | ""),
        Err(_) => false,
    }
}

/// Parse an optional boolean value after a flag. Returns (value, consumed_next_arg).
/// Recognizes "true" as true, "false" as false. Bare flag defaults to true.
fn parse_bool_arg(args: &[String], i: usize) -> (bool, bool) {
    if let Some(v) = args.get(i + 1) {
        match v.as_str() {
            "true" => (true, true),
            "false" => (false, true),
            _ => (true, false),
        }
    } else {
        (true, false)
    }
}

/// Extract --config <path> from args before full flag parsing.
/// Returns `Some(Some(path))` if --config <path> found, `Some(None)` if --config
/// was the last arg with no value, `None` if --config not present.
fn extract_config_path(args: &[String]) -> Option<Option<String>> {
    const FLAGS_WITH_VALUE: &[&str] = &[
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
        "--session-name",
        "--color-scheme",
    ];
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--config" {
            return Some(args.get(i + 1).cloned());
        }
        if FLAGS_WITH_VALUE.contains(&args[i].as_str()) {
            i += 1;
        }
        i += 1;
    }
    None
}

pub fn load_config(args: &[String]) -> Result<Config, String> {
    let explicit = extract_config_path(args)
        .map(|p| ("--config", p))
        .or_else(|| {
            env::var("AGENT_BROWSER_CONFIG")
                .ok()
                .map(|p| ("AGENT_BROWSER_CONFIG", Some(p)))
        });

    if let Some((source, maybe_path)) = explicit {
        let path_str =
            maybe_path.ok_or_else(|| format!("{} requires a file path", source))?;
        let path = PathBuf::from(&path_str);
        if !path.exists() {
            return Err(format!("config file not found: {}", path_str));
        }
        return read_config_file(&path)
            .ok_or_else(|| format!("failed to load config from {}", path_str));
    }

    let user_config = dirs::home_dir()
        .map(|d| d.join(CONFIG_DIR).join(CONFIG_FILENAME))
        .and_then(|p| read_config_file(&p))
        .unwrap_or_default();

    let project_config = read_config_file(&PathBuf::from(PROJECT_CONFIG_FILENAME));

    Ok(match project_config {
        Some(project) => user_config.merge(project),
        None => user_config,
    })
}

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
    pub auto_connect: bool,
    pub session_name: Option<String>,
    pub annotate: bool,
    pub color_scheme: Option<String>,

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
    pub cli_annotate: bool,
}

pub fn parse_flags(args: &[String]) -> Flags {
    let config = load_config(args).unwrap_or_else(|e| {
        eprintln!("{} {}", color::warning_indicator(), e);
        std::process::exit(1);
    });

    let extensions_env = env::var("AGENT_BROWSER_EXTENSIONS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let extensions = if !extensions_env.is_empty() {
        extensions_env
    } else {
        config.extensions.unwrap_or_default()
    };

    let mut flags = Flags {
        json: env_var_is_truthy("AGENT_BROWSER_JSON")
            || config.json.unwrap_or(false),
        full: env_var_is_truthy("AGENT_BROWSER_FULL")
            || config.full.unwrap_or(false),
        headed: env_var_is_truthy("AGENT_BROWSER_HEADED")
            || config.headed.unwrap_or(false),
        debug: env_var_is_truthy("AGENT_BROWSER_DEBUG")
            || config.debug.unwrap_or(false),
        session: env::var("AGENT_BROWSER_SESSION").ok()
            .or(config.session)
            .unwrap_or_else(|| "default".to_string()),
        headers: config.headers,
        executable_path: env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok()
            .or(config.executable_path),
        cdp: config.cdp,
        extensions,
        profile: env::var("AGENT_BROWSER_PROFILE").ok()
            .or(config.profile),
        state: env::var("AGENT_BROWSER_STATE").ok()
            .or(config.state),
        proxy: env::var("AGENT_BROWSER_PROXY").ok()
            .or(config.proxy),
        proxy_bypass: env::var("AGENT_BROWSER_PROXY_BYPASS").ok()
            .or(config.proxy_bypass),
        args: env::var("AGENT_BROWSER_ARGS").ok()
            .or(config.args),
        user_agent: env::var("AGENT_BROWSER_USER_AGENT").ok()
            .or(config.user_agent),
        provider: env::var("AGENT_BROWSER_PROVIDER").ok()
            .or(config.provider),
        ignore_https_errors: env_var_is_truthy("AGENT_BROWSER_IGNORE_HTTPS_ERRORS")
            || config.ignore_https_errors.unwrap_or(false),
        allow_file_access: env_var_is_truthy("AGENT_BROWSER_ALLOW_FILE_ACCESS")
            || config.allow_file_access.unwrap_or(false),
        device: env::var("AGENT_BROWSER_IOS_DEVICE").ok()
            .or(config.device),
        auto_connect: env_var_is_truthy("AGENT_BROWSER_AUTO_CONNECT")
            || config.auto_connect.unwrap_or(false),
        session_name: env::var("AGENT_BROWSER_SESSION_NAME").ok()
            .or(config.session_name),
        annotate: env_var_is_truthy("AGENT_BROWSER_ANNOTATE")
            || config.annotate.unwrap_or(false),
        color_scheme: env::var("AGENT_BROWSER_COLOR_SCHEME").ok()
            .or(config.color_scheme),
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
    };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                let (val, consumed) = parse_bool_arg(args, i);
                flags.json = val;
                if consumed { i += 1; }
            }
            "--full" | "-f" => {
                let (val, consumed) = parse_bool_arg(args, i);
                flags.full = val;
                if consumed { i += 1; }
            }
            "--headed" => {
                let (val, consumed) = parse_bool_arg(args, i);
                flags.headed = val;
                if consumed { i += 1; }
            }
            "--debug" => {
                let (val, consumed) = parse_bool_arg(args, i);
                flags.debug = val;
                if consumed { i += 1; }
            }
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
            "--ignore-https-errors" => {
                let (val, consumed) = parse_bool_arg(args, i);
                flags.ignore_https_errors = val;
                if consumed { i += 1; }
            }
            "--allow-file-access" => {
                let (val, consumed) = parse_bool_arg(args, i);
                flags.allow_file_access = val;
                flags.cli_allow_file_access = true;
                if consumed { i += 1; }
            }
            "--device" => {
                if let Some(d) = args.get(i + 1) {
                    flags.device = Some(d.clone());
                    i += 1;
                }
            }
            "--auto-connect" => {
                let (val, consumed) = parse_bool_arg(args, i);
                flags.auto_connect = val;
                if consumed { i += 1; }
            }
            "--session-name" => {
                if let Some(s) = args.get(i + 1) {
                    flags.session_name = Some(s.clone());
                    i += 1;
                }
            }
            "--annotate" => {
                let (val, consumed) = parse_bool_arg(args, i);
                flags.annotate = val;
                flags.cli_annotate = true;
                if consumed { i += 1; }
            }
            "--color-scheme" => {
                if let Some(s) = args.get(i + 1) {
                    flags.color_scheme = Some(s.clone());
                    i += 1;
                }
            }
            "--config" => {
                // Already handled by load_config(); skip the value
                i += 1;
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

    // Boolean flags that optionally take true/false
    const GLOBAL_BOOL_FLAGS: &[&str] = &[
        "--json",
        "--full",
        "--headed",
        "--debug",
        "--ignore-https-errors",
        "--allow-file-access",
        "--auto-connect",
        "--annotate",
    ];
    // Global flags that always take a value (need to skip the next arg too)
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
        "--session-name",
        "--color-scheme",
        "--config",
    ];

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if skip_next {
            skip_next = false;
            i += 1;
            continue;
        }
        if GLOBAL_FLAGS_WITH_VALUE.contains(&arg.as_str()) {
            skip_next = true;
            i += 1;
            continue;
        }
        if GLOBAL_BOOL_FLAGS.contains(&arg.as_str()) || arg == "-f" {
            if let Some(v) = args.get(i + 1) {
                if matches!(v.as_str(), "true" | "false") {
                    i += 1;
                }
            }
            i += 1;
            continue;
        }
        result.push(arg.clone());
        i += 1;
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
    fn test_cli_annotate_tracking() {
        let flags = parse_flags(&args("--annotate screenshot"));
        assert!(flags.cli_annotate);
        assert!(flags.annotate);
    }

    #[test]
    fn test_cli_annotate_not_set_without_flag() {
        let flags = parse_flags(&args("screenshot"));
        assert!(!flags.cli_annotate);
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

    // === Config file tests ===

    #[test]
    fn test_config_deserialize_full() {
        let json = r#"{
            "headed": true,
            "json": true,
            "full": true,
            "debug": true,
            "session": "test-session",
            "sessionName": "my-app",
            "executablePath": "/usr/bin/chromium",
            "extensions": ["/ext1", "/ext2"],
            "profile": "/tmp/profile",
            "state": "/tmp/state.json",
            "proxy": "http://proxy:8080",
            "proxyBypass": "localhost",
            "args": "--no-sandbox",
            "userAgent": "test-agent",
            "provider": "ios",
            "device": "iPhone 15",
            "ignoreHttpsErrors": true,
            "allowFileAccess": true,
            "cdp": "9222",
            "autoConnect": true,
            "headers": "{\"Auth\":\"token\"}"
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.headed, Some(true));
        assert_eq!(config.json, Some(true));
        assert_eq!(config.full, Some(true));
        assert_eq!(config.debug, Some(true));
        assert_eq!(config.session.as_deref(), Some("test-session"));
        assert_eq!(config.session_name.as_deref(), Some("my-app"));
        assert_eq!(config.executable_path.as_deref(), Some("/usr/bin/chromium"));
        assert_eq!(config.extensions, Some(vec!["/ext1".to_string(), "/ext2".to_string()]));
        assert_eq!(config.profile.as_deref(), Some("/tmp/profile"));
        assert_eq!(config.state.as_deref(), Some("/tmp/state.json"));
        assert_eq!(config.proxy.as_deref(), Some("http://proxy:8080"));
        assert_eq!(config.proxy_bypass.as_deref(), Some("localhost"));
        assert_eq!(config.args.as_deref(), Some("--no-sandbox"));
        assert_eq!(config.user_agent.as_deref(), Some("test-agent"));
        assert_eq!(config.provider.as_deref(), Some("ios"));
        assert_eq!(config.device.as_deref(), Some("iPhone 15"));
        assert_eq!(config.ignore_https_errors, Some(true));
        assert_eq!(config.allow_file_access, Some(true));
        assert_eq!(config.cdp.as_deref(), Some("9222"));
        assert_eq!(config.auto_connect, Some(true));
        assert_eq!(config.headers.as_deref(), Some("{\"Auth\":\"token\"}"));
    }

    #[test]
    fn test_config_deserialize_partial() {
        let json = r#"{"headed": true, "proxy": "http://localhost:8080"}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.headed, Some(true));
        assert_eq!(config.proxy.as_deref(), Some("http://localhost:8080"));
        assert_eq!(config.session, None);
        assert_eq!(config.extensions, None);
        assert_eq!(config.debug, None);
    }

    #[test]
    fn test_config_deserialize_empty() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config.headed, None);
        assert_eq!(config.session, None);
        assert_eq!(config.proxy, None);
    }

    #[test]
    fn test_config_ignores_unknown_keys() {
        let json = r#"{"headed": true, "unknownFutureKey": "value", "anotherOne": 42}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.headed, Some(true));
    }

    #[test]
    fn test_config_merge_project_overrides_user() {
        let user = Config {
            headed: Some(true),
            proxy: Some("http://user-proxy:8080".to_string()),
            profile: Some("/user/profile".to_string()),
            ..Config::default()
        };
        let project = Config {
            proxy: Some("http://project-proxy:9090".to_string()),
            debug: Some(true),
            ..Config::default()
        };
        let merged = user.merge(project);
        assert_eq!(merged.headed, Some(true)); // kept from user
        assert_eq!(merged.proxy.as_deref(), Some("http://project-proxy:9090")); // overridden by project
        assert_eq!(merged.profile.as_deref(), Some("/user/profile")); // kept from user
        assert_eq!(merged.debug, Some(true)); // added by project
    }

    #[test]
    fn test_config_merge_none_does_not_override() {
        let user = Config {
            headed: Some(true),
            proxy: Some("http://proxy:8080".to_string()),
            ..Config::default()
        };
        let project = Config::default();
        let merged = user.merge(project);
        assert_eq!(merged.headed, Some(true));
        assert_eq!(merged.proxy.as_deref(), Some("http://proxy:8080"));
    }

    #[test]
    fn test_load_config_from_file() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("ab-test-config");
        let _ = fs::create_dir_all(&dir);
        let config_path = dir.join("test-config.json");
        let mut f = fs::File::create(&config_path).unwrap();
        writeln!(f, r#"{{"headed": true, "proxy": "http://test:1234"}}"#).unwrap();

        let config = read_config_file(&config_path).unwrap();
        assert_eq!(config.headed, Some(true));
        assert_eq!(config.proxy.as_deref(), Some("http://test:1234"));

        let _ = fs::remove_file(&config_path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn test_load_config_missing_file_returns_none() {
        let result = read_config_file(&PathBuf::from("/nonexistent/agent-browser.json"));
        assert!(result.is_none());
    }

    #[test]
    fn test_load_config_malformed_json_returns_none() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("ab-test-malformed");
        let _ = fs::create_dir_all(&dir);
        let config_path = dir.join("bad-config.json");
        let mut f = fs::File::create(&config_path).unwrap();
        writeln!(f, "{{not valid json}}").unwrap();

        let result = read_config_file(&config_path);
        assert!(result.is_none());

        let _ = fs::remove_file(&config_path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn test_extract_config_path() {
        assert_eq!(
            extract_config_path(&args("--config ./my-config.json open example.com")),
            Some(Some("./my-config.json".to_string()))
        );
    }

    #[test]
    fn test_extract_config_path_missing() {
        assert_eq!(extract_config_path(&args("open example.com")), None);
    }

    #[test]
    fn test_extract_config_path_no_value() {
        assert_eq!(extract_config_path(&args("--config")), Some(None));
    }

    #[test]
    fn test_extract_config_path_skips_flag_values() {
        assert_eq!(extract_config_path(&args("--args --config open")), None);
    }

    #[test]
    fn test_clean_args_removes_config() {
        let cleaned = clean_args(&args("--config ./config.json open example.com"));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    #[test]
    fn test_load_config_with_config_flag() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("ab-test-flag-config");
        let _ = fs::create_dir_all(&dir);
        let config_path = dir.join("custom.json");
        let mut f = fs::File::create(&config_path).unwrap();
        writeln!(f, r#"{{"headed": true, "session": "custom"}}"#).unwrap();

        let flag_args = vec![
            "--config".to_string(),
            config_path.to_string_lossy().to_string(),
            "open".to_string(),
            "example.com".to_string(),
        ];
        let config = load_config(&flag_args).unwrap();
        assert_eq!(config.headed, Some(true));
        assert_eq!(config.session.as_deref(), Some("custom"));

        let _ = fs::remove_file(&config_path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn test_load_config_error_missing_config_value() {
        let result = load_config(&args("--config"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires a file path"));
    }

    #[test]
    fn test_load_config_error_nonexistent_file() {
        let result = load_config(&args("--config /nonexistent/config.json open"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("config file not found"));
    }

    #[test]
    fn test_load_config_error_malformed_explicit() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("ab-test-explicit-malformed");
        let _ = fs::create_dir_all(&dir);
        let config_path = dir.join("bad.json");
        let mut f = fs::File::create(&config_path).unwrap();
        writeln!(f, "{{not valid}}").unwrap();

        let flag_args = vec![
            "--config".to_string(),
            config_path.to_string_lossy().to_string(),
        ];
        let result = load_config(&flag_args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to load config"));

        let _ = fs::remove_file(&config_path);
        let _ = fs::remove_dir(&dir);
    }

    // === Boolean flag value tests ===

    #[test]
    fn test_headed_false() {
        let flags = parse_flags(&args("--headed false open example.com"));
        assert!(!flags.headed);
    }

    #[test]
    fn test_headed_true_explicit() {
        let flags = parse_flags(&args("--headed true open example.com"));
        assert!(flags.headed);
    }

    #[test]
    fn test_headed_bare_defaults_true() {
        let flags = parse_flags(&args("--headed open example.com"));
        assert!(flags.headed);
    }

    #[test]
    fn test_debug_false() {
        let flags = parse_flags(&args("--debug false open example.com"));
        assert!(!flags.debug);
    }

    #[test]
    fn test_json_false() {
        let flags = parse_flags(&args("--json false open example.com"));
        assert!(!flags.json);
    }

    #[test]
    fn test_ignore_https_errors_false() {
        let flags = parse_flags(&args("--ignore-https-errors false open"));
        assert!(!flags.ignore_https_errors);
    }

    #[test]
    fn test_allow_file_access_false() {
        let flags = parse_flags(&args("--allow-file-access false open"));
        assert!(!flags.allow_file_access);
        assert!(flags.cli_allow_file_access);
    }

    #[test]
    fn test_auto_connect_false() {
        let flags = parse_flags(&args("--auto-connect false open"));
        assert!(!flags.auto_connect);
    }

    #[test]
    fn test_full_bare_defaults_true() {
        let flags = parse_flags(&args("--full open example.com"));
        assert!(flags.full);
    }

    #[test]
    fn test_full_false() {
        let flags = parse_flags(&args("--full false open example.com"));
        assert!(!flags.full);
    }

    #[test]
    fn test_full_short_flag() {
        let flags = parse_flags(&args("-f open example.com"));
        assert!(flags.full);
    }

    #[test]
    fn test_clean_args_removes_full_with_value() {
        let cleaned = clean_args(&args("--full false open example.com"));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    #[test]
    fn test_clean_args_removes_short_full() {
        let cleaned = clean_args(&args("-f open example.com"));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    #[test]
    fn test_clean_args_removes_bool_flag_with_value() {
        let cleaned = clean_args(&args("--headed false --debug true open example.com"));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    #[test]
    fn test_clean_args_removes_bare_bool_flag() {
        let cleaned = clean_args(&args("--headed --debug open example.com"));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    // === Extensions merge tests ===

    #[test]
    fn test_config_merge_extensions_concatenated() {
        let user = Config {
            extensions: Some(vec!["/ext1".to_string()]),
            ..Config::default()
        };
        let project = Config {
            extensions: Some(vec!["/ext2".to_string(), "/ext3".to_string()]),
            ..Config::default()
        };
        let merged = user.merge(project);
        assert_eq!(
            merged.extensions,
            Some(vec!["/ext1".to_string(), "/ext2".to_string(), "/ext3".to_string()])
        );
    }

    #[test]
    fn test_config_merge_extensions_user_only() {
        let user = Config {
            extensions: Some(vec!["/ext1".to_string()]),
            ..Config::default()
        };
        let project = Config::default();
        let merged = user.merge(project);
        assert_eq!(merged.extensions, Some(vec!["/ext1".to_string()]));
    }

    #[test]
    fn test_config_merge_extensions_project_only() {
        let user = Config::default();
        let project = Config {
            extensions: Some(vec!["/ext2".to_string()]),
            ..Config::default()
        };
        let merged = user.merge(project);
        assert_eq!(merged.extensions, Some(vec!["/ext2".to_string()]));
    }
}
