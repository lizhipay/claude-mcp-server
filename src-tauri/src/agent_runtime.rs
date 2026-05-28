use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::{
    agent_bridge::{AgentRuntimeStatus, SdkRuntimeConfig},
    claude,
    config::{self, AgentRuntime},
    logs::LogLevel,
    state::AppState,
};

pub async fn run_agent(
    state: AppState,
    prompt: String,
    cwd: PathBuf,
    task_id: String,
    cancel: CancellationToken,
    resume_session_id: Option<String>,
) -> anyhow::Result<String> {
    match config::load_agent_runtime() {
        AgentRuntime::Sdk => {
            let runtime = load_sdk_runtime_config()?;
            let _upstream_guard = state.begin_upstream_request();
            let result = state
                .agent_bridge()
                .run_job(
                    state.clone(),
                    task_id.clone(),
                    prompt.clone(),
                    cwd.clone(),
                    runtime,
                    cancel.clone(),
                    resume_session_id.clone(),
                )
                .await;
            match result {
                Ok(output) => Ok(output),
                Err(error) if resume_session_id.is_none() && should_fallback_to_legacy(&error) => {
                    state.logs().push(
                        LogLevel::Warn,
                        "agent-runtime",
                        None,
                        Some(task_id.clone()),
                        "Agent SDK 不可用，已切回 legacy runtime",
                        Some(serde_json::json!({"error": error.to_string()})),
                    );
                    claude::run_agent(state, prompt, cwd, task_id, cancel).await
                }
                Err(error) => Err(error),
            }
        }
        AgentRuntime::Legacy => {
            if resume_session_id.is_some() {
                anyhow::bail!("legacy runtime 不支持 Agent SDK session 续聊");
            }
            claude::run_agent(state, prompt, cwd, task_id, cancel).await
        }
    }
}

pub fn status(state: &AppState) -> AgentRuntimeStatus {
    let runtime = match config::load_agent_runtime() {
        AgentRuntime::Sdk => "sdk",
        AgentRuntime::Legacy => "legacy",
    };
    state.agent_bridge().status(runtime)
}

fn load_sdk_runtime_config() -> anyhow::Result<SdkRuntimeConfig> {
    let cfg = config::load_config();
    Ok(SdkRuntimeConfig {
        api_key: config::require_api_key()?,
        base_url: config::normalize_api_base_url(&cfg.api_url)?,
        model: cfg.model,
    })
}

fn should_fallback_to_legacy(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("无法启动 Agent SDK bridge")
        || message.contains("找不到 Agent SDK bridge")
        || message.contains("Agent SDK bridge 尚未启动")
}
