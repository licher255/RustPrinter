// src-tauri/src/commands/system.rs
use super::AppState;
use rust_i18n::t;
use tauri::{AppHandle, State};

#[cfg(target_os = "windows")]
fn firewall_script(program: &std::path::Path) -> String {
    let literal = program.to_string_lossy().replace('\'', "''");
    include_str!("../../../scripts/enable_firewall.ps1").replace(
        "param([Parameter(Mandatory=$true)][string]$Program)",
        &format!("$Program = '{literal}'"),
    )
}

#[tauri::command]
pub async fn enable_lan_access() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        tauri::async_runtime::spawn_blocking(|| {
            use std::os::windows::process::CommandExt;
            let program = std::env::current_exe().map_err(|e| e.to_string())?;
            crate::services::diagnostics::record(&format!(
                "[AirPrinter] requesting administrator approval for firewall rules: program={}",
                program.display()
            ));
            let elevate = r#"$ErrorActionPreference='Stop'; $encoded=[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($env:AIRPRINTER_FIREWALL_SOURCE)); $process=Start-Process -FilePath powershell.exe -ArgumentList ('-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand ' + $encoded) -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $process.ExitCode"#;
            let output = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", elevate])
                .env("AIRPRINTER_FIREWALL_SOURCE", firewall_script(&program))
                .creation_flags(0x08000000)
                .output()
                .map_err(|e| e.to_string())?;
            if output.status.success() {
                crate::services::diagnostics::record(
                    "[AirPrinter] firewall rules installed: inbound TCP 631, TCP 8631-8699, UDP 5353 (local subnet)",
                );
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                crate::services::diagnostics::record(&format!(
                    "[AirPrinter] firewall setup failed: status={} stderr={}",
                    output.status,
                    stderr.trim()
                ));
                Err(format!(
                    "Windows did not enable LAN access (administrator approval may be required): {}",
                    stderr
                ))
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("Windows firewall setup requires Windows".into())
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn elevated_script_quotes_executable_without_a_writable_script_file() {
        let source = firewall_script(std::path::Path::new(r"C:\O'Reilly\AirPrinter.exe"));
        assert!(source.contains(r"$Program = 'C:\O''Reilly\AirPrinter.exe'"));
        assert!(!source.contains("[Parameter(Mandatory=$true)][string]$Program"));
    }
}

#[tauri::command]
pub fn set_language(
    lang: String,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    // 标准化语言代码 (例如 zh-CN -> zh)
    let base_lang = lang.split('-').next().unwrap_or(&lang);

    // 简单的白名单验证
    let valid_locales = ["en", "zh"];

    let lang = if !valid_locales.contains(&base_lang) {
        eprintln!("Unsupported language: {}, falling back to en", lang);
        "en".to_string()
    } else {
        base_lang.to_string()
    };

    rust_i18n::set_locale(&lang);

    // 记住用户选择，下次启动时恢复
    if let Ok(mut store) = state.settings.lock() {
        if let Err(error) = store.update(|s| s.language = lang.clone()) {
            eprintln!("Failed to persist language setting: {error}");
        }
    }

    crate::tray::refresh_tray_menu(&app);

    // 确保 locales 文件中有 messages.lang_switched 这个 key
    println!("{}", t!("messages.lang_switched", locale = lang));

    Ok(())
}
