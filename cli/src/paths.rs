use std::env;
use std::path::PathBuf;

const APP_DIR: &str = "agent-browser";

fn env_dir(name: &str) -> Option<PathBuf> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

fn home_fallback(suffix: &[&str]) -> Option<PathBuf> {
    let mut path = dirs::home_dir()?;
    for part in suffix {
        path = path.join(part);
    }
    Some(path)
}

/// Config files belong under the platform config directory.
pub fn config_dir() -> PathBuf {
    env_dir("AGENT_BROWSER_CONFIG_DIR")
        .or_else(dirs::config_dir)
        .or_else(|| home_fallback(&[".config"]))
        .unwrap_or_else(env::temp_dir)
        .join(APP_DIR)
}

/// Persistent application data belongs under the platform data directory.
pub fn data_dir() -> PathBuf {
    env_dir("AGENT_BROWSER_DATA_DIR")
        .or_else(dirs::data_local_dir)
        .or_else(|| home_fallback(&[".local", "share"]))
        .unwrap_or_else(env::temp_dir)
        .join(APP_DIR)
}

/// Re-creatable artifacts belong under the platform cache directory.
pub fn cache_dir() -> PathBuf {
    env_dir("AGENT_BROWSER_CACHE_DIR")
        .or_else(dirs::cache_dir)
        .or_else(|| home_fallback(&[".cache"]))
        .unwrap_or_else(env::temp_dir)
        .join(APP_DIR)
}

/// Runtime files prefer AGENT_BROWSER_RUNTIME_DIR, then XDG_RUNTIME_DIR, and
/// otherwise fall back to app data.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = env_dir("AGENT_BROWSER_RUNTIME_DIR") {
        return dir.join(APP_DIR);
    }

    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join(APP_DIR);
        }
    }

    data_dir().join("run")
}

pub fn user_config_file() -> PathBuf {
    config_dir().join("config.json")
}

pub fn browsers_dir() -> PathBuf {
    data_dir().join("browsers")
}

pub fn state_dir() -> PathBuf {
    data_dir()
}

pub fn sessions_dir() -> PathBuf {
    state_dir().join("sessions")
}

pub fn auth_dir() -> PathBuf {
    data_dir().join("auth")
}

pub fn cache_tmp_dir() -> PathBuf {
    cache_dir().join("tmp")
}
