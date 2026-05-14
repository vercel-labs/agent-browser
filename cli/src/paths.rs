use std::env;
use std::path::PathBuf;

const APP_DIR: &str = "agent-browser";

fn env_path(name: &str) -> Option<PathBuf> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

fn home_fallback(parts: &[&str]) -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(env::temp_dir);
    for part in parts {
        path = path.join(part);
    }
    path
}

fn app_base(env_name: &str, xdg_name: &str, fallback: &[&str]) -> PathBuf {
    env_path(env_name)
        .or_else(|| env_path(xdg_name))
        .unwrap_or_else(|| home_fallback(fallback))
        .join(APP_DIR)
}

pub fn config_dir() -> PathBuf {
    if let Some(home) = env_path("AGENT_BROWSER_HOME") {
        return home.join("config");
    }

    app_base("AGENT_BROWSER_CONFIG_DIR", "XDG_CONFIG_HOME", &[".config"])
}

pub fn state_dir() -> PathBuf {
    if let Some(home) = env_path("AGENT_BROWSER_HOME") {
        return home.join("state");
    }

    app_base(
        "AGENT_BROWSER_STATE_DIR",
        "XDG_STATE_HOME",
        &[".local", "state"],
    )
}

pub fn data_dir() -> PathBuf {
    if let Some(home) = env_path("AGENT_BROWSER_HOME") {
        return home.join("data");
    }

    app_base(
        "AGENT_BROWSER_DATA_DIR",
        "XDG_DATA_HOME",
        &[".local", "share"],
    )
}

pub fn cache_dir() -> PathBuf {
    if let Some(home) = env_path("AGENT_BROWSER_HOME") {
        return home.join("cache");
    }

    app_base("AGENT_BROWSER_CACHE_DIR", "XDG_CACHE_HOME", &[".cache"])
}

pub fn user_config_file() -> PathBuf {
    config_dir().join("config.json")
}

pub fn browsers_dir() -> PathBuf {
    data_dir().join("browsers")
}

pub fn sessions_dir() -> PathBuf {
    state_dir().join("sessions")
}

pub fn auth_dir() -> PathBuf {
    state_dir().join("auth")
}

pub fn encryption_key_file() -> PathBuf {
    state_dir().join(".encryption-key")
}

pub fn tmp_dir() -> PathBuf {
    cache_dir().join("tmp")
}

#[cfg(test)]
mod tests {
    use super::*;

    const VARS: &[&str] = &[
        "AGENT_BROWSER_HOME",
        "AGENT_BROWSER_CONFIG_DIR",
        "AGENT_BROWSER_STATE_DIR",
        "AGENT_BROWSER_DATA_DIR",
        "AGENT_BROWSER_CACHE_DIR",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "HOME",
    ];

    fn clear_vars(guard: &crate::test_utils::EnvGuard<'_>) {
        for var in VARS {
            guard.remove(var);
        }
    }

    #[test]
    fn agent_browser_home_owns_all_dirs() {
        let guard = crate::test_utils::EnvGuard::new(VARS);
        clear_vars(&guard);
        guard.set("AGENT_BROWSER_HOME", "/tmp/ab-home");

        assert_eq!(config_dir(), PathBuf::from("/tmp/ab-home/config"));
        assert_eq!(state_dir(), PathBuf::from("/tmp/ab-home/state"));
        assert_eq!(data_dir(), PathBuf::from("/tmp/ab-home/data"));
        assert_eq!(cache_dir(), PathBuf::from("/tmp/ab-home/cache"));
    }

    #[test]
    fn xdg_dirs_are_used_with_app_suffix() {
        let guard = crate::test_utils::EnvGuard::new(VARS);
        clear_vars(&guard);
        guard.set("XDG_CONFIG_HOME", "/tmp/config");
        guard.set("XDG_STATE_HOME", "/tmp/state");
        guard.set("XDG_DATA_HOME", "/tmp/data");
        guard.set("XDG_CACHE_HOME", "/tmp/cache");

        assert_eq!(config_dir(), PathBuf::from("/tmp/config/agent-browser"));
        assert_eq!(state_dir(), PathBuf::from("/tmp/state/agent-browser"));
        assert_eq!(data_dir(), PathBuf::from("/tmp/data/agent-browser"));
        assert_eq!(cache_dir(), PathBuf::from("/tmp/cache/agent-browser"));
    }

    #[test]
    fn agent_browser_dir_overrides_win_over_xdg() {
        let guard = crate::test_utils::EnvGuard::new(VARS);
        clear_vars(&guard);
        guard.set("AGENT_BROWSER_STATE_DIR", "/tmp/custom-state");
        guard.set("XDG_STATE_HOME", "/tmp/xdg-state");

        assert_eq!(
            state_dir(),
            PathBuf::from("/tmp/custom-state/agent-browser")
        );
    }
}
