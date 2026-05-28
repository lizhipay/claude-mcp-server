use chrono::Utc;
use tauri::{AppHandle, State};

use crate::{
    agent_bridge::AgentRuntimeStatus,
    agent_runtime, claude, config,
    config::{AppConfig, SaveConfigPayload},
    jobs::{JobStatus, JobSummary},
    logs::{LogEntry, LogLevel, LogPage, LogStats},
    server::ServerStatus,
    sessions::{ChatSessionDetail, ChatSessionsSnapshot},
    state::{AppState, RuntimeStatsSnapshot},
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
pub async fn get_runtime_stats(state: State<'_, AppState>) -> Result<RuntimeStatsSnapshot, String> {
    Ok(state.runtime_stats())
}

#[tauri::command]
pub async fn get_agent_runtime_status(
    state: State<'_, AppState>,
) -> Result<AgentRuntimeStatus, String> {
    Ok(agent_runtime::status(&state))
}

#[tauri::command]
pub async fn get_log_stats(
    state: State<'_, AppState>,
    query: Option<String>,
) -> Result<LogStats, String> {
    Ok(state.logs().stats(query.as_deref()))
}

#[tauri::command]
pub async fn get_log_page(
    state: State<'_, AppState>,
    level: Option<LogLevel>,
    offset: isize,
    limit: isize,
    query: Option<String>,
) -> Result<LogPage, String> {
    let offset = usize::try_from(offset).unwrap_or(0);
    let limit = usize::try_from(limit).unwrap_or(0);
    Ok(state.logs().page(level, offset, limit, query.as_deref()))
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

#[tauri::command]
pub async fn get_chat_sessions(state: State<'_, AppState>) -> Result<ChatSessionsSnapshot, String> {
    Ok(state.sessions().snapshot())
}

#[tauri::command]
pub async fn get_chat_session(
    state: State<'_, AppState>,
    job_id: String,
    limit: Option<usize>,
) -> Result<ChatSessionDetail, String> {
    state
        .sessions()
        .detail(&job_id, limit)
        .ok_or_else(|| "聊天记录不存在或已被清理".to_string())
}

#[tauri::command]
pub async fn send_chat_message(
    state: State<'_, AppState>,
    job_id: String,
    prompt: String,
    workdir: Option<String>,
) -> Result<JobSummary, String> {
    if prompt.trim().is_empty() {
        return Err("续聊内容不能为空".to_string());
    }
    let workdir = match workdir {
        Some(value) if !value.trim().is_empty() => {
            Some(crate::mcp::prepare_workdir(&state, Some(value), "send_chat_message").await?)
        }
        _ => None,
    };
    state
        .jobs()
        .continue_job(state.inner().clone(), &job_id, prompt, workdir)
}

#[tauri::command]
pub async fn delete_chat_session(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<ChatSessionsSnapshot, String> {
    if let Some(detail) = state.sessions().detail(&job_id, Some(1)) {
        if let Some(active_job_id) = detail.summary.active_job_id {
            let _ = state.jobs().cancel(&active_job_id);
            state.sessions().finish_job(
                &active_job_id,
                &JobStatus::Cancelled,
                None,
                Some("任务已取消"),
                Utc::now().timestamp_millis(),
            );
        }
    }
    state.sessions().delete(&job_id).map_err(to_message)
}

#[tauri::command]
pub async fn stop_chat_session(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<ChatSessionsSnapshot, String> {
    let detail = state
        .sessions()
        .detail(&job_id, Some(1))
        .ok_or_else(|| "聊天记录不存在或已被清理".to_string())?;
    let active_job_id = detail
        .summary
        .active_job_id
        .ok_or_else(|| "这个任务当前没有在运行".to_string())?;
    state
        .jobs()
        .cancel(&active_job_id)
        .ok_or_else(|| "任务已经结束或不存在".to_string())?;
    state.sessions().finish_job(
        &active_job_id,
        &JobStatus::Cancelled,
        None,
        Some("任务已取消"),
        Utc::now().timestamp_millis(),
    );
    state.notify_runtime_stats_changed();
    Ok(state.sessions().snapshot())
}

fn to_message(error: anyhow::Error) -> String {
    error.to_string()
}
