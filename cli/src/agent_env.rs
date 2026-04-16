use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

const DEFAULT_ENV_FILE: &str = ".agent-browser/.env";
const KNOWN_VARS: &[&str] = &[
    "AGENT_BROWSER_KEYCHAIN_PASSWORD",
    "AGENT_BROWSER_USE_REAL_KEYCHAIN",
];

fn resolve_default_env_file() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(DEFAULT_ENV_FILE))
}

fn parse_dotenv_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut value = trimmed;
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        value = &value[1..value.len() - 1];
        return value
            .replace("\\n", "\n")
            .replace("\\r", "\r")
            .replace("\\t", "\t")
            .replace("\\\"", "\"")
            .replace("\\'", "'")
            .replace("\\\\", "\\");
    }

    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        return value[1..value.len() - 1].to_string();
    }

    value.to_string()
}

fn parse_dotenv(contents: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();

    for line in contents.lines() {
        let mut line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim();
        }

        let mut comment_index = None;
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut escaped = false;
        for (idx, ch) in line.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' && !in_single_quote {
                in_double_quote = !in_double_quote;
                continue;
            }
            if ch == '\'' && !in_double_quote {
                in_single_quote = !in_single_quote;
                continue;
            }
            if ch == '#' && !in_single_quote && !in_double_quote {
                comment_index = Some(idx);
                break;
            }
        }

        if let Some(idx) = comment_index {
            line = line[..idx].trim();
        }

        if line.is_empty() {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        if key.is_empty() {
            continue;
        }

        if !KNOWN_VARS.contains(&key) {
            continue;
        }

        values.insert(key.to_string(), parse_dotenv_value(value.trim()));
    }

    values
}

fn should_set_var(name: &str) -> bool {
    if env::var(name).is_ok() {
        return false;
    }
    KNOWN_VARS.contains(&name)
}

pub fn load_env_file() -> Result<(), String> {
    let path = env::var("AGENT_BROWSER_ENV_FILE")
        .map(PathBuf::from)
        .ok()
        .or_else(resolve_default_env_file);

    let Some(path) = path else {
        return Ok(());
    };

    if !path.exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read env file {}: {}", path.display(), e))?;
    let values = parse_dotenv(&contents);

    for (key, value) in values {
        if should_set_var(&key) {
            // SAFETY: This is deliberate runtime config bootstrapping for local agent
            // runs. Missing values are a non-fatal no-op.
            env::set_var(&key, value);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dotenv_quotes_and_comments() {
        let content = r#"
export AGENT_BROWSER_KEYCHAIN_PASSWORD="line1\nline2"
AGENT_BROWSER_USE_REAL_KEYCHAIN=1 # force real keychain
# AGENT_BROWSER_KEYCHAIN_PASSWORD=ignored
AGENT_BROWSER_KEYCHAIN_PASSWORD='quoted'
AGENT_BROWSER_UNKNOWN=ignored
"#;

        let parsed = parse_dotenv(content);
        assert_eq!(
            parsed.get("AGENT_BROWSER_KEYCHAIN_PASSWORD"),
            Some(&"quoted".to_string())
        );
        assert_eq!(
            parsed.get("AGENT_BROWSER_USE_REAL_KEYCHAIN"),
            Some(&"1".to_string())
        );
        assert!(!parsed.contains_key("AGENT_BROWSER_UNKNOWN"));
    }

    #[test]
    fn test_should_set_var() {
        env::remove_var("AGENT_BROWSER_KEYCHAIN_PASSWORD");
        assert!(should_set_var("AGENT_BROWSER_KEYCHAIN_PASSWORD"));
        env::set_var("AGENT_BROWSER_KEYCHAIN_PASSWORD", "set");
        assert!(!should_set_var("AGENT_BROWSER_KEYCHAIN_PASSWORD"));
        env::remove_var("AGENT_BROWSER_KEYCHAIN_PASSWORD");
    }
}
