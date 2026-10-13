// src-tauri/src/commands/mod.rs

use std::sync::Mutex;
use crate::services::{AirPrintServer, PrinterDetector, SettingsStore};


// 1. 定义共享的应用状态 (所有命令都需要访问它)
pub struct AppState {
    pub detector: Mutex<PrinterDetector>,
    pub server: Mutex<AirPrintServer>,
    pub settings: Mutex<SettingsStore>,
}

// 2. 声明子模块
pub mod logs;
pub mod printer;
pub mod settings;
pub mod system;

// 3. 重新导出子模块中的所有公开项
// 这样 main.rs 就可以直接写: use airprinter::commands::{get_printers, set_language};
// 而不需要写: use airprinter::commands::printer::get_printers;
pub use logs::*;
pub use printer::*;
pub use settings::*;
pub use system::*;
