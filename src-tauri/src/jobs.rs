use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{claude, logs::LogLevel, state::AppState};

const SYNC_WAIT_SECONDS: u64 = 90;
const DEFAULT_RECENT_CHARS: usize = 8_000;
const MAX_CAPTURED_CHARS: usize = 2_000_000;
const PROMPT_PREVIEW_CHARS: usize = 500;

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
}

#[derive(Default)]
pub struct JobStore {
    jobs: Mutex<HashMap<String, JobEntry>>,
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

impl JobStore {
    pub fn start_job(&self, state: AppState, prompt: String, cwd: PathBuf) -> JobSummary {
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
        let entry = JobEntry {
            record: record.clone(),
            cancel: cancel.clone(),
        };
        self.jobs
            .lock()
            .expect("jobs poisoned")
            .insert(job_id.clone(), entry);

        state.logs().push(
            LogLevel::Info,
            "mcp",
            None,
            Some(job_id.clone()),
            "Claude Code 任务已创建",
            Some(json!({"workdir": cwd, "prompt_chars": prompt.chars().count()})),
        );

        let spawned_job_id = job_id.clone();
        tokio::spawn(async move {
            {
                let mut job = record.lock().expect("job poisoned");
                job.status = JobStatus::Running;
                job.started_at = Some(Utc::now().timestamp_millis());
            }
            let result = claude::run_agent(
                state.clone(),
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(SYNC_WAIT_SECONDS);
        loop {
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
            if tokio::time::Instant::now() >= deadline {
                return format!(
                    "[Claude Code job is still running]\njob_id: {job_id}\nUse code_status(job_id) to check progress and code_result(job_id) to fetch the final result."
                );
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    pub fn status(&self, job_id: &str, recent_chars: usize) -> Option<JobSummary> {
        let entry = self
            .jobs
            .lock()
            .expect("jobs poisoned")
            .get(job_id)?
            .clone();
        let record = entry.record.lock().expect("job poisoned");
        Some(summary_from_record(&record, recent_chars))
    }

    pub fn result(&self, job_id: &str) -> Option<Value> {
        let summary = self.status(job_id, 0)?;
        let result = if summary.complete {
            let entry = self
                .jobs
                .lock()
                .expect("jobs poisoned")
                .get(job_id)?
                .clone();
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
        let entry = self
            .jobs
            .lock()
            .expect("jobs poisoned")
            .get(job_id)?
            .clone();
        entry.cancel.cancel();
        {
            let mut job = entry.record.lock().expect("job poisoned");
            if matches!(job.status, JobStatus::Queued | JobStatus::Running) {
                job.status = JobStatus::Cancelled;
                job.ended_at = Some(Utc::now().timestamp_millis());
                job.error = Some("任务已取消".to_string());
            }
        }
        let record = entry.record.lock().expect("job poisoned");
        Some(summary_from_record(&record, DEFAULT_RECENT_CHARS))
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
