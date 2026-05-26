use std::{
    env,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    sync::{oneshot, Mutex as AsyncMutex},
};
use tokio_util::sync::CancellationToken;

use crate::{claude, logs::LogLevel, state::AppState};

#[derive(Debug, Clone)]
pub struct SdkRuntimeConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentRuntimeStatus {
    pub runtime: String,
    pub bridge_started: bool,
    pub sdk_version: Option<String>,
    pub native_binary_path: Option<String>,
    pub bridge_script: Option<String>,
    pub node_executable: String,
    pub active_sessions: usize,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct AgentBridge {
    inner: Arc<AgentBridgeInner>,
}

struct AgentBridgeInner {
    process: AsyncMutex<Option<BridgeProcess>>,
    pending: DashMap<String, PendingJob>,
    sdk_version: Mutex<Option<String>>,
    native_binary_path: Mutex<Option<String>>,
    bridge_script: Mutex<Option<String>>,
    node_executable: Mutex<String>,
    last_error: Mutex<Option<String>>,
    active_sessions: AtomicUsize,
}

struct BridgeProcess {
    stdin: Arc<AsyncMutex<ChildStdin>>,
}

struct PendingJob {
    tx: Mutex<Option<oneshot::Sender<anyhow::Result<String>>>>,
}

#[derive(Debug, Deserialize)]
struct BridgeEvent {
    #[serde(rename = "type")]
    kind: String,
    job_id: Option<String>,
    request_id: Option<String>,
    level: Option<String>,
    source: Option<String>,
    summary: Option<String>,
    detail: Option<Value>,
    output: Option<String>,
    error: Option<String>,
    usage: Option<Value>,
    session_id: Option<String>,
    sdk_version: Option<String>,
    native_binary_path: Option<String>,
    active_jobs: Option<usize>,
    node: Option<String>,
    platform: Option<String>,
    arch: Option<String>,
}

impl Default for AgentBridge {
    fn default() -> Self {
        Self {
            inner: Arc::new(AgentBridgeInner {
                process: AsyncMutex::new(None),
                pending: DashMap::new(),
                sdk_version: Mutex::new(None),
                native_binary_path: Mutex::new(None),
                bridge_script: Mutex::new(None),
                node_executable: Mutex::new(node_executable()),
                last_error: Mutex::new(None),
                active_sessions: AtomicUsize::new(0),
            }),
        }
    }
}

impl AgentBridge {
    pub async fn run_job(
        &self,
        state: AppState,
        job_id: String,
        prompt: String,
        cwd: PathBuf,
        runtime: SdkRuntimeConfig,
        cancel: CancellationToken,
    ) -> anyhow::Result<String> {
        self.ensure_started(&state).await?;
        let (tx, rx) = oneshot::channel();
        self.inner.pending.insert(
            job_id.clone(),
            PendingJob {
                tx: Mutex::new(Some(tx)),
            },
        );
        self.inner.active_sessions.fetch_add(1, Ordering::Relaxed);

        let send_result = self
            .send(json!({
                "type": "start",
                "job_id": job_id.clone(),
                "prompt": prompt,
                "cwd": cwd,
                "api_key": runtime.api_key,
                "base_url": runtime.base_url,
                "model": runtime.model,
            }))
            .await;
        if let Err(error) = send_result {
            self.inner.pending.remove(&job_id);
            self.inner.active_sessions.fetch_sub(1, Ordering::Relaxed);
            return Err(error);
        }

        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = self.send(json!({"type": "cancel", "job_id": job_id})).await;
                self.inner.pending.remove(&job_id);
                self.inner.active_sessions.fetch_sub(1, Ordering::Relaxed);
                anyhow::bail!("任务已取消");
            }
            result = rx => {
                self.inner.active_sessions.fetch_sub(1, Ordering::Relaxed);
                match result {
                    Ok(result) => result,
                    Err(_) => anyhow::bail!("Agent SDK bridge 没有返回任务结果"),
                }
            }
        }
    }

    pub fn status(&self, runtime: impl Into<String>) -> AgentRuntimeStatus {
        AgentRuntimeStatus {
            runtime: runtime.into(),
            bridge_started: self
                .inner
                .process
                .try_lock()
                .map(|process| process.is_some())
                .unwrap_or(false),
            sdk_version: self
                .inner
                .sdk_version
                .lock()
                .expect("sdk version poisoned")
                .clone(),
            native_binary_path: self
                .inner
                .native_binary_path
                .lock()
                .expect("native path poisoned")
                .clone(),
            bridge_script: self
                .inner
                .bridge_script
                .lock()
                .expect("bridge script poisoned")
                .clone(),
            node_executable: self
                .inner
                .node_executable
                .lock()
                .expect("node executable poisoned")
                .clone(),
            active_sessions: self.inner.active_sessions.load(Ordering::Relaxed),
            last_error: self
                .inner
                .last_error
                .lock()
                .expect("last error poisoned")
                .clone(),
        }
    }

    async fn ensure_started(&self, state: &AppState) -> anyhow::Result<()> {
        let mut process = self.inner.process.lock().await;
        if process.is_some() {
            return Ok(());
        }

        let script = locate_bridge_script()?;
        let node = node_executable();
        *self
            .inner
            .bridge_script
            .lock()
            .expect("bridge script poisoned") = Some(script.display().to_string());
        *self
            .inner
            .node_executable
            .lock()
            .expect("node executable poisoned") = node.clone();

        let mut child = Command::new(&node)
            .arg(&script)
            .current_dir(
                script
                    .parent()
                    .and_then(|parent| parent.parent())
                    .unwrap_or_else(|| {
                        script.parent().unwrap_or_else(|| std::path::Path::new("."))
                    }),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                anyhow::anyhow!(
                    "无法启动 Agent SDK bridge（需要可用的 Node.js 18+）：{}",
                    error
                )
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("无法打开 Agent SDK bridge stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("无法打开 Agent SDK bridge stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("无法打开 Agent SDK bridge stderr"))?;

        *process = Some(BridgeProcess {
            stdin: Arc::new(AsyncMutex::new(stdin)),
        });

        state.logs().push(
            LogLevel::Info,
            "agent-bridge",
            None,
            None,
            "Agent SDK bridge 已启动",
            Some(json!({"script": script, "node": node})),
        );

        let read_state = state.clone();
        let read_inner = self.inner.clone();
        tokio::spawn(async move {
            read_stdout_loop(read_state, read_inner, stdout).await;
        });

        let err_state = state.clone();
        tokio::spawn(async move {
            read_stderr_loop(err_state, stderr).await;
        });

        let wait_state = state.clone();
        let wait_inner = self.inner.clone();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    let summary = if status.success() {
                        "Agent SDK bridge 已退出"
                    } else {
                        "Agent SDK bridge 异常退出"
                    };
                    wait_state.logs().push(
                        if status.success() {
                            LogLevel::Info
                        } else {
                            LogLevel::Error
                        },
                        "agent-bridge",
                        None,
                        None,
                        summary,
                        Some(json!({"status": status.code()})),
                    );
                    if !status.success() {
                        fail_all_pending(&wait_inner, "Agent SDK bridge 异常退出");
                    }
                }
                Err(error) => {
                    set_last_error(&wait_inner, error.to_string());
                    wait_state.logs().push(
                        LogLevel::Error,
                        "agent-bridge",
                        None,
                        None,
                        "Agent SDK bridge 等待失败",
                        Some(json!({"error": error.to_string()})),
                    );
                    fail_all_pending(&wait_inner, "Agent SDK bridge 等待失败");
                }
            }
            *wait_inner.process.lock().await = None;
        });

        Ok(())
    }

    async fn send(&self, value: Value) -> anyhow::Result<()> {
        let stdin = {
            let process = self.inner.process.lock().await;
            process
                .as_ref()
                .map(|process| process.stdin.clone())
                .ok_or_else(|| anyhow::anyhow!("Agent SDK bridge 尚未启动"))?
        };
        let mut stdin = stdin.lock().await;
        let mut line = serde_json::to_vec(&value)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }
}

async fn read_stdout_loop(
    state: AppState,
    inner: Arc<AgentBridgeInner>,
    stdout: tokio::process::ChildStdout,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<BridgeEvent>(&line) {
            Ok(event) => handle_bridge_event(&state, &inner, event),
            Err(error) => {
                state.logs().push(
                    LogLevel::Warn,
                    "agent-bridge",
                    None,
                    None,
                    "Agent SDK bridge 输出无法解析",
                    Some(json!({"error": error.to_string(), "line": line})),
                );
            }
        }
    }
}

async fn read_stderr_loop(state: AppState, stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        state.logs().push(
            LogLevel::Warn,
            "agent-bridge",
            None,
            None,
            "Agent SDK bridge stderr",
            Some(json!({"stderr": line})),
        );
    }
}

fn handle_bridge_event(state: &AppState, inner: &Arc<AgentBridgeInner>, event: BridgeEvent) {
    match event.kind.as_str() {
        "ready" | "status_response" => {
            update_bridge_metadata(inner, &event);
            state.logs().push(
                LogLevel::Info,
                "agent-bridge",
                event.request_id.clone(),
                event.job_id.clone(),
                if event.kind == "ready" {
                    "Agent SDK bridge 就绪"
                } else {
                    "Agent SDK bridge 状态已更新"
                },
                Some(json!({
                    "sdk_version": event.sdk_version,
                    "native_binary_path": event.native_binary_path,
                    "active_jobs": event.active_jobs,
                    "node": event.node,
                    "platform": event.platform,
                    "arch": event.arch
                })),
            );
        }
        "started" | "init" | "log" | "stream_summary" => {
            state.logs().push(
                parse_level(event.level.as_deref()).unwrap_or(LogLevel::Info),
                event.source.as_deref().unwrap_or("agent-sdk"),
                None,
                event.job_id.clone(),
                event
                    .summary
                    .unwrap_or_else(|| "Agent SDK 事件".to_string()),
                event.detail,
            );
        }
        "permission_denied" => {
            state.logs().push(
                LogLevel::Warn,
                "permission",
                None,
                event.job_id.clone(),
                event
                    .summary
                    .unwrap_or_else(|| "Agent SDK 权限已拒绝".to_string()),
                event.detail,
            );
        }
        "usage" => {
            if let Some(usage) = event.usage {
                claude::record_external_token_usage(state, &usage, None, event.job_id.clone());
            }
            state.logs().push(
                LogLevel::Debug,
                "agent-sdk",
                None,
                event.job_id.clone(),
                "Agent SDK usage 已记录",
                event.detail,
            );
        }
        "done" => {
            complete_pending(
                inner,
                event.job_id.as_deref(),
                Ok(event.output.unwrap_or_default()),
            );
            state.logs().push(
                LogLevel::Info,
                "agent-sdk",
                None,
                event.job_id.clone(),
                event
                    .summary
                    .unwrap_or_else(|| "Agent SDK 任务完成".to_string()),
                Some(json!({"session_id": event.session_id, "detail": event.detail})),
            );
            state.notify_runtime_stats_changed();
        }
        "cancelled" => {
            complete_pending(
                inner,
                event.job_id.as_deref(),
                Err(anyhow::anyhow!(
                    "{}",
                    event.error.unwrap_or_else(|| "任务已取消".to_string())
                )),
            );
            state.logs().push(
                LogLevel::Warn,
                "agent-sdk",
                None,
                event.job_id.clone(),
                "Agent SDK 任务已取消",
                event.detail,
            );
            state.notify_runtime_stats_changed();
        }
        "error" | "bridge_error" => {
            let error = event
                .error
                .clone()
                .unwrap_or_else(|| "Agent SDK bridge 错误".to_string());
            set_last_error(inner, error.clone());
            complete_pending(
                inner,
                event.job_id.as_deref(),
                Err(anyhow::anyhow!("{}", error)),
            );
            state.logs().push(
                LogLevel::Error,
                event.source.as_deref().unwrap_or("agent-sdk"),
                None,
                event.job_id.clone(),
                event
                    .summary
                    .unwrap_or_else(|| "Agent SDK 执行失败".to_string()),
                Some(json!({"error": error, "detail": event.detail})),
            );
            state.notify_runtime_stats_changed();
        }
        _ => {
            state.logs().push(
                LogLevel::Debug,
                "agent-bridge",
                None,
                event.job_id.clone(),
                format!("Agent SDK bridge 未识别事件：{}", event.kind),
                event.detail,
            );
        }
    }
}

fn complete_pending(
    inner: &Arc<AgentBridgeInner>,
    job_id: Option<&str>,
    result: anyhow::Result<String>,
) {
    let Some(job_id) = job_id else {
        return;
    };
    if let Some((_, pending)) = inner.pending.remove(job_id) {
        if let Some(tx) = pending.tx.lock().expect("pending tx poisoned").take() {
            let _ = tx.send(result);
        }
    }
}

fn fail_all_pending(inner: &Arc<AgentBridgeInner>, message: &str) {
    let job_ids: Vec<String> = inner
        .pending
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    for job_id in job_ids {
        complete_pending(inner, Some(&job_id), Err(anyhow::anyhow!("{}", message)));
    }
}

fn update_bridge_metadata(inner: &Arc<AgentBridgeInner>, event: &BridgeEvent) {
    if let Some(version) = &event.sdk_version {
        *inner.sdk_version.lock().expect("sdk version poisoned") = Some(version.clone());
    }
    if let Some(path) = &event.native_binary_path {
        *inner
            .native_binary_path
            .lock()
            .expect("native path poisoned") = Some(path.clone());
    }
}

fn set_last_error(inner: &Arc<AgentBridgeInner>, error: String) {
    *inner.last_error.lock().expect("last error poisoned") = Some(error);
}

fn parse_level(level: Option<&str>) -> Option<LogLevel> {
    match level {
        Some("debug") => Some(LogLevel::Debug),
        Some("info") => Some(LogLevel::Info),
        Some("warn") => Some(LogLevel::Warn),
        Some("error") => Some(LogLevel::Error),
        _ => None,
    }
}

fn node_executable() -> String {
    env::var("CLAUDE_MCP_NODE").unwrap_or_else(|_| "node".to_string())
}

fn locate_bridge_script() -> anyhow::Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(current_dir) = env::current_dir() {
        candidates.push(current_dir.join("agent-bridge/bridge.mjs"));
        candidates.push(current_dir.join("../agent-bridge/bridge.mjs"));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join("agent-bridge/bridge.mjs"));
            candidates.push(exe_dir.join("resources/agent-bridge/bridge.mjs"));
            if let Some(contents_dir) = exe_dir.parent() {
                candidates.push(contents_dir.join("Resources/agent-bridge/bridge.mjs"));
            }
        }
    }

    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow::anyhow!("找不到 Agent SDK bridge 脚本"))
}
