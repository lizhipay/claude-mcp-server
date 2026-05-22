mod claude;
mod commands;
mod config;
mod jobs;
mod logs;
mod mcp;
mod server;
mod state;
mod token_usage;

use state::AppState;
use tauri::Manager;

pub fn run() {
    let state = AppState::new();
    tauri::Builder::default()
        .manage(state)
        .setup(|app| {
            app.state::<AppState>().set_app_handle(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::test_api_connection,
            commands::start_mcp_server,
            commands::stop_mcp_server,
            commands::get_server_status,
            commands::get_logs,
            commands::clear_logs,
            commands::get_token_usage,
            commands::clear_token_usage
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Claude MCP");
}
