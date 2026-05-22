use std::path::PathBuf;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;

use crate::{claude, logs::LogLevel, state::AppState};

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
        let cwd = workdir_or_current(req.workdir);
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
        let cwd = workdir_or_current(req.workdir);
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
        let cwd = workdir_or_current(req.workdir);
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
        let cwd = workdir_or_current(req.workdir);
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

    #[tool(description = "Cancel a queued or running Claude job.")]
    fn code_cancel(&self, Parameters(req): Parameters<ResultRequest>) -> CallToolResult {
        match self.state.jobs().cancel(&req.job_id) {
            Some(summary) => structured(summary),
            None => CallToolResult::structured_error(json!({
                "job_id": req.job_id,
                "status": "not_found",
                "error": "Unknown job_id"
            })),
        }
    }
}

#[tool_handler]
impl ServerHandler for ClaudeMcpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Use this server to delegate coding tasks to Claude without installing Claude Code CLI.")
    }
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
