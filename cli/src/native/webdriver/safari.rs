use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct SafariDriverProcess {
    child: Child,
    pub port: u16,
}

impl SafariDriverProcess {
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for SafariDriverProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

pub fn find_safaridriver() -> Option<PathBuf> {
    let candidates = ["/usr/bin/safaridriver"];

    for c in &candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }

    // Try PATH
    if let Ok(output) = Command::new("which").arg("safaridriver").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    None
}

pub fn launch_safaridriver(port: u16) -> Result<SafariDriverProcess, String> {
    let driver_path = find_safaridriver()
        .ok_or("safaridriver not found. Safari WebDriver requires macOS with Safari.")?;

    let child = Command::new(&driver_path)
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to launch safaridriver: {}", e))?;

    // Wait for driver to be ready
    std::thread::sleep(Duration::from_millis(500));

    Ok(SafariDriverProcess { child, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_safaridriver() {
        // Only check on macOS
        if cfg!(target_os = "macos") {
            let result = find_safaridriver();
            // Don't assert Some since it may not be enabled
            if let Some(path) = result {
                assert!(path.exists());
            }
        }
    }
}
