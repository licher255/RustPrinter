use crate::models::AppSettings;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Loads and persists user settings as JSON in the app config directory.
pub struct SettingsStore {
    settings: AppSettings,
    path: PathBuf,
}

impl SettingsStore {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let settings = match std::fs::read(&path) {
            Ok(data) => serde_json::from_slice::<AppSettings>(&data)
                .map_err(|e| format!("Invalid saved settings: {e}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppSettings::default(),
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self { settings, path })
    }

    pub fn get(&self) -> AppSettings {
        self.settings.clone()
    }

    pub fn update(&mut self, apply: impl FnOnce(&mut AppSettings)) -> Result<AppSettings, String> {
        let mut next = self.settings.clone();
        apply(&mut next);
        self.save(&next)?;
        self.settings = next;
        Ok(self.settings.clone())
    }

    fn save(&self, settings: &AppSettings) -> Result<(), String> {
        let parent = self.path.parent().ok_or("Invalid settings path")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let data = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        temp.write_all(&data).map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        temp.persist(&self.path).map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn autostart_command(executable: &Path) -> OsString {
    let mut command = OsString::from("\"");
    command.push(executable.as_os_str());
    command.push("\" --background");
    command
}

/// Enables or disables logon autostart via the per-user Run registry key.
#[cfg(target_os = "windows")]
pub fn apply_autostart(enable: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "AirPrinter";

    let mut command = std::process::Command::new("reg.exe");
    if enable {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        command
            .args(["add", KEY, "/v", VALUE, "/t", "REG_SZ", "/d"])
            .arg(autostart_command(&exe))
            .arg("/f");
    } else {
        command.args(["delete", KEY, "/v", VALUE, "/f"]);
    }
    let output = command
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Deleting a value that was never set is not a failure.
    if !enable && (stderr.contains("unable to find") || stderr.contains("找不到")) {
        return Ok(());
    }
    Err(format!("reg.exe failed: {stderr}"))
}

#[cfg(not(target_os = "windows"))]
pub fn apply_autostart(_enable: bool) -> Result<(), String> {
    Err("Autostart is only supported on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_partial_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut store = SettingsStore::load(path.clone()).unwrap();
        assert_eq!(store.get(), AppSettings::default());

        store
            .update(|s| {
                s.language = "zh".into();
                s.close_to_tray = true;
            })
            .unwrap();
        drop(store);

        let mut reloaded = SettingsStore::load(path.clone()).unwrap();
        assert_eq!(reloaded.get().language, "zh");
        assert!(reloaded.get().close_to_tray);
        assert!(!reloaded.get().autostart);

        reloaded.update(|s| s.autostart = true).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["autostart"], true);
        assert_eq!(raw["language"], "zh");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn autostart_uses_background_mode_and_quotes_the_executable() {
        assert_eq!(
            autostart_command(Path::new(r"C:\Program Files\AirPrinter\AirPrinter.exe")),
            OsString::from(r#""C:\Program Files\AirPrinter\AirPrinter.exe" --background"#)
        );
    }
}
