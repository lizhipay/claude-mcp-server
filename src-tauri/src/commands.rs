use tauri::{AppHandle, State};

use crate::{
    claude, config,
    config::{AppConfig, SaveConfigPayload},
    logs::{LogEntry, LogLevel, LogPage, LogStats},
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
pub async fn start_mcp_server(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ServerStatus, String> {
    let cfg = config::load_config();
    config::require_api_key().map_err(to_message)?;
    let result = state
        .server()
        .start(state.inner().clone(), cfg)
        .await
        .map_err(to_message);
    match result {
        Ok(status) => {
            crate::tray::publish_server_status(&app, &status);
            Ok(status)
        }
        Err(error) => {
            let status = state.server().status().await;
            crate::tray::publish_server_status(&app, &status);
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn stop_mcp_server(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ServerStatus, String> {
    let result = state
        .server()
        .stop(state.inner().clone())
        .await
        .map_err(to_message);
    match result {
        Ok(status) => {
            crate::tray::publish_server_status(&app, &status);
            Ok(status)
        }
        Err(error) => {
            let status = state.server().status().await;
            crate::tray::publish_server_status(&app, &status);
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    Ok(state.server().status().await)
}

#[tauri::command]
pub async fn get_log_stats(state: State<'_, AppState>) -> Result<LogStats, String> {
    Ok(state.logs().stats())
}

#[tauri::command]
pub async fn get_log_page(
    state: State<'_, AppState>,
    level: Option<LogLevel>,
    offset: isize,
    limit: isize,
) -> Result<LogPage, String> {
    let offset = usize::try_from(offset).unwrap_or(0);
    let limit = usize::try_from(limit).unwrap_or(0);
    Ok(state.logs().page(level, offset, limit))
}

#[tauri::command]
pub async fn get_log_detail(state: State<'_, AppState>, id: u64) -> Result<LogEntry, String> {
    state
        .logs()
        .detail(id)
        .ok_or_else(|| "日志详情不存在或已被清理".to_string())
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, AppState>) -> Result<LogStats, String> {
    Ok(state.logs().clear_stats())
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
