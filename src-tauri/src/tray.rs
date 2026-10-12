use rust_i18n::t;
use tauri::menu::{Menu, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime, WebviewWindowBuilder};

pub const TRAY_ID: &str = "main";

pub fn build_tray_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let show = MenuItemBuilder::with_id("show", t!("tray.show")).build(app)?;
    let quit = MenuItemBuilder::with_id("quit", t!("tray.quit")).build(app)?;
    Menu::with_items(app, &[&show, &quit])
}

pub fn show_main_window(app: &AppHandle) -> Result<(), String> {
    let window = match app.get_webview_window("main") {
        Some(window) => window,
        None => {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|config| config.label == "main")
                .ok_or("main window configuration is missing")?;
            WebviewWindowBuilder::from_config(app, config)
                .map_err(|error| error.to_string())?
                .build()
                .map_err(|error| error.to_string())?
        }
    };
    window.show().map_err(|error| error.to_string())?;
    window.unminimize().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())?;
    Ok(())
}

pub fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("AirPrinter")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Err(error) = show_main_window(app) {
                    eprintln!("[AirPrinter] 无法打开主窗口: {error}");
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Err(error) = show_main_window(tray.app_handle()) {
                    eprintln!("[AirPrinter] 无法打开主窗口: {error}");
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Rebuilds the tray menu so its labels follow the current locale.
pub fn refresh_tray_menu(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), build_tray_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}
