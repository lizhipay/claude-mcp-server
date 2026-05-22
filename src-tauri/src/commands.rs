use tauri::State;

use crate::{
    claude, config,
    config::{AppConfig, SaveConfigPayload},
    logs::LogSnapshot,
    server::ServerStatus,
    state::AppState,
    token_usage::TokenUsageSnapshot,
};

#[tauri::command]
pub async fn get_config() -> Result<AppConfig, String> {
    Ok(config::load_config())
}

#[tauri::command]
pub async fn save_config(payload: SaveConfigPayload) -> Result<AppConfig, String> {
    config::save_config(payload).map_err(to_message)
}

#[tauri::command]
pub async fn test_api_connection(state: State<'_, AppState>) -> Result<String, String> {
    claude::test_connection(&state).await.map_err(to_message)
}

#[tauri::command]
pub async fn start_mcp_server(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    let cfg = config::load_config();
    config::require_api_key().map_err(to_message)?;
    state
        .server()
        .start(state.inner().clone(), cfg)
        .await
        .map_err(to_message)
}

#[tauri::command]
pub async fn stop_mcp_server(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    state
        .server()
        .stop(state.inner().clone())
        .await
        .map_err(to_message)
}

#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    Ok(state.server().status().await)
}

#[tauri::command]
pub async fn get_logs(state: State<'_, AppState>) -> Result<LogSnapshot, String> {
    Ok(state.logs().snapshot())
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, AppState>) -> Result<LogSnapshot, String> {
    Ok(state.logs().clear())
}

#[tauri::command]
pub async fn get_token_usage(state: State<'_, AppState>) -> Result<TokenUsageSnapshot, String> {
    Ok(state.token_usage().snapshot())
}

#[tauri::command]
pub async fn clear_token_usage(state: State<'_, AppState>) -> Result<TokenUsageSnapshot, String> {
    state.token_usage().clear().map_err(to_message)
}

fn to_message(error: anyhow::Error) -> String {
    error.to_string()
}
