// Public library interface for agent-browser
// Reuses existing CLI internals to avoid code duplication

mod commands;
mod connection;
mod flags;
mod validation;

use serde_json::Value;
use std::path::PathBuf;

pub use connection::Response;

/// Configuration for agent-browser library
pub struct AgentBrowserConfig {
    pub node_path: String,
    pub dist_dir: String,
    pub profile_path: String,
    pub session: String,
    pub headed: bool,
}

/// Library client
pub struct AgentBrowser {
    config: AgentBrowserConfig,
}

/// Library error types
#[derive(Debug)]
pub enum AgentBrowserError {
    ParseError(String),
    DaemonError(String),
    CommandError(String),
    IoError(String),
}

impl std::fmt::Display for AgentBrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentBrowserError::ParseError(s) => write!(f, "parse error: {}", s),
            AgentBrowserError::DaemonError(s) => write!(f, "daemon error: {}", s),
            AgentBrowserError::CommandError(s) => write!(f, "command error: {}", s),
            AgentBrowserError::IoError(s) => write!(f, "io error: {}", s),
        }
    }
}

impl std::error::Error for AgentBrowserError {}

impl AgentBrowser {
    pub fn new(config: AgentBrowserConfig) -> Self {
        Self { config }
    }

    pub fn run(&self, command: &str) -> Result<Value, AgentBrowserError> {
        let tokens: Vec<String> = shell_split(command);
        if tokens.is_empty() {
            return Err(AgentBrowserError::ParseError("empty command".into()));
        }

        let flags = build_flags(&self.config);
        let cmd = commands::parse_command(&tokens, &flags)
            .map_err(|e| AgentBrowserError::ParseError(e.format()))?;

        self.ensure_daemon()?;

        let resp = connection::send_command(cmd, &self.config.session)
            .map_err(|e| AgentBrowserError::IoError(e))?;

        if resp.success {
            Ok(resp.data.unwrap_or(Value::Null))
        } else {
            Err(AgentBrowserError::CommandError(
                resp.error.unwrap_or_else(|| "unknown error".to_string()),
            ))
        }
    }

    pub fn close(&self) -> Result<Value, AgentBrowserError> {
        self.run("close")
    }

    fn ensure_daemon(&self) -> Result<(), AgentBrowserError> {
        let dist_path = PathBuf::from(&self.config.dist_dir);
        let daemon_path = dist_path.join("daemon.js");
        if !daemon_path.exists() {
            return Err(AgentBrowserError::DaemonError(format!(
                "daemon.js not found at {}",
                daemon_path.display()
            )));
        }

        // Set AGENT_BROWSER_HOME so ensure_daemon can find daemon.js
        let home_path = dist_path.parent().unwrap_or(&dist_path);
        std::env::set_var("AGENT_BROWSER_HOME", home_path);

        let result = connection::ensure_daemon(
            &self.config.session,
            self.config.headed,
            None, // executable_path is for browser, not node
            &[],
            None,
            None,
            None,
            None,
            false,
            false,
            if self.config.profile_path.is_empty() {
                None
            } else {
                Some(&self.config.profile_path)
            },
            None,
            None,
            None,
            None,
        )
        .map_err(|e| AgentBrowserError::DaemonError(e))?;

        if !result.already_running {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        Ok(())
    }
}

fn build_flags(config: &AgentBrowserConfig) -> flags::Flags {
    flags::Flags {
        session: config.session.clone(),
        json: false,
        full: false,
        headed: config.headed,
        debug: false,
        headers: None,
        executable_path: None,
        extensions: Vec::new(),
        cdp: None,
        profile: if config.profile_path.is_empty() {
            None
        } else {
            Some(config.profile_path.clone())
        },
        state: None,
        proxy: None,
        proxy_bypass: None,
        args: None,
        user_agent: None,
        provider: None,
        ignore_https_errors: false,
        allow_file_access: false,
        device: None,
        auto_connect: false,
        session_name: None,
        cli_executable_path: false,
        cli_extensions: false,
        cli_profile: false,
        cli_state: false,
        cli_args: false,
        cli_user_agent: false,
        cli_proxy: false,
        cli_proxy_bypass: false,
        cli_allow_file_access: false,
    }
}

fn shell_split(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_double = false;
    let mut in_single = false;

    for ch in input.chars() {
        match ch {
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            ' ' | '\t' if !in_double && !in_single => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_split() {
        assert_eq!(shell_split("open example.com"), vec!["open", "example.com"]);
        assert_eq!(
            shell_split(r#"fill @e3 "hello world""#),
            vec!["fill", "@e3", "hello world"]
        );
    }
}
