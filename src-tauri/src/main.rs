// Windows queue bridge: DNS-SD discovery and IPP transport.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::Manager;

use airprinter::services::{AirPrintServer, PrinterDetector};
use airprinter::*;

// 导入命令
use airprinter::commands::{
    enable_lan_access, get_printers, get_settings, get_shared_printers, set_autostart,
    set_close_to_tray, set_language, share_printer, stop_printer, unshare_printer, AppState,
};

const FOREGROUND_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const BACKGROUND_RECONCILE_INTERVAL: Duration = Duration::from_secs(300);

fn main() {
    let startup = match startup_options() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("AirPrinter startup argument error: {error}");
            std::process::exit(2);
        }
    };
    let background = startup.background;
    let receive_only_dir = startup.receive_only_dir;
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║              🖨️  AirPrinter 启动中...                    ║");
    println!("╚══════════════════════════════════════════════════════════╝");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            let config_dir = app.path().app_config_dir()?;
            if let Err(error) = airprinter::services::diagnostics::init(&config_dir) {
                eprintln!("[AirPrinter] 初始化打印诊断记录失败: {error}");
            }
            let preferences = config_dir.join("shared-printers.json");
            let settings_path = config_dir.join("settings.json");
            let settings_store = airprinter::services::SettingsStore::load(settings_path)
                .map_err(std::io::Error::other)?;
            let settings = settings_store.get();
            let mut airprint_server =
                AirPrintServer::with_preferences(preferences).map_err(std::io::Error::other)?;
            if let Some(directory) = &receive_only_dir {
                std::fs::create_dir_all(directory)?;
                if !directory.is_dir() {
                    return Err(
                        std::io::Error::other("receive-only path is not a directory").into(),
                    );
                }
                airprinter::services::diagnostics::record(&format!(
                    "[AirPrinter] receive-only mode: saving validated documents to {}",
                    directory.display()
                ));
            }
            airprint_server
                .set_receive_directory(receive_only_dir.clone())
                .map_err(std::io::Error::other)?;

            // 恢复上次选择的语言
            rust_i18n::set_locale(&settings.language);

            // 让注册表中的开机启动项与保存的设置保持一致
            if settings.autostart {
                if let Err(error) = airprinter::services::settings::apply_autostart(true) {
                    eprintln!("[AirPrinter] 恢复开机启动失败: {error}");
                }
            }

            app.manage(AppState {
                detector: Mutex::new(PrinterDetector::new()),
                server: Mutex::new(airprint_server),
                settings: Mutex::new(settings_store),
            });

            if let Err(error) = airprinter::tray::setup_tray(app.handle()) {
                eprintln!("[AirPrinter] 系统托盘初始化失败: {error}");
            }
            if !background {
                airprinter::tray::show_main_window(app.handle()).map_err(std::io::Error::other)?;
            }

            let handle = app.handle().clone();
            thread::spawn::<_, ()>(move || loop {
                let state = handle.state::<AppState>();
                let detected = match state.detector.lock() {
                    Ok(detector) => detector.detect(),
                    Err(error) => Err(error.to_string()),
                };
                match detected {
                    Ok(printers) => {
                        if let Ok(mut server) = state.server.lock() {
                            for error in server.reconcile(&printers) {
                                eprintln!("[AirPrinter] 共享同步失败: {error}");
                            }
                        }
                    }
                    Err(error) => eprintln!("[AirPrinter] 检测 Windows 打印机失败: {error}"),
                }
                thread::sleep(reconcile_interval(
                    handle.get_webview_window("main").is_some(),
                ));
            });

            println!(
                "✅ 后端初始化完成，当前语言: {}",
                rust_i18n::locale().to_string()
            );
            println!("");
            println!("📋 AirPrint 服务发现机制：");
            println!("   • _ipp._tcp (端口 631)              - 基础 IPP 服务");
            println!("   • _printer._tcp (端口 0)            - RFC 6763 Flagship Naming");
            println!("   • _universal._sub._ipp._tcp        - AirPrint 发现子类型");
            println!("");
            println!("⚠️  使用提示：");
            println!("   1. 确保手机和电脑在可互通的同一局域网，电脑可使用网线");
            println!("   2. 点击“允许局域网访问”，为本程序放行 mDNS 和 IPP");
            println!("   3. 路由器不能开启 'AP隔离' / '客户端隔离'");
            println!("");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_printers,
            share_printer,
            stop_printer,
            get_shared_printers,
            unshare_printer,
            enable_lan_access,
            set_language,
            get_settings,
            set_close_to_tray,
            set_autostart,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let close_to_tray = window
                    .state::<AppState>()
                    .settings
                    .lock()
                    .map(|store| store.get().close_to_tray)
                    .unwrap_or(false);
                if close_to_tray {
                    api.prevent_close();
                    if let Err(error) = window.destroy() {
                        eprintln!("[AirPrinter] 释放主窗口失败: {error}");
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app, event| {
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                let close_to_tray = app
                    .state::<AppState>()
                    .settings
                    .lock()
                    .map(|store| store.get().close_to_tray)
                    .unwrap_or(false);
                if should_prevent_exit(background, close_to_tray) {
                    api.prevent_exit();
                }
            }
        });
}

fn reconcile_interval(has_main_window: bool) -> Duration {
    if has_main_window {
        FOREGROUND_RECONCILE_INTERVAL
    } else {
        BACKGROUND_RECONCILE_INTERVAL
    }
}

fn should_prevent_exit(background: bool, close_to_tray: bool) -> bool {
    background || close_to_tray
}

#[derive(Debug, PartialEq)]
struct StartupOptions {
    background: bool,
    receive_only_dir: Option<PathBuf>,
}

fn startup_options() -> Result<StartupOptions, String> {
    parse_startup_options(std::env::args_os().skip(1))
}

fn parse_startup_options(
    args: impl IntoIterator<Item = OsString>,
) -> Result<StartupOptions, String> {
    let mut args = args.into_iter();
    let mut options = StartupOptions {
        background: false,
        receive_only_dir: None,
    };
    while let Some(arg) = args.next() {
        if arg == "--background" {
            options.background = true;
        } else if arg == "--receive-only" {
            if options.receive_only_dir.is_some() {
                return Err("--receive-only may be specified only once".into());
            }
            let value = args
                .next()
                .ok_or("--receive-only requires an absolute directory path")?;
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err("--receive-only requires an absolute directory path".into());
            }
            options.receive_only_dir = Some(path);
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_options_combine_background_and_receive_only() {
        let directory = std::env::temp_dir().join("airprinter-receive-only");
        let options = parse_startup_options([
            OsString::from("--background"),
            OsString::from("--receive-only"),
            directory.clone().into_os_string(),
        ])
        .unwrap();

        assert!(options.background);
        assert_eq!(options.receive_only_dir, Some(directory));
    }

    #[test]
    fn receive_only_still_rejects_duplicate_values() {
        let directory = std::env::temp_dir();
        let result = parse_startup_options([
            OsString::from("--receive-only"),
            directory.clone().into_os_string(),
            OsString::from("--receive-only"),
            directory.into_os_string(),
        ]);

        assert_eq!(
            result.unwrap_err(),
            "--receive-only may be specified only once"
        );
    }

    #[test]
    fn background_reconciliation_runs_less_often() {
        assert_eq!(reconcile_interval(true), Duration::from_secs(30));
        assert_eq!(reconcile_interval(false), Duration::from_secs(300));
    }

    #[test]
    fn background_mode_stays_alive_without_a_window() {
        assert!(should_prevent_exit(true, false));
        assert!(should_prevent_exit(false, true));
        assert!(!should_prevent_exit(false, false));
    }
}
