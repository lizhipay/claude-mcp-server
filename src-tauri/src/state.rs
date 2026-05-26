use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::Client;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{
    agent_bridge::AgentBridge, jobs::JobStore, logs::LogStore, server::ServerController,
    token_usage::TokenUsageStore,
};

const RUNTIME_STATS_EVENT: &str = "runtime-stats-updated";
const RUNTIME_STATS_EMIT_MS: u64 = 250;
const HTTP_CONNECT_TIMEOUT_SECONDS: u64 = 15;
const HTTP_POOL_MAX_IDLE_PER_HOST: usize = 512;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub logs: LogStore,
    pub jobs: JobStore,
    pub server: ServerController,
    pub token_usage: TokenUsageStore,
    pub agent_bridge: AgentBridge,
    pub http: Client,
    pub runtime: RuntimeState,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeStatsSnapshot {
    pub total_jobs: usize,
    pub queued_jobs: usize,
    pub running_jobs: usize,
    pub succeeded_jobs: usize,
    pub failed_jobs: usize,
    pub cancelled_jobs: usize,
    pub active_upstream_requests: usize,
    pub logs_retained: usize,
    pub logs_dropped: usize,
    pub logs_pending: usize,
    pub token_pending: usize,
    pub token_updated_at: Option<String>,
}

pub struct RuntimeState {
    active_upstream_requests: AtomicUsize,
    last_emit_ms: Arc<AtomicU64>,
    emit_pending: Arc<AtomicBool>,
    app_handle: Mutex<Option<AppHandle>>,
}

pub struct UpstreamRequestGuard {
    state: AppState,
    active: bool,
}

impl AppState {
    pub fn new() -> Self {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECONDS))
            .pool_max_idle_per_host(HTTP_POOL_MAX_IDLE_PER_HOST)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            inner: Arc::new(AppStateInner {
                logs: LogStore::default(),
                jobs: JobStore::default(),
                server: ServerController::default(),
                token_usage: TokenUsageStore::default(),
                agent_bridge: AgentBridge::default(),
                http,
                runtime: RuntimeState::default(),
            }),
        }
    }

    pub fn logs(&self) -> &LogStore {
        &self.inner.logs
    }

    pub fn jobs(&self) -> &JobStore {
        &self.inner.jobs
    }

    pub fn server(&self) -> &ServerController {
        &self.inner.server
    }

    pub fn token_usage(&self) -> &TokenUsageStore {
        &self.inner.token_usage
    }

    pub fn agent_bridge(&self) -> &AgentBridge {
        &self.inner.agent_bridge
    }

    pub fn http(&self) -> &Client {
        &self.inner.http
    }

    pub fn begin_upstream_request(&self) -> UpstreamRequestGuard {
        self.inner
            .runtime
            .active_upstream_requests
            .fetch_add(1, Ordering::Relaxed);
        self.notify_runtime_stats_changed();
        UpstreamRequestGuard {
            state: self.clone(),
            active: true,
        }
    }

    pub fn active_upstream_requests(&self) -> usize {
        self.inner
            .runtime
            .active_upstream_requests
            .load(Ordering::Relaxed)
    }

    pub fn runtime_stats(&self) -> RuntimeStatsSnapshot {
        let job_stats = self.jobs().stats();
        let log_stats = self.logs().fast_stats();
        let usage = self.token_usage().snapshot();
        RuntimeStatsSnapshot {
            total_jobs: job_stats.total,
            queued_jobs: job_stats.queued,
            running_jobs: job_stats.running,
            succeeded_jobs: job_stats.succeeded,
            failed_jobs: job_stats.failed,
            cancelled_jobs: job_stats.cancelled,
            active_upstream_requests: self.active_upstream_requests(),
            logs_retained: log_stats.total,
            logs_dropped: log_stats.dropped,
            logs_pending: self.logs().pending_len(),
            token_pending: self.token_usage().pending_count(),
            token_updated_at: usage.updated_at,
        }
    }

    pub fn notify_runtime_stats_changed(&self) {
        self.inner.runtime.schedule_event();
    }

    pub fn set_app_handle(&self, app_handle: AppHandle) {
        self.inner.logs.set_app_handle(app_handle.clone());
        self.inner.token_usage.set_app_handle(app_handle.clone());
        self.inner.runtime.set_app_handle(app_handle);
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            active_upstream_requests: AtomicUsize::new(0),
            last_emit_ms: Arc::new(AtomicU64::new(0)),
            emit_pending: Arc::new(AtomicBool::new(false)),
            app_handle: Mutex::new(None),
        }
    }
}

impl RuntimeState {
    fn set_app_handle(&self, app_handle: AppHandle) {
        *self.app_handle.lock().expect("runtime app handle poisoned") = Some(app_handle);
    }

    fn schedule_event(&self) {
        let Some(handle) = self
            .app_handle
            .lock()
            .expect("runtime app handle poisoned")
            .clone()
        else {
            return;
        };

        let now = current_millis();
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= RUNTIME_STATS_EMIT_MS {
            self.last_emit_ms.store(now, Ordering::Relaxed);
            let _ = handle.emit(RUNTIME_STATS_EVENT, ());
            return;
        }

        if self.emit_pending.swap(true, Ordering::AcqRel) {
            return;
        }

        let pending = self.emit_pending.clone();
        let last_emit = self.last_emit_ms.clone();
        let delay = RUNTIME_STATS_EMIT_MS.saturating_sub(now.saturating_sub(last));
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay.max(1))).await;
            last_emit.store(current_millis(), Ordering::Relaxed);
            pending.store(false, Ordering::Release);
            let _ = handle.emit(RUNTIME_STATS_EVENT, ());
        });
    }
}

impl Drop for UpstreamRequestGuard {
    fn drop(&mut self) {
        if self.active {
            self.state
                .inner
                .runtime
                .active_upstream_requests
                .fetch_sub(1, Ordering::Relaxed);
            self.state.notify_runtime_stats_changed();
        }
    }
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_request_tracking_does_not_limit_concurrency() {
        let state = AppState::new();
        let guards: Vec<_> = (0..300).map(|_| state.begin_upstream_request()).collect();

        assert_eq!(state.active_upstream_requests(), 300);

        drop(guards);
        assert_eq!(state.active_upstream_requests(), 0);
    }
}
