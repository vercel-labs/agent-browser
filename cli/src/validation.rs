/// Check if a session name is valid (alphanumeric, hyphens, and underscores only)
pub fn is_valid_session_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Convert arbitrary caller-provided text into a valid session-name component.
pub fn sanitize_session_component(value: &str) -> String {
    let mut out = String::new();
    let mut last_was_sep = false;

    for c in value.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase().filter(|c| c.is_alphanumeric()));
            last_was_sep = false;
        } else if c == '-' || c == '_' {
            if !out.is_empty() && !last_was_sep {
                out.push(c);
                last_was_sep = true;
            }
        } else if !out.is_empty() && !last_was_sep {
            out.push('-');
            last_was_sep = true;
        }
    }

    while out.ends_with(['-', '_']) {
        out.pop();
    }

    out
}

/// Generate error message for invalid session name
pub fn session_name_error(name: &str) -> String {
    format!(
        "Invalid session name '{}'. Only alphanumeric characters, hyphens, and underscores are allowed.",
        name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_validation_accepts_supported_names() {
        for name in ["default", "next-loop", "next_loop", "café-東京_7"] {
            assert!(is_valid_session_name(name), "{name}");
        }
    }

    #[test]
    fn session_name_validation_rejects_empty_and_spaces() {
        for name in ["", "bad name"] {
            assert!(!is_valid_session_name(name), "{name}");
        }
    }

    #[test]
    fn sanitize_session_component_produces_valid_component() {
        let value = sanitize_session_component("Next Dev Loop: /Users/me/worktree!");

        assert_eq!(value, "next-dev-loop-users-me-worktree");
        assert!(is_valid_session_name(&value));
    }

    #[test]
    fn sanitize_session_component_trims_separators() {
        assert_eq!(sanitize_session_component(" --Agent__ "), "agent");
    }

    #[test]
    fn sanitize_session_component_accepts_dotted_capital_i() {
        for (input, expected) in [
            ("İstanbul", "istanbul"),
            (" --İSTANBUL__東京 ", "istanbul_東京"),
            ("Ångström-東京_7", "ångström-東京_7"),
        ] {
            let component = sanitize_session_component(input);
            assert_eq!(component, expected);
            assert!(is_valid_session_name(&component), "{input}: {component}");
        }
    }

    #[test]
    fn sanitized_alphanumeric_characters_remain_valid_session_components() {
        for code in 0..=char::MAX as u32 {
            let Some(c) = char::from_u32(code).filter(|c| c.is_alphanumeric()) else {
                continue;
            };
            let component = sanitize_session_component(&c.to_string());
            assert!(
                is_valid_session_name(&component),
                "U+{code:04X} produced invalid session component {component:?}"
            );
        }
    }
}
