mod agent_bridge;
mod agent_runtime;
mod claude;
mod commands;
mod config;
mod jobs;
mod logs;
mod mcp;
mod server;
mod state;
mod token_usage;
mod tray;

use state::AppState;
use tauri::{Manager, RunEvent, WindowEvent};

pub fn run() {
    let state = AppState::new();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(state)
        .setup(|app| {
            app.state::<AppState>().set_app_handle(app.handle().clone());
            tray::setup(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    tray::hide_main_window(window.app_handle());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::test_api_connection,
            commands::start_mcp_server,
            commands::stop_mcp_server,
            commands::get_server_status,
            commands::get_runtime_stats,
            commands::get_agent_runtime_status,
            commands::get_log_stats,
            commands::get_log_page,
            commands::get_log_detail,
            commands::clear_logs,
            commands::get_token_usage,
            commands::clear_token_usage
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Claude MCP");

    app.run(|app, event| match event {
        RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            api.prevent_exit();
            tray::quit_app(app.clone());
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => tray::show_main_window(app),
        _ => {}
    });
}
