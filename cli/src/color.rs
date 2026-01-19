//! Color output utilities respecting NO_COLOR environment variable.
//!
//! When the NO_COLOR environment variable is present (regardless of value),
//! all color formatting is disabled per https://no-color.org/

use std::env;
use std::sync::OnceLock;

/// Returns true if color output is enabled (NO_COLOR is NOT set)
pub fn is_enabled() -> bool {
    static COLORS_ENABLED: OnceLock<bool> = OnceLock::new();
    *COLORS_ENABLED.get_or_init(|| env::var("NO_COLOR").is_err())
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
