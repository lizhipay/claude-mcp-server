use serde_json::json;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Emitter, Manager, Wry,
};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::{
    config,
    logs::LogLevel,
    server::{ServerStatus, ServiceStatus},
    state::AppState,
};

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_ID: &str = "claude-mcp-tray";
const MENU_SHOW: &str = "show_window";
const MENU_HIDE: &str = "hide_window";
const MENU_START: &str = "start_service";
const MENU_STOP: &str = "stop_service";
const MENU_COPY: &str = "copy_mcp_url";
const MENU_QUIT: &str = "quit_app";
const SERVER_STATUS_EVENT: &str = "server-status-updated";

pub struct TrayMenuState {
    start_service: MenuItem<Wry>,
    stop_service: MenuItem<Wry>,
    copy_mcp_url: MenuItem<Wry>,
}

pub fn setup(app: &mut App) -> tauri::Result<()> {
    let show_window = MenuItem::with_id(app, MENU_SHOW, "显示 Claude MCP", true, None::<&str>)?;
    let hide_window = MenuItem::with_id(app, MENU_HIDE, "隐藏窗口", true, None::<&str>)?;
    let start_service = MenuItem::with_id(app, MENU_START, "启动服务", true, None::<&str>)?;
    let stop_service = MenuItem::with_id(app, MENU_STOP, "停止服务", false, None::<&str>)?;
    let copy_mcp_url = MenuItem::with_id(app, MENU_COPY, "复制 MCP 地址", false, None::<&str>)?;
    let quit_app = MenuItem::with_id(app, MENU_QUIT, "退出 Claude MCP", true, None::<&str>)?;
    let separator_one = PredefinedMenuItem::separator(app)?;
    let separator_two = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_window,
            &hide_window,
            &separator_one,
            &start_service,
            &stop_service,
            &copy_mcp_url,
            &separator_two,
            &quit_app,
        ],
    )?;

    app.manage(TrayMenuState {
        start_service,
        stop_service,
        copy_mcp_url,
    });

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Claude MCP")
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .on_menu_event(handle_menu_event);

    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }

    #[cfg(target_os = "macos")]
    {
        tray = tray.icon_as_template(false);
    }

    tray.build(app)?;
    update_tray_menu(app.handle(), &stopped_status());
    Ok(())
}

pub fn show_main_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(true);

    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let _ = window.set_skip_taskbar(false);

        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn hide_main_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(false);

    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let _ = window.set_skip_taskbar(true);

        let _ = window.hide();
    }
}

pub fn publish_server_status(app: &AppHandle, status: &ServerStatus) {
    update_tray_menu(app, status);
    let _ = app.emit(SERVER_STATUS_EVENT, status);
}

pub fn quit_app(app: AppHandle) {
    let state = app.state::<AppState>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let _ = state.server().stop(state.clone()).await;
        app.exit(0);
    });
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_SHOW => show_main_window(app),
        MENU_HIDE => hide_main_window(app),
        MENU_START => start_service(app.clone()),
        MENU_STOP => stop_service(app.clone()),
        MENU_COPY => copy_mcp_url(app),
        MENU_QUIT => quit_app(app.clone()),
        _ => {}
    }
}

fn start_service(app: AppHandle) {
    let state = app.state::<AppState>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let result = async {
            config::require_api_key()?;
            let cfg = config::load_config();
            state.server().start(state.clone(), cfg).await
        }
        .await;

        match result {
            Ok(status) => publish_server_status(&app, &status),
            Err(error) => {
                state.logs().push(
                    LogLevel::Error,
                    "server",
                    None,
                    None,
                    "托盘启动 MCP 服务失败",
                    Some(json!({"error": error.to_string()})),
                );
                let status = state.server().status().await;
                publish_server_status(&app, &status);
            }
        }
    });
}

fn stop_service(app: AppHandle) {
    let state = app.state::<AppState>().inner().clone();
    tauri::async_runtime::spawn(async move {
        match state.server().stop(state.clone()).await {
            Ok(status) => publish_server_status(&app, &status),
            Err(error) => {
                state.logs().push(
                    LogLevel::Error,
                    "server",
                    None,
                    None,
                    "托盘停止 MCP 服务失败",
                    Some(json!({"error": error.to_string()})),
                );
                let status = state.server().status().await;
                publish_server_status(&app, &status);
            }
        }
    });
}

fn copy_mcp_url(app: &AppHandle) {
    let state = app.state::<AppState>();
    let app = app.clone();
    let state = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        let status = state.server().status().await;
        if let Some(url) = status.mcp_url {
            match app.clipboard().write_text(url.clone()) {
                Ok(()) => state.logs().push(
                    LogLevel::Info,
                    "tray",
                    None,
                    None,
                    "已复制 MCP 地址",
                    Some(json!({"mcp_url": url})),
                ),
                Err(error) => state.logs().push(
                    LogLevel::Error,
                    "tray",
                    None,
                    None,
                    "复制 MCP 地址失败",
                    Some(json!({"error": error.to_string()})),
                ),
            };
        }
    });
}

fn update_tray_menu(app: &AppHandle, status: &ServerStatus) {
    let Some(menu) = app.try_state::<TrayMenuState>() else {
        return;
    };
    let running = status.status == ServiceStatus::Running;
    let starting = status.status == ServiceStatus::Starting;
    let can_start = !running && !starting;
    let can_stop = running || starting;
    let can_copy = status.mcp_url.is_some();

    let _ = menu.start_service.set_enabled(can_start);
    let _ = menu.stop_service.set_enabled(can_stop);
    let _ = menu.copy_mcp_url.set_enabled(can_copy);
}

fn stopped_status() -> ServerStatus {
    ServerStatus {
        status: ServiceStatus::Stopped,
        mcp_url: None,
        health_url: None,
        message: "休息中".to_string(),
    }
}
