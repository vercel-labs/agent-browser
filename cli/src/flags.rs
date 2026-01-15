use std::env;

use crate::validation::is_valid_session_name;

pub struct Flags {
    pub json: bool,
    pub full: bool,
    pub headed: bool,
    pub debug: bool,
    pub session: String,
    pub headers: Option<String>,
    pub executable_path: Option<String>,
    pub cdp: Option<String>,
    /// Session persistence name (for auto-save/load of cookies and storage)
    pub session_name: Option<String>,
}

/// Result of flag parsing, which may include validation errors
pub struct ParsedFlags {
    pub flags: Flags,
    pub errors: Vec<String>,
}

pub fn parse_flags(args: &[String]) -> ParsedFlags {
    let mut errors: Vec<String> = Vec::new();
    
    // Validate session_name from environment if present
    let env_session_name = env::var("AGENT_BROWSER_SESSION_NAME").ok();
    let validated_env_session_name = if let Some(ref name) = env_session_name {
        if is_valid_session_name(name) {
            Some(name.clone())
        } else {
            errors.push(format!(
                "Invalid AGENT_BROWSER_SESSION_NAME '{}'. Only alphanumeric characters, hyphens, and underscores are allowed.",
                name
            ));
            None
        }
    } else {
        None
    };

    let mut flags = Flags {
        json: false,
        full: false,
        headed: false,
        debug: false,
        session: env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string()),
        headers: None,
        executable_path: env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok(),
        cdp: None,
        session_name: validated_env_session_name,
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
                    i += 1;
                }
            },
            "--extension" => {
                if let Some(s) = args.get(i + 1) {
                    flags.extensions.push(s.clone());
                    i += 1;
                }
            },
            "--cdp" => {
                if let Some(s) = args.get(i + 1) {
                    flags.cdp = Some(s.clone());
                    i += 1;
                }
            }
            "--session-name" => {
                if let Some(s) = args.get(i + 1) {
                    if is_valid_session_name(s) {
                        flags.session_name = Some(s.clone());
                    } else {
                        errors.push(format!(
                            "Invalid session name '{}'. Only alphanumeric characters, hyphens, and underscores are allowed.",
                            s
                        ));
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    ParsedFlags { flags, errors }
}

pub fn clean_args(args: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut skip_next = false;

    // Global flags that should be stripped from command args
    const GLOBAL_FLAGS: &[&str] = &["--json", "--full", "--headed", "--debug"];
    // Global flags that take a value (need to skip the next arg too)
    const GLOBAL_FLAGS_WITH_VALUE: &[&str] = &["--session", "--headers", "--executable-path", "--cdp", "--session-name"];

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
        let parsed = parse_flags(&args(r#"open example.com --headers {"Auth":"token"}"#));
        assert_eq!(parsed.flags.headers, Some(r#"{"Auth":"token"}"#.to_string()));
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
        let parsed = parse_flags(&input);
        assert_eq!(parsed.flags.headers, Some(r#"{"Authorization": "Bearer token"}"#.to_string()));
    }

    #[test]
    fn test_parse_no_headers_flag() {
        let parsed = parse_flags(&args("open example.com"));
        assert!(parsed.flags.headers.is_none());
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
        let parsed = parse_flags(&input);
        assert_eq!(parsed.flags.headers, Some(r#"{"Auth":"token"}"#.to_string()));
        assert!(parsed.flags.json);
        assert!(parsed.flags.headed);
        
        let clean = clean_args(&input);
        assert_eq!(clean, vec!["open", "example.com"]);
    }

    #[test]
    fn test_parse_executable_path_flag() {
        let parsed = parse_flags(&args("--executable-path /path/to/chromium open example.com"));
        assert_eq!(parsed.flags.executable_path, Some("/path/to/chromium".to_string()));
    }

    #[test]
    fn test_parse_executable_path_flag_no_value() {
        let parsed = parse_flags(&args("--executable-path"));
        assert_eq!(parsed.flags.executable_path, None);
    }

    #[test]
    fn test_clean_args_removes_executable_path() {
        let cleaned = clean_args(&args("--executable-path /path/to/chromium open example.com"));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    #[test]
    fn test_clean_args_removes_executable_path_with_other_flags() {
        let cleaned = clean_args(&args("--json --executable-path /path/to/chromium --headed open example.com"));
        assert_eq!(cleaned, vec!["open", "example.com"]);
    }

    #[test]
    fn test_parse_flags_with_session_and_executable_path() {
        let parsed = parse_flags(&args("--session test --executable-path /custom/chrome open example.com"));
        assert_eq!(parsed.flags.session, "test");
        assert_eq!(parsed.flags.executable_path, Some("/custom/chrome".to_string()));
    }

    #[test]
    fn test_invalid_session_name_rejected() {
        let parsed = parse_flags(&args("--session-name ../bad open example.com"));
        assert!(!parsed.errors.is_empty());
        assert!(parsed.flags.session_name.is_none());
    }

    #[test]
    fn test_valid_session_name_accepted() {
        let parsed = parse_flags(&args("--session-name my-project open example.com"));
        assert!(parsed.errors.is_empty());
        assert_eq!(parsed.flags.session_name, Some("my-project".to_string()));
    }
}
