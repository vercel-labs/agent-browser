use serde_json::Value;

/// The mouse button a click uses, named as CDP names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub const ALL: [MouseButton; 5] = [
        MouseButton::Left,
        MouseButton::Right,
        MouseButton::Middle,
        MouseButton::Back,
        MouseButton::Forward,
    ];

    /// Parses the lowercase CDP button name.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "middle" => Some(Self::Middle),
            "back" => Some(Self::Back),
            "forward" => Some(Self::Forward),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
            Self::Back => "back",
            Self::Forward => "forward",
        }
    }

    /// The CDP `buttons` bit for this button.
    pub fn mask(self) -> i32 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
            Self::Back => 8,
            Self::Forward => 16,
        }
    }

    /// The first button held in the CDP `buttons` bitmask.
    pub fn primary(buttons: i32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|button| buttons & button.mask() != 0)
    }
}

/// Keyboard modifiers held during a mouse event, kept as the CDP modifier bitmask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers(i32);

impl Modifiers {
    /// The CDP bit for a modifier name, case-insensitive.
    pub fn bit(name: &str) -> Option<i32> {
        match name.to_lowercase().as_str() {
            "alt" => Some(1),
            "control" | "ctrl" => Some(2),
            "meta" | "cmd" | "command" => Some(4),
            "shift" => Some(8),
            _ => None,
        }
    }

    /// Parses a comma or plus separated modifier list such as `ctrl+shift`.
    pub fn parse_list(list: &str) -> Result<Self, String> {
        let mut mask = 0;
        for part in list.split([',', '+']) {
            let part = part.trim();
            let bit = if part.is_empty() {
                None
            } else {
                Self::bit(part)
            };
            match bit {
                Some(bit) => mask |= bit,
                None => {
                    return Err(format!(
                        "Unknown modifier '{}'. Use alt, control, meta, or shift",
                        part
                    ))
                }
            }
        }
        Ok(Self(mask))
    }

    /// Accepts a CDP modifier bitmask that only holds the four known bits.
    pub fn from_mask(mask: i64) -> Option<Self> {
        (0..=15).contains(&mask).then_some(Self(mask as i32))
    }

    pub fn mask(self) -> i32 {
        self.0
    }

    /// The `modifiers` field to send, left out when nothing is held.
    pub fn cdp(self) -> Option<i32> {
        (self.0 != 0).then_some(self.0)
    }
}

/// A full click: which button, how many presses, and which modifiers are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickSpec {
    pub button: MouseButton,
    pub count: u8,
    pub modifiers: Modifiers,
}

impl Default for ClickSpec {
    fn default() -> Self {
        Self {
            button: MouseButton::Left,
            count: 1,
            modifiers: Modifiers::default(),
        }
    }
}

impl ClickSpec {
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// What a click command asks for: a normal pointer click, or opening the link URL in a new tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickPlan {
    NewTab,
    Pointer(ClickSpec),
}

impl ClickPlan {
    /// Decodes and validates the `button`, `clickCount`, `modifiers`, and `newTab` fields.
    pub fn from_command(cmd: &Value, default_count: u8) -> Result<Self, String> {
        let button = match cmd.get("button") {
            None | Some(Value::Null) => MouseButton::Left,
            Some(Value::String(name)) => MouseButton::parse(name).ok_or_else(|| {
                format!(
                    "Unknown mouse button '{}'. Use left, right, middle, back, or forward",
                    name
                )
            })?,
            Some(other) => {
                return Err(format!(
                    "Unknown mouse button '{}'. Use left, right, middle, back, or forward",
                    other
                ))
            }
        };

        let count = match cmd.get("clickCount") {
            None | Some(Value::Null) => default_count,
            Some(value) => match value.as_i64() {
                Some(n) if (1..=255).contains(&n) => n as u8,
                _ => return Err("clickCount must be between 1 and 255".to_string()),
            },
        };

        let modifiers = match cmd.get("modifiers") {
            None | Some(Value::Null) => Modifiers::default(),
            Some(value) => value
                .as_i64()
                .and_then(Modifiers::from_mask)
                .ok_or_else(|| {
                    "modifiers must be a bitmask between 0 and 15 (Alt 1, Control 2, Meta 4, Shift 8)"
                        .to_string()
                })?,
        };

        if cmd.get("newTab").and_then(Value::as_bool) == Some(true) {
            if cmd.get("button").is_some_and(|v| !v.is_null()) || modifiers.mask() != 0 {
                return Err(
                    "--new-tab opens the link URL directly and cannot be combined with --button or --modifiers"
                        .to_string(),
                );
            }
            return Ok(Self::NewTab);
        }

        Ok(Self::Pointer(ClickSpec {
            button,
            count,
            modifiers,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_command_defaults_use_the_given_count() {
        assert_eq!(
            ClickPlan::from_command(&json!({}), 1).unwrap(),
            ClickPlan::Pointer(ClickSpec::default())
        );
        assert_eq!(
            ClickPlan::from_command(&json!({}), 2).unwrap(),
            ClickPlan::Pointer(ClickSpec {
                count: 2,
                ..ClickSpec::default()
            })
        );
    }

    #[test]
    fn from_command_reads_button_and_modifiers() {
        let plan =
            ClickPlan::from_command(&json!({ "button": "right", "modifiers": 10 }), 1).unwrap();
        assert_eq!(
            plan,
            ClickPlan::Pointer(ClickSpec {
                button: MouseButton::Right,
                count: 1,
                modifiers: Modifiers::from_mask(10).unwrap(),
            })
        );
        assert_eq!(MouseButton::Right.mask(), 2);
    }

    #[test]
    fn from_command_rejects_bad_button_and_modifiers() {
        assert!(ClickPlan::from_command(&json!({ "button": "bogus" }), 1).is_err());
        assert!(ClickPlan::from_command(&json!({ "modifiers": 16 }), 1).is_err());
        assert!(ClickPlan::from_command(&json!({ "clickCount": 0 }), 1).is_err());
    }

    #[test]
    fn new_tab_rejects_button_and_modifiers() {
        assert_eq!(
            ClickPlan::from_command(&json!({ "newTab": true }), 1).unwrap(),
            ClickPlan::NewTab
        );
        assert!(ClickPlan::from_command(&json!({ "newTab": true, "button": "left" }), 1).is_err());
        assert!(ClickPlan::from_command(&json!({ "newTab": true, "modifiers": 1 }), 1).is_err());
    }

    #[test]
    fn parse_list_accepts_both_separators_and_aliases() {
        assert_eq!(
            Modifiers::parse_list("Ctrl+Shift").unwrap(),
            Modifiers::parse_list("control,shift").unwrap()
        );
        assert_eq!(Modifiers::parse_list("Ctrl+Shift").unwrap().mask(), 10);
        assert!(Modifiers::parse_list("ctrl,super").is_err());
        assert!(Modifiers::parse_list("").is_err());
    }
}
