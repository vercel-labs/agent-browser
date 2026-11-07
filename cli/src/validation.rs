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

/// Return page headers only when the JSON value is an object of string values.
pub fn validate_headers(
    value: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, String> {
    let headers = value
        .as_object()
        .ok_or_else(|| "headers must be a JSON object with string values".to_string())?;
    if !headers.values().all(serde_json::Value::is_string) {
        return Err("headers must be a JSON object with string values".to_string());
    }
    Ok(headers)
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
    fn headers_validation_accepts_empty_and_string_values() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({ "X-Empty": "" }),
            serde_json::json!({
                "x-MiXeD": "value",
                "Authorization": "Bearer token",
                "X-Unicode": "你好 🌍"
            }),
        ] {
            assert!(validate_headers(&value).is_ok(), "{value}");
        }
    }

    #[test]
    fn headers_validation_rejects_invalid_shapes_without_echoing_input() {
        let invalid = [
            serde_json::json!(null),
            serde_json::json!(false),
            serde_json::json!(0),
            serde_json::json!(1.5),
            serde_json::json!("VALIDATION_HEADER_SECRET_TOP_LEVEL"),
            serde_json::json!([]),
            serde_json::json!([{}]),
            serde_json::json!({ "VALIDATION_HEADER_SECRET_KEY": null }),
            serde_json::json!({ "VALIDATION_HEADER_SECRET_KEY": false }),
            serde_json::json!({ "VALIDATION_HEADER_SECRET_KEY": 1 }),
            serde_json::json!({ "VALIDATION_HEADER_SECRET_KEY": 1.5 }),
            serde_json::json!({ "VALIDATION_HEADER_SECRET_KEY": [] }),
            serde_json::json!({ "VALIDATION_HEADER_SECRET_KEY": {} }),
            serde_json::json!({
                "X-Good": "kept",
                "VALIDATION_HEADER_SECRET_KEY": {
                    "credential": "VALIDATION_HEADER_SECRET_VALUE"
                }
            }),
        ];

        for value in invalid {
            let error = validate_headers(&value).unwrap_err();
            assert_eq!(error, "headers must be a JSON object with string values");
            assert!(!error.contains("VALIDATION_HEADER_SECRET"));
        }
    }
}
