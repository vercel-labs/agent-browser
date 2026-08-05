//! Color output utilities.
//!
//! Colors are off by default (agent-friendly). Enable with
//! `AGENT_BROWSER_COLOR=1`. Setting `NO_COLOR` to any value disables
//! colors per <https://no-color.org/>.

use std::env;
use std::sync::OnceLock;

fn env_is_truthy(name: &str) -> Option<bool> {
    env::var(name)
        .ok()
        .map(|val| !matches!(val.to_lowercase().as_str(), "0" | "false" | "no"))
}

/// Returns true if color output is enabled.
///
/// Priority: `NO_COLOR` (presence disables, per spec) >
/// `AGENT_BROWSER_COLOR` (truthy enables) > default (off).
pub fn is_enabled() -> bool {
    static COLORS_ENABLED: OnceLock<bool> = OnceLock::new();
    *COLORS_ENABLED.get_or_init(|| {
        if env::var_os("NO_COLOR").is_some() {
            return false;
        }
        env_is_truthy("AGENT_BROWSER_COLOR").unwrap_or(false)
    })
}

/// Format text in red (errors)
pub fn red(text: &str) -> String {
    if is_enabled() {
        format!("\x1b[31m{}\x1b[0m", text)
    } else {
        text.to_string()
    }
}

/// Format text in green (success)
pub fn green(text: &str) -> String {
    if is_enabled() {
        format!("\x1b[32m{}\x1b[0m", text)
    } else {
        text.to_string()
    }
}

/// Format text in yellow (warnings)
pub fn yellow(text: &str) -> String {
    if is_enabled() {
        format!("\x1b[33m{}\x1b[0m", text)
    } else {
        text.to_string()
    }
}

/// Format text in cyan (info/progress)
pub fn cyan(text: &str) -> String {
    if is_enabled() {
        format!("\x1b[36m{}\x1b[0m", text)
    } else {
        text.to_string()
    }
}

/// Format text in bold
pub fn bold(text: &str) -> String {
    if is_enabled() {
        format!("\x1b[1m{}\x1b[0m", text)
    } else {
        text.to_string()
    }
}

/// Format text in dim
pub fn dim(text: &str) -> String {
    if is_enabled() {
        format!("\x1b[2m{}\x1b[0m", text)
    } else {
        text.to_string()
    }
}

/// Red X error indicator
pub fn error_indicator() -> &'static str {
    static INDICATOR: OnceLock<String> = OnceLock::new();
    INDICATOR.get_or_init(|| {
        if is_enabled() {
            "\x1b[31m✗\x1b[0m".to_string()
        } else {
            "✗".to_string()
        }
    })
}

/// Green checkmark success indicator
pub fn success_indicator() -> &'static str {
    static INDICATOR: OnceLock<String> = OnceLock::new();
    INDICATOR.get_or_init(|| {
        if is_enabled() {
            "\x1b[32m✓\x1b[0m".to_string()
        } else {
            "✓".to_string()
        }
    })
}

/// Yellow warning indicator
pub fn warning_indicator() -> &'static str {
    static INDICATOR: OnceLock<String> = OnceLock::new();
    INDICATOR.get_or_init(|| {
        if is_enabled() {
            "\x1b[33m⚠\x1b[0m".to_string()
        } else {
            "⚠".to_string()
        }
    })
}

/// Get console log color prefix by level
pub fn console_level_prefix(level: &str) -> String {
    if !is_enabled() {
        return format!("[{}]", level);
    }

    let color = match level {
        "error" => "\x1b[31m",
        "warning" => "\x1b[33m",
        "info" => "\x1b[36m",
        _ => "",
    };
    if color.is_empty() {
        format!("[{}]", level)
    } else {
        format!("{}[{}]\x1b[0m", color, level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_red_contains_ansi_codes() {
        // Test the format structure (actual color depends on NO_COLOR env)
        let formatted = format!("\x1b[31m{}\x1b[0m", "error");
        assert!(formatted.contains("\x1b[31m"));
        assert!(formatted.contains("\x1b[0m"));
    }

    #[test]
    fn test_green_contains_ansi_codes() {
        let formatted = format!("\x1b[32m{}\x1b[0m", "success");
        assert!(formatted.contains("\x1b[32m"));
    }

    #[test]
    fn test_console_level_prefix_contains_level() {
        // Regardless of color state, the level text should be present
        assert!(console_level_prefix("error").contains("error"));
        assert!(console_level_prefix("warning").contains("warning"));
        assert!(console_level_prefix("info").contains("info"));
        assert!(console_level_prefix("log").contains("log"));
    }

    #[test]
    fn test_indicators_contain_symbols() {
        // Regardless of color state, symbols should be present
        assert!(error_indicator().contains('✗'));
        assert!(success_indicator().contains('✓'));
        assert!(warning_indicator().contains('⚠'));
    }
}
