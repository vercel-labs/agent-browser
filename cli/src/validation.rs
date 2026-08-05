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
            out.extend(c.to_lowercase());
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
    fn sanitize_session_component_produces_valid_component() {
        let value = sanitize_session_component("Next Dev Loop: /Users/me/worktree!");

        assert_eq!(value, "next-dev-loop-users-me-worktree");
        assert!(is_valid_session_name(&value));
    }

    #[test]
    fn sanitize_session_component_trims_separators() {
        assert_eq!(sanitize_session_component(" --Agent__ "), "agent");
    }
}
