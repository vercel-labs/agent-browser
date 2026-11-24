use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct DriverProcess {
    child: Child,
    pub port: u16,
    /// True when the browser was launched without a window.
    pub headless: bool,
}

impl DriverProcess {
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DriverProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A desktop browser automated through a WebDriver server the daemon spawns
/// on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalDriver {
    Safari,
    Ladybird,
}

impl LocalDriver {
    /// Maps a lowercase `--provider` name to its local driver.
    pub fn from_provider(provider: &str) -> Option<Self> {
        match provider {
            "safari" => Some(Self::Safari),
            "ladybird" => Some(Self::Ladybird),
            _ => None,
        }
    }

    /// The provider name, which is also the engine name and WebDriver `browserName`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Safari => "safari",
            Self::Ladybird => "ladybird",
        }
    }

    /// The browser name used in user-facing messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Safari => "Safari",
            Self::Ladybird => "Ladybird",
        }
    }

    fn find(self) -> Option<PathBuf> {
        let (candidates, on_path): (&[&str], &str) = match self {
            Self::Safari => (&["/usr/bin/safaridriver"], "safaridriver"),
            Self::Ladybird => (
                &[
                    "/opt/ladybird/bin/WebDriver",
                    "/Applications/Ladybird.app/Contents/MacOS/WebDriver",
                ],
                "ladybird",
            ),
        };
        if let Some(path) = candidates.iter().map(PathBuf::from).find(|p| p.exists()) {
            return Some(path);
        }

        let output = Command::new("which").arg(on_path).output().ok()?;
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || path.is_empty() {
            return None;
        }
        match self {
            Self::Safari => Some(PathBuf::from(path)),
            Self::Ladybird => PathBuf::from(path)
                .canonicalize()
                .ok()?
                .parent()
                .map(|dir| dir.join("WebDriver"))
                .filter(|p| p.exists()),
        }
    }

    /// Spawns the browser's WebDriver server on `port`. `headless` applies to
    /// Ladybird only; Safari always runs headed.
    pub fn launch(self, port: u16, headless: bool) -> Result<DriverProcess, String> {
        let driver_path = self.find().ok_or(match self {
            Self::Safari => "safaridriver not found. Safari WebDriver requires macOS with Safari.",
            Self::Ladybird => {
                "Ladybird WebDriver not found. Install Ladybird from https://ladybird.org/download/"
            }
        })?;

        let headless = headless && self == Self::Ladybird;
        let mut command = Command::new(&driver_path);
        command.arg("--port").arg(port.to_string());
        if headless {
            command.arg("--headless");
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to launch {} WebDriver: {}", self.label(), e))?;

        // Wait for driver to be ready
        std::thread::sleep(Duration::from_millis(500));

        Ok(DriverProcess {
            child,
            port,
            headless,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_safaridriver() {
        // Only check on macOS
        if cfg!(target_os = "macos") {
            let result = LocalDriver::Safari.find();
            // Don't assert Some since it may not be enabled
            if let Some(path) = result {
                assert!(path.exists());
            }
        }
    }
}
