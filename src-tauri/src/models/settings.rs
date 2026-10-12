use serde::{Deserialize, Serialize};

fn default_language() -> String {
    "en".into()
}

fn default_close_to_tray() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AppSettings {
    #[serde(default = "default_language")]
    pub language: String,
    /// Hide to the system tray instead of quitting when the window is closed.
    #[serde(default = "default_close_to_tray")]
    pub close_to_tray: bool,
    /// Register the app in the Windows Run key so it starts at logon.
    #[serde(default)]
    pub autostart: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            language: default_language(),
            close_to_tray: default_close_to_tray(),
            autostart: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_uses_defaults() {
        let settings: AppSettings = serde_json::from_str(r#"{"language":"zh"}"#).unwrap();
        assert_eq!(settings.language, "zh");
        assert!(settings.close_to_tray);
        assert!(!settings.autostart);
        assert_eq!(
            serde_json::from_str::<AppSettings>("{}").unwrap(),
            AppSettings::default()
        );
    }

    #[test]
    fn explicit_false_close_to_tray_is_preserved() {
        let settings: AppSettings = serde_json::from_str(r#"{"close_to_tray":false}"#).unwrap();
        assert!(!settings.close_to_tray);
    }
}
