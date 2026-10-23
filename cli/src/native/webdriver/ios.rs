use serde_json::{json, Value};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct IosDevice {
    pub name: String,
    pub udid: String,
    pub state: String,
    pub runtime: String,
    pub is_real: bool,
}

pub fn list_simulators() -> Result<Vec<IosDevice>, String> {
    let output = Command::new("xcrun")
        .args(["simctl", "list", "devices", "--json"])
        .output()
        .map_err(|e| format!("Failed to run xcrun simctl: {}", e))?;

    if !output.status.success() {
        return Err("xcrun simctl failed. Xcode may not be installed.".to_string());
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let parsed: Value =
        serde_json::from_str(&json_str).map_err(|e| format!("Failed to parse simctl: {}", e))?;

    let mut devices = Vec::new();
    if let Some(device_map) = parsed.get("devices").and_then(|v| v.as_object()) {
        for (runtime, device_list) in device_map {
            if let Some(arr) = device_list.as_array() {
                for device in arr {
                    let name = device
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let udid = device
                        .get("udid")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let state = device
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    devices.push(IosDevice {
                        name,
                        udid,
                        state,
                        runtime: runtime.clone(),
                        is_real: false,
                    });
                }
            }
        }
    }
    Ok(devices)
}

pub fn list_real_devices() -> Result<Vec<IosDevice>, String> {
    let output = Command::new("xcrun")
        .args(["xctrace", "list", "devices"])
        .output()
        .map_err(|e| format!("Failed to run xcrun xctrace: {}", e))?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut devices = Vec::new();
    let mut in_devices = false;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("== Devices ==") {
            in_devices = true;
            continue;
        }
        if trimmed.starts_with("== Simulators ==") {
            break;
        }
        if !in_devices || trimmed.is_empty() {
            continue;
        }
        // Format: "Device Name (OS Version) (UDID)"
        if let Some(udid_start) = trimmed.rfind('(') {
            let udid_end = trimmed.len() - 1;
            let udid = &trimmed[udid_start + 1..udid_end];
            // Validate it looks like a UDID (contains hyphens)
            if udid.contains('-') && udid.len() > 20 {
                let name_part = trimmed[..udid_start].trim();
                let name = if let Some(paren_pos) = name_part.rfind('(') {
                    name_part[..paren_pos].trim().to_string()
                } else {
                    name_part.to_string()
                };
                devices.push(IosDevice {
                    name,
                    udid: udid.to_string(),
                    state: "Connected".to_string(),
                    runtime: String::new(),
                    is_real: true,
                });
            }
        }
    }

    Ok(devices)
}

pub fn list_all_devices() -> Result<Vec<IosDevice>, String> {
    let mut all = list_simulators().unwrap_or_default();
    all.extend(list_real_devices().unwrap_or_default());
    Ok(all)
}

fn matches_name_or_udid(device: &IosDevice, selector: &str) -> bool {
    device.udid == selector
        || device.name == selector
        || device
            .name
            .to_lowercase()
            .contains(&selector.to_lowercase())
}

pub fn boot_simulator(udid: &str) -> Result<(), String> {
    let output = Command::new("xcrun")
        .args(["simctl", "boot", udid])
        .output()
        .map_err(|e| format!("Failed to boot simulator: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("current state: Booted") {
            return Ok(());
        }
        return Err(format!("Failed to boot simulator {}: {}", udid, stderr));
    }
    Ok(())
}

pub fn shutdown_simulator(udid: &str) -> Result<(), String> {
    let output = Command::new("xcrun")
        .args(["simctl", "shutdown", udid])
        .output()
        .map_err(|e| format!("Failed to shutdown simulator: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("current state: Shutdown") {
            return Ok(());
        }
        return Err(format!("Failed to shutdown simulator {}: {}", udid, stderr));
    }
    Ok(())
}

fn select_explicit_device(
    devices: Vec<IosDevice>,
    name_or_udid: Option<&str>,
    udid: Option<&str>,
) -> Result<IosDevice, String> {
    if let Some(u) = udid {
        return devices
            .into_iter()
            .find(|d| d.udid == u)
            .ok_or_else(|| format!("No iOS device with UDID '{}' was found", u));
    }

    if let Some(selector) = name_or_udid {
        return devices
            .into_iter()
            .find(|d| matches_name_or_udid(d, selector))
            .ok_or_else(|| format!("No iOS device matched selector '{}'", selector));
    }

    Err("No iOS device selector provided".to_string())
}

pub fn select_device(name_or_udid: Option<&str>, udid: Option<&str>) -> Result<IosDevice, String> {
    if name_or_udid.is_some() || udid.is_some() {
        return select_explicit_device(list_all_devices()?, name_or_udid, udid);
    }

    // Default: prefer most recent iPhone, prefer Pro
    let devices = list_simulators()?;
    let iphone_devices: Vec<&IosDevice> = devices
        .iter()
        .filter(|d| d.name.starts_with("iPhone"))
        .collect();

    if iphone_devices.is_empty() {
        return devices
            .into_iter()
            .next()
            .ok_or("No iOS simulators found".to_string());
    }

    // Prefer Pro models
    if let Some(pro) = iphone_devices.iter().find(|d| d.name.contains("Pro")) {
        return Ok((*pro).clone());
    }

    Ok((*iphone_devices.last().unwrap()).clone())
}

pub fn to_device_json(devices: &[IosDevice]) -> Value {
    let list: Vec<Value> = devices
        .iter()
        .map(|d| {
            json!({
                "name": d.name,
                "udid": d.udid,
                "state": d.state,
                "runtime": d.runtime,
                "isReal": d.is_real,
            })
        })
        .collect();
    json!({ "devices": list })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ios_device_struct() {
        let device = IosDevice {
            name: "iPhone 15 Pro".to_string(),
            udid: "ABC-123".to_string(),
            state: "Booted".to_string(),
            runtime: "iOS-17-0".to_string(),
            is_real: false,
        };
        assert_eq!(device.name, "iPhone 15 Pro");
        assert!(!device.is_real);
    }

    #[test]
    fn test_to_device_json() {
        let devices = vec![IosDevice {
            name: "Test".to_string(),
            udid: "123".to_string(),
            state: "Shutdown".to_string(),
            runtime: "iOS-17".to_string(),
            is_real: false,
        }];
        let json = to_device_json(&devices);
        assert!(json.get("devices").unwrap().as_array().unwrap().len() == 1);
    }

    #[test]
    fn name_or_udid_selector_accepts_name_or_exact_udid() {
        let device = IosDevice {
            name: "iPhone 15 Pro".to_string(),
            udid: "ABC-123".to_string(),
            state: "Shutdown".to_string(),
            runtime: "iOS-17".to_string(),
            is_real: false,
        };

        assert!(matches_name_or_udid(&device, "iPhone 15"));
        assert!(matches_name_or_udid(&device, "ABC-123"));
        assert!(!matches_name_or_udid(&device, "DEF-456"));
    }

    #[test]
    fn missing_explicit_selector_returns_error_without_default_fallback() {
        let device = IosDevice {
            name: "iPhone 15 Pro".to_string(),
            udid: "ABC-123".to_string(),
            state: "Shutdown".to_string(),
            runtime: "iOS-17".to_string(),
            is_real: false,
        };

        let result = select_explicit_device(vec![device], Some("iPhone 99"), None);

        assert_eq!(
            result.unwrap_err(),
            "No iOS device matched selector 'iPhone 99'"
        );
    }
}
