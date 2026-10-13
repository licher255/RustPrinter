// src-tauri/src/commands/logs.rs

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

/// Location and size of the on-disk diagnostics log, shown in the log panel.
#[derive(Serialize)]
pub struct LogInfo {
    pub path: String,
    pub size_bytes: u64,
}

#[tauri::command]
pub fn get_log_info() -> Result<LogInfo, String> {
    let path = crate::services::diagnostics::file_path()
        .ok_or("Diagnostics logging is not initialized")?;
    let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(LogInfo {
        path: path.display().to_string(),
        size_bytes,
    })
}

/// Newest diagnostics content for the in-app viewer and the copy button.
/// Bounded so a long-running instance never floods the webview.
#[tauri::command]
pub fn read_logs() -> Result<String, String> {
    crate::services::diagnostics::tail(512 * 1024, 4000)
}

/// Reveal the diagnostics log in Explorer so it can be attached to a report.
#[tauri::command]
pub fn open_log_folder(app: AppHandle) -> Result<(), String> {
    let path = crate::services::diagnostics::file_path()
        .ok_or("Diagnostics logging is not initialized")?;
    crate::services::diagnostics::record("[AirPrinter] user revealed the diagnostics log folder");
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| e.to_string())
}
