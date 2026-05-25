use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use chrono::Utc;
use dashmap::DashMap;
use futures_util::{future::select_all, FutureExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{claude, logs::LogLevel, state::AppState};

const SYNC_WAIT_SECONDS: u64 = 90;
const DEFAULT_RECENT_CHARS: usize = 8_000;
const MAX_CAPTURED_CHARS: usize = 2_000_000;
const PROMPT_PREVIEW_CHARS: usize = 500;
const MAX_RETAINED_JOBS: usize = 20_000;
const COMPLETED_JOB_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
const JOB_CLEANUP_INTERVAL_MS: i64 = 60_000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
struct JobRecord {
    job_id: String,
    prompt: String,
    cwd: PathBuf,
    created_at: i64,
    started_at: Option<i64>,
    ended_at: Option<i64>,
    status: JobStatus,
    output: String,
    output_truncated: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct JobEntry {
    record: Arc<Mutex<JobRecord>>,
    cancel: CancellationToken,
    notify: Arc<Notify>,
}

pub struct JobStore {
    jobs: DashMap<String, JobEntry>,
    last_cleanup_ms: AtomicI64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobSummary {
    pub job_id: String,
    pub status: JobStatus,
    pub complete: bool,
    pub cwd: String,
    pub prompt_preview: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub output_recent: String,
    pub output_truncated: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct JobStoreStats {
    pub total: usize,
    pub queued: usize,
    pub running: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
}

impl Default for JobStore {
    fn default() -> Self {
        Self {
            jobs: DashMap::new(),
            last_cleanup_ms: AtomicI64::new(0),
        }
    }
}

impl JobStore {
    pub fn start_job(&self, state: AppState, prompt: String, cwd: PathBuf) -> JobSummary {
        self.cleanup_if_needed();
        let job_id = Uuid::new_v4().to_string();
        let record = Arc::new(Mutex::new(JobRecord {
            job_id: job_id.clone(),
            prompt: prompt.clone(),
            cwd: cwd.clone(),
            created_at: Utc::now().timestamp_millis(),
            started_at: None,
            ended_at: None,
            status: JobStatus::Queued,
            output: String::new(),
            output_truncated: false,
            error: None,
        }));
        let cancel = CancellationToken::new();
        let notify = Arc::new(Notify::new());
        let entry = JobEntry {
            record: record.clone(),
            cancel: cancel.clone(),
            notify: notify.clone(),
        };
        self.jobs.insert(job_id.clone(), entry);
        state.notify_runtime_stats_changed();

        state.logs().push(
            LogLevel::Info,
            "mcp",
            None,
            Some(job_id.clone()),
            "Claude Code 任务已创建",
            Some(json!({"workdir": cwd, "prompt_chars": prompt.chars().count()})),
        );

        let spawned_job_id = job_id.clone();
        let spawned_state = state.clone();
        tokio::spawn(async move {
            {
                let mut job = record.lock().expect("job poisoned");
                job.status = JobStatus::Running;
                job.started_at = Some(Utc::now().timestamp_millis());
            }
            spawned_state.notify_runtime_stats_changed();
            let result = claude::run_agent(
                spawned_state.clone(),
                prompt,
                cwd,
                spawned_job_id.clone(),
                cancel.clone(),
            )
            .await;
            let mut job = record.lock().expect("job poisoned");
            job.ended_at = Some(Utc::now().timestamp_millis());
            if cancel.is_cancelled() {
                job.status = JobStatus::Cancelled;
                job.error = Some("任务已取消".to_string());
            } else {
                match result {
                    Ok(output) => {
                        job.status = JobStatus::Succeeded;
                        append_output(&mut job, &output);
                    }
                    Err(error) => {
                        job.status = JobStatus::Failed;
                        job.error = Some(error.to_string());
                    }
                }
            }
            notify.notify_waiters();
            spawned_state.notify_runtime_stats_changed();
        });

        self.status(&job_id, 0).expect("fresh job exists")
    }

    pub async fn run_with_fast_fallback(
        &self,
        state: AppState,
        prompt: String,
        cwd: PathBuf,
    ) -> String {
        let summary = self.start_job(state, prompt, cwd);
        let job_id = summary.job_id.clone();
        let waited = self
            .wait_batch_for(
                std::slice::from_ref(&job_id),
                Duration::from_secs(SYNC_WAIT_SECONDS),
                0,
            )
            .await;
        if waited
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if let Some(result) = self.result(&job_id) {
                if result
                    .get("complete")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return result
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
        format!(
            "[Claude Code job is still running]\njob_id: {job_id}\nUse code_status(job_id) to check progress and code_result(job_id) to fetch the final result."
        )
    }

    pub fn status(&self, job_id: &str, recent_chars: usize) -> Option<JobSummary> {
        let entry = self.jobs.get(job_id)?.clone();
        let record = entry.record.lock().expect("job poisoned");
        Some(summary_from_record(&record, recent_chars))
    }

    pub fn result(&self, job_id: &str) -> Option<Value> {
        let summary = self.status(job_id, 0)?;
        let result = if summary.complete {
            let entry = self.jobs.get(job_id)?.clone();
            let job = entry.record.lock().expect("job poisoned");
            if job.status == JobStatus::Succeeded {
                Some(job.output.clone())
            } else {
                Some(format!(
                    "[Claude Code job {}]\n{}",
                    match job.status {
                        JobStatus::Cancelled => "cancelled",
                        JobStatus::Failed => "failed",
                        _ => "finished",
                    },
                    job.error.clone().unwrap_or_default()
                ))
            }
        } else {
            None
        };
        Some(json!({
            "job_id": summary.job_id,
            "status": summary.status,
            "complete": summary.complete,
            "cwd": summary.cwd,
            "prompt_preview": summary.prompt_preview,
            "created_at": summary.created_at,
            "started_at": summary.started_at,
            "ended_at": summary.ended_at,
            "output_truncated": summary.output_truncated,
            "error": summary.error,
            "result": result,
            "message": if summary.complete { "" } else { "Claude Code job is not finished yet. Use code_status(job_id) to monitor progress." }
        }))
    }

    pub fn cancel(&self, job_id: &str) -> Option<JobSummary> {
        let entry = self.jobs.get(job_id)?.clone();
        entry.cancel.cancel();
        {
            let mut job = entry.record.lock().expect("job poisoned");
            if matches!(job.status, JobStatus::Queued | JobStatus::Running) {
                job.status = JobStatus::Cancelled;
                job.ended_at = Some(Utc::now().timestamp_millis());
                job.error = Some("任务已取消".to_string());
            }
        }
        entry.notify.notify_waiters();
        let record = entry.record.lock().expect("job poisoned");
        Some(summary_from_record(&record, DEFAULT_RECENT_CHARS))
    }

    pub async fn wait_batch_for(
        &self,
        job_ids: &[String],
        timeout: Duration,
        recent_chars: usize,
    ) -> Value {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let snapshot = self.batch_result(job_ids, recent_chars);
            if snapshot
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return snapshot;
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return snapshot;
            }

            let waiters = self.incomplete_waiters(job_ids);
            if waiters.is_empty() {
                return self.batch_result(job_ids, recent_chars);
            }

            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    return self.batch_result(job_ids, recent_chars);
                }
                _ = select_all(waiters) => {}
            }
        }
    }

    pub async fn poll_batch_for(
        &self,
        job_ids: &[String],
        seen_job_ids: &[String],
        timeout: Duration,
        recent_chars: usize,
        include_running: bool,
    ) -> Value {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let snapshot =
                self.batch_poll_result(job_ids, seen_job_ids, recent_chars, include_running);
            if snapshot
                .get("ready_count")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0
                || snapshot
                    .get("complete")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                return snapshot;
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return snapshot;
            }

            let waiters = self.incomplete_waiters(job_ids);
            if waiters.is_empty() {
                return self.batch_poll_result(
                    job_ids,
                    seen_job_ids,
                    recent_chars,
                    include_running,
                );
            }

            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    return self.batch_poll_result(job_ids, seen_job_ids, recent_chars, include_running);
                }
                _ = select_all(waiters) => {}
            }
        }
    }

    pub fn batch_result(&self, job_ids: &[String], recent_chars: usize) -> Value {
        let mut completed = Vec::new();
        let mut running = Vec::new();
        let mut failed = Vec::new();
        let mut cancelled = Vec::new();
        let mut not_found = Vec::new();

        for job_id in job_ids {
            let item = self.job_item(job_id, recent_chars);
            match item.get("status").and_then(Value::as_str).unwrap_or("") {
                "succeeded" => completed.push(item),
                "failed" => failed.push(item),
                "cancelled" => cancelled.push(item),
                "not_found" => not_found.push(item),
                _ => running.push(item),
            }
        }

        let complete = running.is_empty();
        json!({
            "total": job_ids.len(),
            "complete": complete,
            "completed": completed,
            "running": running,
            "failed": failed,
            "cancelled": cancelled,
            "not_found": not_found
        })
    }

    pub fn batch_poll_result(
        &self,
        job_ids: &[String],
        seen_job_ids: &[String],
        recent_chars: usize,
        include_running: bool,
    ) -> Value {
        let job_id_set: HashSet<&str> = job_ids.iter().map(String::as_str).collect();
        let seen_set: HashSet<&str> = seen_job_ids.iter().map(String::as_str).collect();
        let mut next_seen_set = HashSet::new();
        let mut next_seen_job_ids = Vec::new();
        for job_id in seen_job_ids
            .iter()
            .filter(|job_id| job_id_set.contains(job_id.as_str()))
        {
            if next_seen_set.insert(job_id.clone()) {
                next_seen_job_ids.push(job_id.clone());
            }
        }

        let mut completed = Vec::new();
        let mut failed = Vec::new();
        let mut cancelled = Vec::new();
        let mut not_found = Vec::new();
        let mut running = Vec::new();
        let mut running_count = 0usize;
        let mut ready_count = 0usize;

        for job_id in job_ids {
            let item = self.job_item(job_id, recent_chars);
            let status = item.get("status").and_then(Value::as_str).unwrap_or("");
            let is_terminal = matches!(status, "succeeded" | "failed" | "cancelled" | "not_found");

            if is_terminal {
                if !seen_set.contains(job_id.as_str()) {
                    ready_count += 1;
                    match status {
                        "succeeded" => completed.push(item),
                        "failed" => failed.push(item),
                        "cancelled" => cancelled.push(item),
                        "not_found" => not_found.push(item),
                        _ => {}
                    }
                }
                if next_seen_set.insert(job_id.clone()) {
                    next_seen_job_ids.push(job_id.clone());
                }
            } else {
                running_count += 1;
                if include_running {
                    running.push(item);
                }
            }
        }

        let mut result = serde_json::Map::new();
        result.insert("total".to_string(), json!(job_ids.len()));
        result.insert("complete".to_string(), json!(running_count == 0));
        result.insert("running_count".to_string(), json!(running_count));
        result.insert("ready_count".to_string(), json!(ready_count));
        result.insert("completed".to_string(), json!(completed));
        result.insert("failed".to_string(), json!(failed));
        result.insert("cancelled".to_string(), json!(cancelled));
        result.insert("not_found".to_string(), json!(not_found));
        result.insert("next_seen_job_ids".to_string(), json!(next_seen_job_ids));
        if include_running {
            result.insert("running".to_string(), json!(running));
        }
        Value::Object(result)
    }

    pub fn stats(&self) -> JobStoreStats {
        let mut stats = JobStoreStats {
            total: self.jobs.len(),
            ..JobStoreStats::default()
        };
        for entry in self.jobs.iter() {
            let job = entry.record.lock().expect("job poisoned");
            match job.status {
                JobStatus::Queued => stats.queued += 1,
                JobStatus::Running => stats.running += 1,
                JobStatus::Succeeded => stats.succeeded += 1,
                JobStatus::Failed => stats.failed += 1,
                JobStatus::Cancelled => stats.cancelled += 1,
            }
        }
        stats
    }

    fn incomplete_waiters(
        &self,
        job_ids: &[String],
    ) -> Vec<futures_util::future::BoxFuture<'static, ()>> {
        let mut waiters = Vec::new();
        for job_id in job_ids {
            let Some(entry) = self.jobs.get(job_id).map(|entry| entry.clone()) else {
                continue;
            };
            let notify = entry.notify.clone();
            let waiter = async move {
                notify.notified().await;
            }
            .boxed();
            let is_incomplete = {
                let job = entry.record.lock().expect("job poisoned");
                !is_terminal(&job.status)
            };
            if is_incomplete {
                waiters.push(waiter);
            }
        }
        waiters
    }

    fn job_item(&self, job_id: &str, recent_chars: usize) -> Value {
        let Some(entry) = self.jobs.get(job_id).map(|entry| entry.clone()) else {
            return json!({
                "job_id": job_id,
                "status": "not_found",
                "complete": true,
                "cwd": null,
                "prompt_preview": "",
                "created_at": null,
                "started_at": null,
                "ended_at": null,
                "output_recent": "",
                "output_truncated": false,
                "error": "Unknown job_id",
                "result": null
            });
        };
        let job = entry.record.lock().expect("job poisoned");
        let summary = summary_from_record(&job, recent_chars);
        let result = if is_terminal(&job.status) {
            match &job.status {
                JobStatus::Succeeded => Some(job.output.clone()),
                JobStatus::Failed => Some(format!(
                    "[Claude Code job failed]\n{}",
                    job.error.clone().unwrap_or_default()
                )),
                JobStatus::Cancelled => Some(format!(
                    "[Claude Code job cancelled]\n{}",
                    job.error.clone().unwrap_or_default()
                )),
                _ => None,
            }
        } else {
            None
        };

        json!({
            "job_id": summary.job_id,
            "status": status_label(&summary.status),
            "complete": summary.complete,
            "cwd": summary.cwd,
            "prompt_preview": summary.prompt_preview,
            "created_at": summary.created_at,
            "started_at": summary.started_at,
            "ended_at": summary.ended_at,
            "output_recent": summary.output_recent,
            "output_truncated": summary.output_truncated,
            "error": summary.error,
            "result": result
        })
    }

    fn cleanup_if_needed(&self) {
        let now = Utc::now().timestamp_millis();
        let last = self.last_cleanup_ms.load(Ordering::Relaxed);
        if self.jobs.len() < MAX_RETAINED_JOBS && now.saturating_sub(last) < JOB_CLEANUP_INTERVAL_MS
        {
            return;
        }
        if self
            .last_cleanup_ms
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        self.cleanup_completed(now);
    }

    fn cleanup_completed(&self, now: i64) {
        let mut removable = Vec::new();
        for entry in self.jobs.iter() {
            let job = entry.record.lock().expect("job poisoned");
            if !matches!(
                job.status,
                JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
            ) {
                continue;
            }
            let ended_at = job.ended_at.unwrap_or(job.created_at);
            if now.saturating_sub(ended_at) >= COMPLETED_JOB_RETENTION_MS {
                removable.push((entry.key().clone(), ended_at));
            }
        }

        if self.jobs.len().saturating_sub(removable.len()) > MAX_RETAINED_JOBS {
            let mut completed = Vec::new();
            for entry in self.jobs.iter() {
                let job = entry.record.lock().expect("job poisoned");
                if matches!(
                    job.status,
                    JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
                ) {
                    completed.push((entry.key().clone(), job.ended_at.unwrap_or(job.created_at)));
                }
            }
            completed.sort_by_key(|(_, ended_at)| *ended_at);
            let target = self.jobs.len().saturating_sub(MAX_RETAINED_JOBS);
            removable.extend(completed.into_iter().take(target));
        }

        removable.sort_by_key(|(_, ended_at)| *ended_at);
        removable.dedup_by(|a, b| a.0 == b.0);
        for (job_id, _) in removable {
            self.jobs.remove(&job_id);
        }
    }
}

fn summary_from_record(job: &JobRecord, recent_chars: usize) -> JobSummary {
    JobSummary {
        job_id: job.job_id.clone(),
        status: job.status.clone(),
        complete: matches!(
            job.status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        ),
        cwd: job.cwd.display().to_string(),
        prompt_preview: truncate_chars(&job.prompt, PROMPT_PREVIEW_CHARS),
        created_at: job.created_at,
        started_at: job.started_at,
        ended_at: job.ended_at,
        output_recent: recent_chars_from(&job.output, recent_chars),
        output_truncated: job.output_truncated,
        error: job.error.clone(),
    }
}

fn append_output(job: &mut JobRecord, text: &str) {
    job.output.push_str(text);
    if job.output.chars().count() > MAX_CAPTURED_CHARS {
        job.output = recent_chars_from(&job.output, MAX_CAPTURED_CHARS);
        job.output_truncated = true;
    }
}

fn is_terminal(status: &JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    )
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

fn recent_chars_from(text: &str, recent_chars: usize) -> String {
    if recent_chars == 0 {
        return String::new();
    }
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(recent_chars))
        .collect()
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

    fn insert_job(
        store: &JobStore,
        id: &str,
        status: JobStatus,
        ended_at: Option<i64>,
    ) -> JobEntry {
        let now = Utc::now().timestamp_millis();
        let entry = JobEntry {
            record: Arc::new(Mutex::new(JobRecord {
                job_id: id.to_string(),
                prompt: "test".to_string(),
                cwd: PathBuf::from("."),
                created_at: now,
                started_at: Some(now),
                ended_at,
                status,
                output: String::new(),
                output_truncated: false,
                error: None,
            })),
            cancel: CancellationToken::new(),
            notify: Arc::new(Notify::new()),
        };
        store.jobs.insert(id.to_string(), entry.clone());
        entry
    }

    #[test]
    fn tracks_many_jobs_in_concurrent_store() {
        let store = JobStore::default();
        for index in 0..500 {
            let status = if index % 2 == 0 {
                JobStatus::Running
            } else {
                JobStatus::Succeeded
            };
            insert_job(&store, &format!("job-{index}"), status, Some(index));
        }

        let stats = store.stats();

        assert_eq!(stats.total, 500);
        assert_eq!(stats.running, 250);
        assert_eq!(stats.succeeded, 250);
    }

    #[test]
    fn cleanup_removes_old_completed_jobs_but_keeps_running_jobs() {
        let store = JobStore::default();
        let now = Utc::now().timestamp_millis();
        let old = now - COMPLETED_JOB_RETENTION_MS - 1_000;
        insert_job(&store, "old-done", JobStatus::Succeeded, Some(old));
        insert_job(&store, "old-running", JobStatus::Running, Some(old));

        store.cleanup_completed(now);

        assert!(store.status("old-done", 0).is_none());
        assert!(store.status("old-running", 0).is_some());
    }

    #[tokio::test]
    async fn wait_batch_returns_when_running_job_is_notified() {
        let store = Arc::new(JobStore::default());
        let entry = insert_job(&store, "job-1", JobStatus::Running, None);
        let record = entry.record.clone();
        let notify = entry.notify.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut job = record.lock().expect("job poisoned");
            job.status = JobStatus::Succeeded;
            job.ended_at = Some(Utc::now().timestamp_millis());
            job.output = "done".to_string();
            drop(job);
            notify.notify_waiters();
        });

        let started = tokio::time::Instant::now();
        let result = store
            .wait_batch_for(&["job-1".to_string()], Duration::from_secs(2), 100)
            .await;

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(result["complete"], true);
        assert_eq!(result["completed"][0]["result"], "done");
    }

    #[tokio::test]
    async fn wait_batch_times_out_with_running_job() {
        let store = JobStore::default();
        insert_job(&store, "job-1", JobStatus::Running, None);

        let result = store
            .wait_batch_for(&["job-1".to_string()], Duration::from_millis(20), 100)
            .await;

        assert_eq!(result["complete"], false);
        assert_eq!(result["running"][0]["job_id"], "job-1");
    }

    #[test]
    fn batch_result_groups_missing_and_terminal_jobs() {
        let store = JobStore::default();
        {
            let entry = insert_job(&store, "done", JobStatus::Succeeded, Some(1));
            entry.record.lock().expect("job poisoned").output = "ok".to_string();
        }
        {
            let entry = insert_job(&store, "failed", JobStatus::Failed, Some(2));
            entry.record.lock().expect("job poisoned").error = Some("boom".to_string());
        }
        insert_job(&store, "cancelled", JobStatus::Cancelled, Some(3));

        let result = store.batch_result(
            &[
                "done".to_string(),
                "failed".to_string(),
                "cancelled".to_string(),
                "missing".to_string(),
            ],
            100,
        );

        assert_eq!(result["complete"], true);
        assert_eq!(result["completed"][0]["result"], "ok");
        assert_eq!(result["failed"][0]["error"], "boom");
        assert_eq!(result["cancelled"][0]["status"], "cancelled");
        assert_eq!(result["not_found"][0]["job_id"], "missing");
    }

    #[tokio::test]
    async fn poll_batch_returns_unseen_completed_immediately() {
        let store = JobStore::default();
        {
            let entry = insert_job(&store, "done", JobStatus::Succeeded, Some(1));
            entry.record.lock().expect("job poisoned").output = "ok".to_string();
        }

        let started = tokio::time::Instant::now();
        let result = store
            .poll_batch_for(
                &["done".to_string()],
                &[],
                Duration::from_secs(2),
                100,
                false,
            )
            .await;

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(result["complete"], true);
        assert_eq!(result["ready_count"], 1);
        assert_eq!(result["completed"][0]["result"], "ok");
        assert_eq!(result["next_seen_job_ids"][0], "done");
        assert!(result.get("running").is_none());
    }

    #[tokio::test]
    async fn poll_batch_skips_seen_terminal_jobs() {
        let store = JobStore::default();
        insert_job(&store, "done", JobStatus::Succeeded, Some(1));

        let result = store
            .poll_batch_for(
                &["done".to_string()],
                &["done".to_string()],
                Duration::from_secs(2),
                100,
                false,
            )
            .await;

        assert_eq!(result["complete"], true);
        assert_eq!(result["ready_count"], 0);
        assert_eq!(result["completed"].as_array().unwrap().len(), 0);
        assert_eq!(result["next_seen_job_ids"][0], "done");
    }

    #[tokio::test]
    async fn poll_batch_times_out_without_new_completed_jobs() {
        let store = JobStore::default();
        insert_job(&store, "running", JobStatus::Running, None);

        let result = store
            .poll_batch_for(
                &["running".to_string()],
                &[],
                Duration::from_millis(20),
                100,
                true,
            )
            .await;

        assert_eq!(result["complete"], false);
        assert_eq!(result["ready_count"], 0);
        assert_eq!(result["running_count"], 1);
        assert_eq!(result["running"][0]["job_id"], "running");
    }

    #[tokio::test]
    async fn poll_batch_returns_when_running_job_is_notified() {
        let store = Arc::new(JobStore::default());
        let entry = insert_job(&store, "job-1", JobStatus::Running, None);
        let record = entry.record.clone();
        let notify = entry.notify.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut job = record.lock().expect("job poisoned");
            job.status = JobStatus::Succeeded;
            job.ended_at = Some(Utc::now().timestamp_millis());
            job.output = "done".to_string();
            drop(job);
            notify.notify_waiters();
        });

        let started = tokio::time::Instant::now();
        let result = store
            .poll_batch_for(
                &["job-1".to_string()],
                &[],
                Duration::from_secs(2),
                100,
                false,
            )
            .await;

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(result["complete"], true);
        assert_eq!(result["ready_count"], 1);
        assert_eq!(result["completed"][0]["result"], "done");
    }

    #[test]
    fn poll_batch_groups_failed_cancelled_and_not_found() {
        let store = JobStore::default();
        {
            let entry = insert_job(&store, "failed", JobStatus::Failed, Some(1));
            entry.record.lock().expect("job poisoned").error = Some("boom".to_string());
        }
        insert_job(&store, "cancelled", JobStatus::Cancelled, Some(2));

        let result = store.batch_poll_result(
            &[
                "failed".to_string(),
                "cancelled".to_string(),
                "missing".to_string(),
            ],
            &[],
            100,
            false,
        );

        assert_eq!(result["complete"], true);
        assert_eq!(result["ready_count"], 3);
        assert_eq!(result["failed"][0]["error"], "boom");
        assert_eq!(result["cancelled"][0]["job_id"], "cancelled");
        assert_eq!(result["not_found"][0]["job_id"], "missing");
        assert_eq!(
            result["next_seen_job_ids"],
            json!(["failed", "cancelled", "missing"])
        );
    }
}
