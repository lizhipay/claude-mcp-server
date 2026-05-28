use std::{io::ErrorKind, path::PathBuf};

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;

use crate::{claude, logs::LogLevel, state::AppState};

const DEFAULT_WAIT_SECONDS: u64 = 90;
const DEFAULT_POLL_SECONDS: u64 = 3;
const MAX_WAIT_SECONDS: u64 = 600;
const MAX_BATCH_JOB_IDS: usize = 100;
const MAX_POLL_JOB_IDS: usize = 500;
const DEFAULT_BATCH_RECENT_CHARS: usize = 4_000;

#[derive(Clone)]
pub struct ClaudeMcpService {
    state: AppState,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CodeRequest {
    #[schemars(description = "The coding task or question to send to Claude.")]
    pub prompt: String,
    #[schemars(description = "Working directory. Defaults to the server process cwd.")]
    pub workdir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CodeWithContextRequest {
    #[schemars(description = "The coding task or question.")]
    pub prompt: String,
    #[schemars(description = "Files to include as context, relative to workdir unless absolute.")]
    pub files: Vec<String>,
    #[schemars(description = "Working directory. Defaults to the server process cwd.")]
    pub workdir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StatusRequest {
    pub job_id: String,
    pub recent_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ResultRequest {
    pub job_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitRequest {
    pub job_id: String,
    pub timeout_seconds: Option<u64>,
    pub recent_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BatchWaitRequest {
    pub job_ids: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub recent_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BatchResultRequest {
    pub job_ids: Vec<String>,
    pub recent_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BatchPollRequest {
    pub job_ids: Vec<String>,
    pub seen_job_ids: Option<Vec<String>>,
    pub timeout_seconds: Option<u64>,
    pub recent_chars: Option<usize>,
    pub include_running: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ContinueStartRequest {
    #[schemars(description = "Existing job_id whose Agent SDK session should be resumed.")]
    pub job_id: String,
    #[schemars(description = "Follow-up instruction to send into the same Agent SDK session.")]
    pub prompt: String,
    #[schemars(
        description = "Optional working directory override. Defaults to the original session workdir."
    )]
    pub workdir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ContinueWaitRequest {
    #[schemars(description = "Existing job_id whose Agent SDK session should be resumed.")]
    pub job_id: String,
    #[schemars(description = "Follow-up instruction to send into the same Agent SDK session.")]
    pub prompt: String,
    #[schemars(
        description = "Optional working directory override. Defaults to the original session workdir."
    )]
    pub workdir: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub recent_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ChatHistoryRequest {
    pub job_id: String,
    pub limit: Option<usize>,
}

impl ClaudeMcpService {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl ClaudeMcpService {
    #[tool(
        description = "Send a coding task to Claude and return the response. If it runs longer than about 90 seconds, this returns a job_id."
    )]
    async fn code(&self, Parameters(req): Parameters<CodeRequest>) -> String {
        let cwd = match self.prepare_workdir(req.workdir, "code").await {
            Ok(cwd) => cwd,
            Err(error) => return format!("[workdir error]\n{error}"),
        };
        self.state.logs().push(
            LogLevel::Info,
            "mcp",
            None,
            None,
            "收到 code 调用",
            Some(json!({"workdir": cwd, "prompt_chars": req.prompt.chars().count()})),
        );
        self.state
            .jobs()
            .run_with_fast_fallback(self.state.clone(), req.prompt, cwd)
            .await
    }

    #[tool(
        description = "Send a coding task to Claude with specific files as context. If it runs longer than about 90 seconds, this returns a job_id."
    )]
    async fn code_with_context(
        &self,
        Parameters(req): Parameters<CodeWithContextRequest>,
    ) -> String {
        let cwd = match self.prepare_workdir(req.workdir, "code_with_context").await {
            Ok(cwd) => cwd,
            Err(error) => return format!("[workdir error]\n{error}"),
        };
        match build_context_prompt(&cwd, &req.files, &req.prompt).await {
            Ok(prompt) => {
                self.state.logs().push(
                    LogLevel::Info,
                    "mcp",
                    None,
                    None,
                    "收到 code_with_context 调用",
                    Some(json!({"workdir": cwd, "files": req.files})),
                );
                self.state
                    .jobs()
                    .run_with_fast_fallback(self.state.clone(), prompt, cwd)
                    .await
            }
            Err(error) => format!("[context error]\n{error}"),
        }
    }

    #[tool(description = "Start a Claude coding job and return immediately with a job_id.")]
    async fn code_start(&self, Parameters(req): Parameters<CodeRequest>) -> CallToolResult {
        let cwd = match self.prepare_workdir(req.workdir, "code_start").await {
            Ok(cwd) => cwd,
            Err(error) => return CallToolResult::structured_error(json!({"error": error})),
        };
        let summary = self
            .state
            .jobs()
            .start_job(self.state.clone(), req.prompt, cwd);
        structured(summary)
    }

    #[tool(description = "Start a Claude coding job with specific files as context.")]
    async fn code_with_context_start(
        &self,
        Parameters(req): Parameters<CodeWithContextRequest>,
    ) -> CallToolResult {
        let cwd = match self
            .prepare_workdir(req.workdir, "code_with_context_start")
            .await
        {
            Ok(cwd) => cwd,
            Err(error) => return CallToolResult::structured_error(json!({"error": error})),
        };
        match build_context_prompt(&cwd, &req.files, &req.prompt).await {
            Ok(prompt) => {
                let summary = self.state.jobs().start_job(self.state.clone(), prompt, cwd);
                structured(summary)
            }
            Err(error) => CallToolResult::structured_error(json!({
                "error": error.to_string()
            })),
        }
    }

    #[tool(description = "Alias for code_start.")]
    async fn code_async(&self, Parameters(req): Parameters<CodeRequest>) -> CallToolResult {
        self.code_start(Parameters(req)).await
    }

    #[tool(description = "Alias for code_with_context_start.")]
    async fn code_with_context_async(
        &self,
        Parameters(req): Parameters<CodeWithContextRequest>,
    ) -> CallToolResult {
        self.code_with_context_start(Parameters(req)).await
    }

    #[tool(description = "Check a Claude job's status and recent output.")]
    fn code_status(&self, Parameters(req): Parameters<StatusRequest>) -> CallToolResult {
        match self
            .state
            .jobs()
            .status(&req.job_id, req.recent_chars.unwrap_or(8_000))
        {
            Some(summary) => structured(summary),
            None => CallToolResult::structured_error(json!({
                "job_id": req.job_id,
                "status": "not_found",
                "error": "Unknown job_id"
            })),
        }
    }

    #[tool(description = "Fetch the final result for a Claude job once it is complete.")]
    fn code_result(&self, Parameters(req): Parameters<ResultRequest>) -> CallToolResult {
        match self.state.jobs().result(&req.job_id) {
            Some(value) => CallToolResult::structured(value),
            None => CallToolResult::structured_error(json!({
                "job_id": req.job_id,
                "status": "not_found",
                "error": "Unknown job_id",
                "result": null
            })),
        }
    }

    #[tool(
        description = "Wait for one Claude job to finish, returning grouped job state and result."
    )]
    async fn code_wait(&self, Parameters(req): Parameters<WaitRequest>) -> CallToolResult {
        let job_ids = match validate_batch_job_ids(vec![req.job_id]) {
            Ok(job_ids) => job_ids,
            Err(error) => return CallToolResult::structured_error(json!({"error": error})),
        };
        let timeout = wait_duration(req.timeout_seconds);
        let recent_chars = req.recent_chars.unwrap_or(8_000);
        let result = self
            .state
            .jobs()
            .wait_batch_for(&job_ids, timeout, recent_chars)
            .await;
        CallToolResult::structured(result)
    }

    #[tool(description = "Wait for multiple Claude jobs to finish and return grouped results.")]
    async fn code_batch_wait(
        &self,
        Parameters(req): Parameters<BatchWaitRequest>,
    ) -> CallToolResult {
        let job_ids = match validate_batch_job_ids(req.job_ids) {
            Ok(job_ids) => job_ids,
            Err(error) => return CallToolResult::structured_error(json!({"error": error})),
        };
        let timeout = wait_duration(req.timeout_seconds);
        let recent_chars = req.recent_chars.unwrap_or(DEFAULT_BATCH_RECENT_CHARS);
        let result = self
            .state
            .jobs()
            .wait_batch_for(&job_ids, timeout, recent_chars)
            .await;
        CallToolResult::structured(result)
    }

    #[tool(description = "Fetch grouped results for multiple Claude jobs without waiting.")]
    fn code_batch_result(&self, Parameters(req): Parameters<BatchResultRequest>) -> CallToolResult {
        let job_ids = match validate_batch_job_ids(req.job_ids) {
            Ok(job_ids) => job_ids,
            Err(error) => return CallToolResult::structured_error(json!({"error": error})),
        };
        let recent_chars = req.recent_chars.unwrap_or(DEFAULT_BATCH_RECENT_CHARS);
        CallToolResult::structured(self.state.jobs().batch_result(&job_ids, recent_chars))
    }

    #[tool(
        description = "Poll multiple Claude jobs and return only newly completed, failed, cancelled, or missing results."
    )]
    async fn code_batch_poll(
        &self,
        Parameters(req): Parameters<BatchPollRequest>,
    ) -> CallToolResult {
        let job_ids = match validate_job_ids(req.job_ids, MAX_POLL_JOB_IDS) {
            Ok(job_ids) => job_ids,
            Err(error) => return CallToolResult::structured_error(json!({"error": error})),
        };
        let seen_job_ids = normalize_job_ids(req.seen_job_ids.unwrap_or_default());
        let timeout = poll_duration(req.timeout_seconds);
        let recent_chars = req.recent_chars.unwrap_or(DEFAULT_BATCH_RECENT_CHARS);
        let include_running = req.include_running.unwrap_or(false);
        let result = self
            .state
            .jobs()
            .poll_batch_for(
                &job_ids,
                &seen_job_ids,
                timeout,
                recent_chars,
                include_running,
            )
            .await;
        CallToolResult::structured(result)
    }

    #[tool(
        description = "Continue an existing Agent SDK session by job_id and return a new job_id immediately. When the user only gives a job_id or asks to continue a previous task, call code_chat_history first and read codex_context before composing the follow-up prompt."
    )]
    async fn code_continue_start(
        &self,
        Parameters(req): Parameters<ContinueStartRequest>,
    ) -> CallToolResult {
        let workdir = match prepare_optional_continue_workdir(&self.state, req.workdir).await {
            Ok(workdir) => workdir,
            Err(error) => return CallToolResult::structured_error(json!({"error": error})),
        };
        match self
            .state
            .jobs()
            .continue_job(self.state.clone(), &req.job_id, req.prompt, workdir)
        {
            Ok(summary) => structured(summary),
            Err(error) => CallToolResult::structured_error(json!({
                "job_id": req.job_id,
                "error": error
            })),
        }
    }

    #[tool(
        description = "Continue an existing Agent SDK session by job_id and wait for the new continuation job. When the user only gives a job_id or asks to continue a previous task, call code_chat_history first and read codex_context before composing the follow-up prompt."
    )]
    async fn code_continue(
        &self,
        Parameters(req): Parameters<ContinueWaitRequest>,
    ) -> CallToolResult {
        let workdir = match prepare_optional_continue_workdir(&self.state, req.workdir).await {
            Ok(workdir) => workdir,
            Err(error) => return CallToolResult::structured_error(json!({"error": error})),
        };
        let summary = match self.state.jobs().continue_job(
            self.state.clone(),
            &req.job_id,
            req.prompt,
            workdir,
        ) {
            Ok(summary) => summary,
            Err(error) => {
                return CallToolResult::structured_error(json!({
                    "job_id": req.job_id,
                    "error": error
                }))
            }
        };
        let timeout = wait_duration(req.timeout_seconds);
        let recent_chars = req.recent_chars.unwrap_or(8_000);
        let result = self
            .state
            .jobs()
            .wait_batch_for(std::slice::from_ref(&summary.job_id), timeout, recent_chars)
            .await;
        CallToolResult::structured(result)
    }

    #[tool(
        description = "Return lightweight chat history, codex_context, and resumability for a Claude job session. Use this first before code_continue_start/code_continue so Codex can see the visible history while Claude MCP resumes the hidden Agent SDK session."
    )]
    fn code_chat_history(&self, Parameters(req): Parameters<ChatHistoryRequest>) -> CallToolResult {
        match self.state.sessions().detail(&req.job_id, req.limit) {
            Some(detail) => structured(detail),
            None => CallToolResult::structured_error(json!({
                "job_id": req.job_id,
                "error": "聊天记录不存在或已被清理"
            })),
        }
    }

    #[tool(description = "Cancel a queued or running Claude job.")]
    fn code_cancel(&self, Parameters(req): Parameters<ResultRequest>) -> CallToolResult {
        match self.state.jobs().cancel(&req.job_id) {
            Some(summary) => {
                self.state.notify_runtime_stats_changed();
                structured(summary)
            }
            None => CallToolResult::structured_error(json!({
                "job_id": req.job_id,
                "status": "not_found",
                "error": "Unknown job_id"
            })),
        }
    }

    async fn prepare_workdir(
        &self,
        workdir: Option<String>,
        tool_name: &str,
    ) -> Result<PathBuf, String> {
        prepare_workdir(&self.state, workdir, tool_name).await
    }
}

#[tool_handler]
impl ServerHandler for ClaudeMcpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Use this server to delegate coding tasks to Claude without installing Claude Code CLI.")
    }
}

fn wait_duration(timeout_seconds: Option<u64>) -> std::time::Duration {
    std::time::Duration::from_secs(
        timeout_seconds
            .unwrap_or(DEFAULT_WAIT_SECONDS)
            .clamp(1, MAX_WAIT_SECONDS),
    )
}

fn poll_duration(timeout_seconds: Option<u64>) -> std::time::Duration {
    std::time::Duration::from_secs(
        timeout_seconds
            .unwrap_or(DEFAULT_POLL_SECONDS)
            .min(MAX_WAIT_SECONDS),
    )
}

fn validate_batch_job_ids(job_ids: Vec<String>) -> Result<Vec<String>, String> {
    validate_job_ids(job_ids, MAX_BATCH_JOB_IDS)
}

fn validate_job_ids(job_ids: Vec<String>, max: usize) -> Result<Vec<String>, String> {
    let job_ids = normalize_job_ids(job_ids);
    if job_ids.is_empty() {
        return Err("job_ids 不能为空".to_string());
    }
    if job_ids.len() > max {
        return Err(format!("一次最多处理 {max} 个任务"));
    }
    Ok(job_ids)
}

fn normalize_job_ids(job_ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let job_ids: Vec<String> = job_ids
        .into_iter()
        .map(|job_id| job_id.trim().to_string())
        .filter(|job_id| !job_id.is_empty())
        .filter(|job_id| seen.insert(job_id.clone()))
        .collect();
    job_ids
}

fn structured<T: serde::Serialize>(value: T) -> CallToolResult {
    CallToolResult::structured(serde_json::to_value(value).unwrap_or_else(|error| {
        json!({
            "error": error.to_string()
        })
    }))
}

fn workdir_or_current(workdir: Option<String>) -> PathBuf {
    workdir
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) async fn prepare_workdir(
    state: &AppState,
    workdir: Option<String>,
    tool_name: &str,
) -> Result<PathBuf, String> {
    let cwd = workdir_or_current(workdir);
    match fs::metadata(&cwd).await {
        Ok(metadata) if metadata.is_dir() => Ok(cwd),
        Ok(_) => {
            let error = format!("workdir 不是目录：{}", cwd.display());
            state.logs().push(
                LogLevel::Error,
                "mcp",
                None,
                None,
                error.clone(),
                Some(json!({"tool": tool_name, "workdir": cwd})),
            );
            Err(error)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(&cwd).await.map_err(|create_error| {
                format!("无法创建 workdir {}：{}", cwd.display(), create_error)
            })?;
            state.logs().push(
                LogLevel::Info,
                "mcp",
                None,
                None,
                "自动创建 workdir",
                Some(json!({"tool": tool_name, "workdir": cwd})),
            );
            Ok(cwd)
        }
        Err(error) => {
            let error = format!("无法访问 workdir {}：{}", cwd.display(), error);
            state.logs().push(
                LogLevel::Error,
                "mcp",
                None,
                None,
                error.clone(),
                Some(json!({"tool": tool_name, "workdir": cwd})),
            );
            Err(error)
        }
    }
}

async fn prepare_optional_continue_workdir(
    state: &AppState,
    workdir: Option<String>,
) -> Result<Option<PathBuf>, String> {
    match workdir {
        Some(value) if !value.trim().is_empty() => {
            prepare_workdir(state, Some(value), "code_continue")
                .await
                .map(Some)
        }
        _ => Ok(None),
    }
}

async fn build_context_prompt(
    cwd: &PathBuf,
    files: &[String],
    prompt: &str,
) -> anyhow::Result<String> {
    let mut blocks = Vec::new();
    for file in files {
        let path = {
            let candidate = PathBuf::from(file);
            if candidate.is_absolute() {
                candidate
            } else {
                cwd.join(candidate)
            }
        };
        let content = fs::read_to_string(&path).await?;
        blocks.push(format!(
            "[File: {}]\n{}\n",
            file,
            claude::truncate(&content, 80_000)
        ));
    }
    Ok(format!("{}\n\n{}", blocks.join("\n"), prompt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prepare_workdir_creates_missing_directory() {
        let state = AppState::new();
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("new-project").join("nested");

        let prepared = prepare_workdir(&state, Some(cwd.display().to_string()), "code_start")
            .await
            .unwrap();

        assert_eq!(prepared, cwd);
        assert!(prepared.is_dir());

        let page = state.logs().page(Some(LogLevel::Info), 0, 10, None);
        assert!(page
            .entries
            .iter()
            .any(|entry| entry.summary == "自动创建 workdir"));
    }

    #[tokio::test]
    async fn prepare_workdir_rejects_file_path() {
        let state = AppState::new();
        let root = tempfile::tempdir().unwrap();
        let file_path = root.path().join("not-a-directory");
        fs::write(&file_path, "content").await.unwrap();

        let error = prepare_workdir(&state, Some(file_path.display().to_string()), "code")
            .await
            .unwrap_err();

        assert!(error.contains("workdir 不是目录"));
        assert!(state.logs().page(Some(LogLevel::Error), 0, 10, None).total > 0);
    }
}
