use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::Utc;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::jobs::JobStatus;

const CHAT_SESSIONS_FILE_NAME: &str = "chat-sessions.json";
const CHAT_SESSIONS_EVENT: &str = "chat-sessions-updated";
const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const MAX_STORED_CHARS: usize = 40_000;
const MAX_CODEX_CONTEXT_CHARS: usize = 80_000;
const PREVIEW_CHARS: usize = 140;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatJobRecord {
    pub job_id: String,
    pub parent_job_id: Option<String>,
    pub prompt: String,
    pub status: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub output: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSessionRecord {
    pub root_job_id: String,
    pub latest_job_id: String,
    pub session_id: Option<String>,
    pub workdir: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub active_job_id: Option<String>,
    pub jobs: Vec<ChatJobRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSessionSummary {
    pub root_job_id: String,
    pub latest_job_id: String,
    pub session_id: Option<String>,
    pub workdir: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub active_job_id: Option<String>,
    pub job_count: usize,
    pub title: String,
    pub resumable: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSessionDetail {
    #[serde(flatten)]
    pub summary: ChatSessionSummary,
    pub codex_context: String,
    pub jobs: Vec<ChatJobRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSessionsSnapshot {
    pub sessions: Vec<ChatSessionSummary>,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct ContinueTarget {
    pub root_job_id: String,
    pub parent_job_id: String,
    pub session_id: String,
    pub workdir: PathBuf,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ChatSessionsFile {
    #[serde(default)]
    sessions: BTreeMap<String, ChatSessionRecord>,
    #[serde(default)]
    updated_at: i64,
}

pub struct SessionStore {
    path: Option<PathBuf>,
    data: Arc<Mutex<ChatSessionsFile>>,
    app_handle: Arc<Mutex<Option<AppHandle>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        let path = chat_sessions_path().ok();
        let data = path
            .as_deref()
            .map(load_sessions_file_from)
            .unwrap_or_default();
        let store = Self {
            path,
            data: Arc::new(Mutex::new(data)),
            app_handle: Arc::new(Mutex::new(None)),
        };
        store.cleanup_expired();
        store
    }

    #[cfg(test)]
    pub fn new_for_path(path: PathBuf) -> Self {
        Self {
            data: Arc::new(Mutex::new(load_sessions_file_from(&path))),
            path: Some(path),
            app_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_app_handle(&self, app_handle: AppHandle) {
        *self
            .app_handle
            .lock()
            .expect("chat sessions app handle poisoned") = Some(app_handle);
    }

    pub fn create_root_job(
        &self,
        job_id: &str,
        prompt: &str,
        workdir: &Path,
        created_at: i64,
    ) -> i64 {
        let expires_at = expiry_from(created_at);
        let job = ChatJobRecord {
            job_id: job_id.to_string(),
            parent_job_id: None,
            prompt: clamp_text(prompt),
            status: status_label(&JobStatus::Queued).to_string(),
            created_at,
            started_at: None,
            ended_at: None,
            output: None,
            error: None,
        };
        let session = ChatSessionRecord {
            root_job_id: job_id.to_string(),
            latest_job_id: job_id.to_string(),
            session_id: None,
            workdir: workdir.display().to_string(),
            status: status_label(&JobStatus::Queued).to_string(),
            created_at,
            updated_at: created_at,
            expires_at,
            active_job_id: Some(job_id.to_string()),
            jobs: vec![job],
        };
        self.update(|data| {
            data.sessions.insert(job_id.to_string(), session);
            data.updated_at = created_at;
        });
        expires_at
    }

    pub fn begin_continuation(
        &self,
        source_job_id: &str,
        new_job_id: &str,
        prompt: &str,
        workdir_override: Option<PathBuf>,
    ) -> anyhow::Result<ContinueTarget> {
        let now = now_ms();
        let mut target = None;
        let mut update_error = None;
        self.update(|data| {
            cleanup_expired_locked(data, now);
            let Some(root_job_id) = find_root_job_id(data, source_job_id) else {
                update_error = Some(anyhow::anyhow!("找不到可续聊的 job_id：{source_job_id}"));
                return;
            };
            let Some(session) = data.sessions.get_mut(&root_job_id) else {
                update_error = Some(anyhow::anyhow!("聊天记录不存在或已被清理"));
                return;
            };
            if session.expires_at <= now {
                update_error = Some(anyhow::anyhow!("聊天记录已过期，无法继续"));
                return;
            }
            if let Some(active_job_id) = &session.active_job_id {
                update_error = Some(anyhow::anyhow!(
                    "当前 session 正在运行：{active_job_id}，请等待完成或取消后再继续"
                ));
                return;
            }
            let Some(session_id) = session.session_id.clone() else {
                update_error = Some(anyhow::anyhow!(
                    "这个 job 没有可恢复的 Agent SDK session，无法继续"
                ));
                return;
            };

            let parent_job_id = session.latest_job_id.clone();
            let workdir = workdir_override
                .clone()
                .unwrap_or_else(|| PathBuf::from(&session.workdir));
            let expires_at = expiry_from(now);
            session.latest_job_id = new_job_id.to_string();
            session.workdir = workdir.display().to_string();
            session.status = status_label(&JobStatus::Queued).to_string();
            session.updated_at = now;
            session.expires_at = expires_at;
            session.active_job_id = Some(new_job_id.to_string());
            session.jobs.push(ChatJobRecord {
                job_id: new_job_id.to_string(),
                parent_job_id: Some(parent_job_id.clone()),
                prompt: clamp_text(prompt),
                status: status_label(&JobStatus::Queued).to_string(),
                created_at: now,
                started_at: None,
                ended_at: None,
                output: None,
                error: None,
            });
            data.updated_at = now;
            target = Some(ContinueTarget {
                root_job_id,
                parent_job_id,
                session_id,
                workdir,
                expires_at,
            });
        });
        if let Some(error) = update_error {
            return Err(error);
        }
        target.ok_or_else(|| anyhow::anyhow!("无法创建续聊任务"))
    }

    pub fn mark_started(&self, job_id: &str, started_at: i64) {
        self.update_job(job_id, |session, job| {
            job.status = status_label(&JobStatus::Running).to_string();
            job.started_at = Some(started_at);
            session.status = job.status.clone();
            session.updated_at = started_at;
        });
    }

    pub fn attach_session_id(&self, job_id: &str, session_id: &str) {
        self.update_job(job_id, |session, _job| {
            session.session_id = Some(session_id.to_string());
            session.updated_at = now_ms();
        });
    }

    pub fn finish_job(
        &self,
        job_id: &str,
        status: &JobStatus,
        output: Option<&str>,
        error: Option<&str>,
        ended_at: i64,
    ) {
        self.update_job(job_id, |session, job| {
            let label = status_label(status).to_string();
            job.status = label.clone();
            job.ended_at = Some(ended_at);
            job.output = output.map(clamp_text);
            job.error = error.map(clamp_text);
            session.status = label;
            session.updated_at = ended_at;
            session.expires_at = expiry_from(ended_at);
            if session.active_job_id.as_deref() == Some(job_id) {
                session.active_job_id = None;
            }
        });
    }

    pub fn snapshot(&self) -> ChatSessionsSnapshot {
        self.cleanup_expired();
        let data = self.data.lock().expect("chat sessions poisoned");
        ChatSessionsSnapshot {
            sessions: summaries(&data),
            updated_at: data.updated_at,
        }
    }

    pub fn detail(&self, job_id: &str, limit: Option<usize>) -> Option<ChatSessionDetail> {
        self.cleanup_expired();
        let data = self.data.lock().expect("chat sessions poisoned");
        let root_job_id = find_root_job_id(&data, job_id)?;
        let session = data.sessions.get(&root_job_id)?;
        let summary = summarize(session);
        let mut jobs = session.jobs.clone();
        if let Some(limit) = limit.filter(|limit| *limit > 0) {
            let skip = jobs.len().saturating_sub(limit);
            jobs = jobs.into_iter().skip(skip).collect();
        }
        let codex_context = build_codex_context(&summary, &jobs);
        Some(ChatSessionDetail {
            summary,
            codex_context,
            jobs,
        })
    }

    pub fn delete(&self, job_id: &str) -> anyhow::Result<ChatSessionsSnapshot> {
        let mut found = false;
        let mut blocked = None;
        self.update(|data| {
            if let Some(root_job_id) = find_root_job_id(data, job_id) {
                if let Some(session) = data.sessions.get(&root_job_id) {
                    if let Some(active_job_id) = &session.active_job_id {
                        blocked = Some(active_job_id.clone());
                        found = true;
                        return;
                    }
                }
                data.sessions.remove(&root_job_id);
                data.updated_at = now_ms();
                found = true;
            }
        });
        if let Some(active_job_id) = blocked {
            anyhow::bail!("当前 session 正在运行：{active_job_id}，请完成或取消后再删除");
        }
        if !found {
            anyhow::bail!("聊天记录不存在或已被清理");
        }
        Ok(self.snapshot())
    }

    fn cleanup_expired(&self) -> bool {
        let now = now_ms();
        let snapshot = {
            let mut data = self.data.lock().expect("chat sessions poisoned");
            if !cleanup_expired_locked(&mut data, now) {
                return false;
            }
            data.updated_at = now;
            let snapshot = data.clone();
            if let Some(path) = &self.path {
                if let Err(error) = persist_sessions_file(path, &snapshot) {
                    eprintln!("failed to persist chat sessions: {error}");
                }
            }
            snapshot
        };
        self.emit_snapshot_value(&snapshot);
        true
    }

    fn update_job(
        &self,
        job_id: &str,
        update: impl FnOnce(&mut ChatSessionRecord, &mut ChatJobRecord),
    ) {
        let mut changed = false;
        self.update(|data| {
            let Some(root_job_id) = find_root_job_id(data, job_id) else {
                return;
            };
            let Some(session) = data.sessions.get_mut(&root_job_id) else {
                return;
            };
            let Some(index) = session.jobs.iter().position(|job| job.job_id == job_id) else {
                return;
            };
            let mut job = session.jobs.remove(index);
            update(session, &mut job);
            session.jobs.insert(index, job);
            data.updated_at = now_ms();
            changed = true;
        });
        if changed {
            self.emit_snapshot();
        }
    }

    fn update(&self, update: impl FnOnce(&mut ChatSessionsFile)) {
        let snapshot = {
            let mut data = self.data.lock().expect("chat sessions poisoned");
            update(&mut data);
            let snapshot = data.clone();
            if let Some(path) = &self.path {
                if let Err(error) = persist_sessions_file(path, &snapshot) {
                    eprintln!("failed to persist chat sessions: {error}");
                }
            }
            snapshot
        };
        self.emit_snapshot_value(&snapshot);
    }

    fn emit_snapshot(&self) {
        let data = self.data.lock().expect("chat sessions poisoned").clone();
        self.emit_snapshot_value(&data);
    }

    fn emit_snapshot_value(&self, data: &ChatSessionsFile) {
        let Some(handle) = self
            .app_handle
            .lock()
            .expect("chat sessions app handle poisoned")
            .clone()
        else {
            return;
        };
        let _ = handle.emit(
            CHAT_SESSIONS_EVENT,
            ChatSessionsSnapshot {
                sessions: summaries(data),
                updated_at: data.updated_at,
            },
        );
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn summaries(data: &ChatSessionsFile) -> Vec<ChatSessionSummary> {
    let mut sessions: Vec<_> = data.sessions.values().map(summarize).collect();
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions
}

fn summarize(session: &ChatSessionRecord) -> ChatSessionSummary {
    let now = now_ms();
    let blocked_reason = blocked_reason(session, now);
    ChatSessionSummary {
        root_job_id: session.root_job_id.clone(),
        latest_job_id: session.latest_job_id.clone(),
        session_id: session.session_id.clone(),
        workdir: session.workdir.clone(),
        status: session.status.clone(),
        created_at: session.created_at,
        updated_at: session.updated_at,
        expires_at: session.expires_at,
        active_job_id: session.active_job_id.clone(),
        job_count: session.jobs.len(),
        title: session
            .jobs
            .first()
            .map(|job| truncate_chars(&job.prompt, PREVIEW_CHARS))
            .unwrap_or_else(|| "未命名任务".to_string()),
        resumable: blocked_reason.is_none(),
        blocked_reason,
    }
}

fn blocked_reason(session: &ChatSessionRecord, now: i64) -> Option<String> {
    if session.expires_at <= now {
        return Some("聊天记录已过期".to_string());
    }
    if let Some(active_job_id) = &session.active_job_id {
        return Some(format!("任务运行中：{active_job_id}"));
    }
    if session.session_id.is_none() {
        return Some("没有可恢复的 Agent SDK session".to_string());
    }
    None
}

fn build_codex_context(summary: &ChatSessionSummary, jobs: &[ChatJobRecord]) -> String {
    let mut lines = Vec::new();
    lines.push("# Claude MCP 任务上下文".to_string());
    lines.push(format!("- root_job_id: {}", summary.root_job_id));
    lines.push(format!("- latest_job_id: {}", summary.latest_job_id));
    lines.push(format!("- workdir: {}", summary.workdir));
    lines.push(format!("- status: {}", summary.status));
    lines.push(format!("- resumable: {}", summary.resumable));
    lines.push(format!("- expires_at: {}", summary.expires_at));
    if let Some(session_id) = &summary.session_id {
        lines.push(format!("- session_id: {session_id}"));
    }
    if let Some(reason) = &summary.blocked_reason {
        lines.push(format!("- blocked_reason: {reason}"));
    }
    lines.push(String::new());
    lines.push("## 历史轮次".to_string());
    for (index, job) in jobs.iter().enumerate() {
        lines.push(format!(
            "### 第 {} 轮 · {} · {}",
            index + 1,
            job.status,
            job.job_id
        ));
        if let Some(parent_job_id) = &job.parent_job_id {
            lines.push(format!("- parent_job_id: {parent_job_id}"));
        }
        lines.push("- 用户要求:".to_string());
        lines.push(job.prompt.clone());
        if let Some(error) = &job.error {
            lines.push("- Claude 错误:".to_string());
            lines.push(error.clone());
        } else if let Some(output) = &job.output {
            lines.push("- Claude 回复:".to_string());
            lines.push(output.clone());
        } else {
            lines.push("- Claude 回复: 任务尚未完成。".to_string());
        }
        lines.push(String::new());
    }
    lines.push("## Codex 续聊建议".to_string());
    lines.push(
        "如果用户要求继续修改，请先阅读以上历史，再调用 code_continue_start 或 code_continue；prompt 只写新的修改要求，不需要把完整历史重复塞回去。"
            .to_string(),
    );
    truncate_chars(&lines.join("\n"), MAX_CODEX_CONTEXT_CHARS)
}

fn find_root_job_id(data: &ChatSessionsFile, job_id: &str) -> Option<String> {
    if data.sessions.contains_key(job_id) {
        return Some(job_id.to_string());
    }
    data.sessions
        .iter()
        .find(|(_, session)| session.jobs.iter().any(|job| job.job_id == job_id))
        .map(|(root_job_id, _)| root_job_id.clone())
}

fn cleanup_expired_locked(data: &mut ChatSessionsFile, now: i64) -> bool {
    let before = data.sessions.len();
    data.sessions
        .retain(|_, session| session.expires_at > now || session.active_job_id.is_some());
    before != data.sessions.len()
}

fn chat_sessions_path() -> anyhow::Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "zoe", "cclaude-mcp")
        .ok_or_else(|| anyhow::anyhow!("无法找到配置目录"))?;
    Ok(dirs.config_dir().join(CHAT_SESSIONS_FILE_NAME))
}

fn load_sessions_file_from(path: &Path) -> ChatSessionsFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<ChatSessionsFile>(&raw).ok())
        .unwrap_or_default()
}

fn persist_sessions_file(path: &Path, data: &ChatSessionsFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(data)?)?;
    restrict_session_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_session_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_session_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn status_label(status: &JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::Succeeded => "succeeded",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn expiry_from(timestamp_ms: i64) -> i64 {
    timestamp_ms.saturating_add(RETENTION_MS)
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn clamp_text(text: &str) -> String {
    truncate_chars(text, MAX_STORED_CHARS)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut output: String = text.chars().take(max_chars).collect();
    output.push_str("...");
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_root_and_attaches_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new_for_path(dir.path().join("chat-sessions.json"));
        let now = now_ms();
        store.create_root_job("job-1", "hello", Path::new("/tmp/project"), now);
        store.attach_session_id("job-1", "session-1");

        let detail = store.detail("job-1", None).unwrap();
        assert_eq!(detail.summary.session_id.as_deref(), Some("session-1"));
        assert_eq!(detail.summary.root_job_id, "job-1");
        assert_eq!(detail.jobs.len(), 1);
    }

    #[test]
    fn rejects_continuation_without_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new_for_path(dir.path().join("chat-sessions.json"));
        let now = now_ms();
        store.create_root_job("job-1", "hello", Path::new("/tmp/project"), now);
        store.finish_job("job-1", &JobStatus::Succeeded, Some("done"), None, now + 1);

        let error = store
            .begin_continuation("job-1", "job-2", "continue", None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("没有可恢复"));
    }

    #[test]
    fn creates_continuation_and_blocks_parallel_same_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new_for_path(dir.path().join("chat-sessions.json"));
        let now = now_ms();
        store.create_root_job("job-1", "hello", Path::new("/tmp/project"), now);
        store.attach_session_id("job-1", "session-1");
        store.finish_job("job-1", &JobStatus::Succeeded, Some("done"), None, now + 1);

        let target = store
            .begin_continuation("job-1", "job-2", "continue", None)
            .unwrap();
        assert_eq!(target.session_id, "session-1");
        assert_eq!(target.root_job_id, "job-1");
        assert_eq!(target.parent_job_id, "job-1");

        let error = store
            .begin_continuation("job-1", "job-3", "again", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("正在运行"));
    }

    #[test]
    fn persists_and_loads_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat-sessions.json");
        {
            let store = SessionStore::new_for_path(path.clone());
            store.create_root_job("job-1", "hello", Path::new("/tmp/project"), now_ms());
            store.attach_session_id("job-1", "session-1");
        }

        let loaded = SessionStore::new_for_path(path);
        assert_eq!(
            loaded.detail("job-1", None).unwrap().summary.session_id,
            Some("session-1".to_string())
        );
    }

    #[test]
    fn detail_includes_codex_visible_context() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new_for_path(dir.path().join("chat-sessions.json"));
        let now = now_ms();
        store.create_root_job("job-1", "first prompt", Path::new("/tmp/project"), now);
        store.attach_session_id("job-1", "session-1");
        store.finish_job(
            "job-1",
            &JobStatus::Succeeded,
            Some("first answer"),
            None,
            now + 1,
        );

        let detail = store.detail("job-1", None).unwrap();

        assert!(detail.codex_context.contains("root_job_id: job-1"));
        assert!(detail.codex_context.contains("workdir: /tmp/project"));
        assert!(detail.codex_context.contains("first prompt"));
        assert!(detail.codex_context.contains("first answer"));
        assert!(detail.codex_context.contains("code_continue_start"));
    }

    #[test]
    fn delete_rejects_active_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new_for_path(dir.path().join("chat-sessions.json"));
        store.create_root_job("job-1", "hello", Path::new("/tmp/project"), now_ms());

        let error = store.delete("job-1").unwrap_err().to_string();

        assert!(error.contains("正在运行"));
        assert!(store.detail("job-1", None).is_some());
    }
}
