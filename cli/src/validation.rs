/// Validation utilities for session names and other inputs.
/// Mirrors the TypeScript validation in src/state-utils.ts

/// Validates that a session name is safe for use in file paths.
/// Must be alphanumeric with hyphens and underscores only.
/// Returns true if valid, false otherwise.
///
/// Uses byte-level iteration for performance since session names are ASCII-only.
/// Single pass validates all constraints simultaneously.
#[inline]
pub fn is_valid_session_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    
    // Single pass: check each byte for validity
    // This is faster than multiple contains() calls + chars().all()
    for &b in name.as_bytes() {
        // Only allow: a-z, A-Z, 0-9, -, _
        // Rejects: dots (.), slashes (/\), spaces, and all other special chars
        let is_valid = b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        if !is_valid {
            return false;
        }
    }
    
    true
}

/// Returns a validation error message for an invalid session name.
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
    fn test_valid_session_names() {
        assert!(is_valid_session_name("myproject"));
        assert!(is_valid_session_name("my-project"));
        assert!(is_valid_session_name("my_project"));
        assert!(is_valid_session_name("MyProject123"));
        assert!(is_valid_session_name("test-session_v2"));
    }

    #[test]
    fn test_invalid_session_names() {
        // Empty
        assert!(!is_valid_session_name(""));
        
        // Path traversal attempts
        assert!(!is_valid_session_name(".."));
        assert!(!is_valid_session_name("../etc/passwd"));
        assert!(!is_valid_session_name("..\\windows"));
        assert!(!is_valid_session_name("foo/bar"));
        assert!(!is_valid_session_name("foo\\bar"));
        
        // Special characters
        assert!(!is_valid_session_name("my project"));  // spaces
        assert!(!is_valid_session_name("my.project"));  // dots
        assert!(!is_valid_session_name("my@project"));  // at sign
        assert!(!is_valid_session_name("my:project"));  // colon
        assert!(!is_valid_session_name("my*project"));  // asterisk
    }
}
