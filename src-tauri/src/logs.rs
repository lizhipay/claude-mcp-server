use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter};

const MAX_LOGS: usize = 1_200_000;
const MAX_LOG_PAGE_LIMIT: usize = 500;
const LOG_STATS_EVENT: &str = "log-stats-updated";
const LOG_STATS_EMIT_MS: u64 = 100;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: u64,
    pub time: String,
    pub level: LogLevel,
    pub source: String,
    pub request_id: Option<String>,
    pub task_id: Option<String>,
    pub summary: String,
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogListEntry {
    pub id: u64,
    pub time: String,
    pub level: LogLevel,
    pub source: String,
    pub request_id: Option<String>,
    pub task_id: Option<String>,
    pub summary: String,
    pub has_detail: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogStats {
    pub total: usize,
    pub dropped: usize,
    pub debug: usize,
    pub info: usize,
    pub warn: usize,
    pub error: usize,
    pub latest_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPage {
    pub entries: Vec<LogListEntry>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub dropped: usize,
    pub latest_id: Option<u64>,
}

pub struct LogStore {
    inner: Mutex<LogInner>,
    dropped: AtomicUsize,
    next_id: AtomicU64,
    last_stats_emit_ms: Arc<AtomicU64>,
    stats_emit_pending: Arc<AtomicBool>,
    app_handle: Mutex<Option<AppHandle>>,
}

#[derive(Default)]
struct LogInner {
    entries: VecDeque<LogEntry>,
    debug_ids: VecDeque<u64>,
    info_ids: VecDeque<u64>,
    warn_ids: VecDeque<u64>,
    error_ids: VecDeque<u64>,
}

impl Default for LogStore {
    fn default() -> Self {
        Self {
            inner: Mutex::new(LogInner::default()),
            dropped: AtomicUsize::new(0),
            next_id: AtomicU64::new(0),
            last_stats_emit_ms: Arc::new(AtomicU64::new(0)),
            stats_emit_pending: Arc::new(AtomicBool::new(false)),
            app_handle: Mutex::new(None),
        }
    }
}

impl LogStore {
    pub fn set_app_handle(&self, app_handle: AppHandle) {
        *self.app_handle.lock().expect("log app handle poisoned") = Some(app_handle);
    }

    pub fn push(
        &self,
        level: LogLevel,
        source: impl Into<String>,
        request_id: Option<String>,
        task_id: Option<String>,
        summary: impl Into<String>,
        detail: Option<Value>,
    ) -> LogEntry {
        let entry = LogEntry {
            id: self.next_id.fetch_add(1, Ordering::Relaxed) + 1,
            time: Local::now().format("%H:%M:%S%.3f").to_string(),
            level,
            source: source.into(),
            request_id,
            task_id,
            summary: summary.into(),
            detail: detail.map(redact_value),
        };
        {
            let mut inner = self.inner.lock().expect("logs poisoned");
            if inner.entries.len() >= MAX_LOGS {
                if let Some(removed) = inner.entries.pop_front() {
                    inner.remove_level_id(removed.level, removed.id);
                }
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            inner.add_level_id(entry.level, entry.id);
            inner.entries.push_back(entry.clone());
        }
        self.schedule_stats_event();
        entry
    }

    pub fn clear_stats(&self) -> LogStats {
        *self.inner.lock().expect("logs poisoned") = LogInner::default();
        self.dropped.store(0, Ordering::Relaxed);
        self.emit_stats_event_now();
        self.stats()
    }

    pub fn stats(&self) -> LogStats {
        self.inner
            .lock()
            .expect("logs poisoned")
            .stats(self.dropped.load(Ordering::Relaxed))
    }

    pub fn page(&self, level: Option<LogLevel>, offset: usize, limit: usize) -> LogPage {
        let inner = self.inner.lock().expect("logs poisoned");
        let total = inner.total_for_level(level);
        let offset = offset.min(total);
        let limit = limit.min(MAX_LOG_PAGE_LIMIT);
        let end = offset.saturating_add(limit).min(total);
        let entries = inner.page(level, offset, end);

        LogPage {
            entries,
            total,
            offset,
            limit,
            dropped: self.dropped.load(Ordering::Relaxed),
            latest_id: inner.entries.back().map(|entry| entry.id),
        }
    }

    pub fn detail(&self, id: u64) -> Option<LogEntry> {
        self.inner
            .lock()
            .expect("logs poisoned")
            .entry_by_id(id)
            .cloned()
    }

    fn schedule_stats_event(&self) {
        let Some(handle) = self
            .app_handle
            .lock()
            .expect("log app handle poisoned")
            .clone()
        else {
            return;
        };

        let now = current_millis();
        let last = self.last_stats_emit_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= LOG_STATS_EMIT_MS {
            self.last_stats_emit_ms.store(now, Ordering::Relaxed);
            let _ = handle.emit(LOG_STATS_EVENT, ());
            return;
        }

        if self.stats_emit_pending.swap(true, Ordering::AcqRel) {
            return;
        }

        let delay = LOG_STATS_EMIT_MS.saturating_sub(now.saturating_sub(last));
        let pending = self.stats_emit_pending.clone();
        let last_emit = self.last_stats_emit_ms.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay.max(1))).await;
            last_emit.store(current_millis(), Ordering::Relaxed);
            pending.store(false, Ordering::Release);
            let _ = handle.emit(LOG_STATS_EVENT, ());
        });
    }

    fn emit_stats_event_now(&self) {
        self.last_stats_emit_ms
            .store(current_millis(), Ordering::Relaxed);
        if let Some(handle) = self
            .app_handle
            .lock()
            .expect("log app handle poisoned")
            .clone()
        {
            let _ = handle.emit(LOG_STATS_EVENT, ());
        }
    }
}

impl LogInner {
    fn add_level_id(&mut self, level: LogLevel, id: u64) {
        self.ids_mut(level).push_back(id);
    }

    fn remove_level_id(&mut self, level: LogLevel, id: u64) {
        let removed = self.ids_mut(level).pop_front();
        debug_assert_eq!(removed, Some(id));
    }

    fn total_for_level(&self, level: Option<LogLevel>) -> usize {
        match level {
            Some(level) => self.ids(level).len(),
            None => self.entries.len(),
        }
    }

    fn page(&self, level: Option<LogLevel>, offset: usize, end: usize) -> Vec<LogListEntry> {
        match level {
            None => self
                .entries
                .range(offset..end)
                .map(LogListEntry::from)
                .collect(),
            Some(level) => self
                .ids(level)
                .range(offset..end)
                .filter_map(|id| self.entry_by_id(*id))
                .map(LogListEntry::from)
                .collect(),
        }
    }

    fn entry_by_id(&self, id: u64) -> Option<&LogEntry> {
        let first_id = self.entries.front()?.id;
        let index = id.checked_sub(first_id)? as usize;
        self.entries.get(index).filter(|entry| entry.id == id)
    }

    fn stats(&self, dropped: usize) -> LogStats {
        LogStats {
            total: self.entries.len(),
            dropped,
            debug: self.debug_ids.len(),
            info: self.info_ids.len(),
            warn: self.warn_ids.len(),
            error: self.error_ids.len(),
            latest_id: self.entries.back().map(|entry| entry.id),
        }
    }

    fn ids(&self, level: LogLevel) -> &VecDeque<u64> {
        match level {
            LogLevel::Debug => &self.debug_ids,
            LogLevel::Info => &self.info_ids,
            LogLevel::Warn => &self.warn_ids,
            LogLevel::Error => &self.error_ids,
        }
    }

    fn ids_mut(&mut self, level: LogLevel) -> &mut VecDeque<u64> {
        match level {
            LogLevel::Debug => &mut self.debug_ids,
            LogLevel::Info => &mut self.info_ids,
            LogLevel::Warn => &mut self.warn_ids,
            LogLevel::Error => &mut self.error_ids,
        }
    }
}

impl From<&LogEntry> for LogListEntry {
    fn from(entry: &LogEntry) -> Self {
        Self {
            id: entry.id,
            time: entry.time.clone(),
            level: entry.level,
            source: entry.source.clone(),
            request_id: entry.request_id.clone(),
            task_id: entry.task_id.clone(),
            summary: entry.summary.clone(),
            has_detail: entry.detail.is_some(),
        }
    }
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(redact_map(map)),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_value).collect()),
        other => other,
    }
}

fn redact_map(map: Map<String, Value>) -> Map<String, Value> {
    map.into_iter()
        .map(|(key, value)| {
            let lowered = key.to_ascii_lowercase();
            if lowered.contains("key")
                || lowered.contains("secret")
                || lowered.contains("authorization")
                || lowered.contains("token")
            {
                (key, Value::String("••••".to_string()))
            } else {
                (key, redact_value(value))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_secret_like_keys() {
        let store = LogStore::default();
        let entry = store.push(
            LogLevel::Info,
            "test",
            None,
            None,
            "hello",
            Some(json!({"api_key": "sk-test", "nested": {"authorization": "Bearer x"}})),
        );
        assert_eq!(entry.detail.unwrap()["api_key"], "••••");
    }

    #[test]
    fn clear_removes_memory_logs() {
        let store = LogStore::default();
        store.push(LogLevel::Info, "test", None, None, "hello", None);
        assert_eq!(store.clear_stats().total, 0);
    }

    #[test]
    fn pages_and_filters_lightweight_logs() {
        let store = LogStore::default();
        store.push(
            LogLevel::Info,
            "api",
            None,
            None,
            "one",
            Some(json!({"a": 1})),
        );
        store.push(LogLevel::Debug, "stream", None, None, "two", None);
        store.push(LogLevel::Info, "tool", None, None, "three", None);

        let stats = store.stats();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.info, 2);
        assert_eq!(stats.debug, 1);

        let page = store.page(Some(LogLevel::Info), 0, 10);
        assert_eq!(page.total, 2);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].summary, "one");
        assert!(page.entries[0].has_detail);
        assert_eq!(page.entries[1].summary, "three");
    }

    #[test]
    fn zero_limit_returns_empty_page() {
        let store = LogStore::default();
        store.push(LogLevel::Info, "api", None, None, "one", None);

        let page = store.page(None, 0, 0);

        assert_eq!(page.total, 1);
        assert_eq!(page.offset, 0);
        assert_eq!(page.limit, 0);
        assert!(page.entries.is_empty());
    }

    #[test]
    fn gets_detail_by_id() {
        let store = LogStore::default();
        let entry = store.push(
            LogLevel::Warn,
            "test",
            Some("req".to_string()),
            Some("task".to_string()),
            "detail",
            Some(json!({"path": "/tmp/a"})),
        );

        let detail = store.detail(entry.id).unwrap();
        assert_eq!(detail.id, entry.id);
        assert_eq!(detail.detail.unwrap()["path"], "/tmp/a");
        assert!(store.detail(entry.id + 1).is_none());
    }

    #[test]
    fn keeps_more_than_one_million_logs_and_drops_oldest() {
        let store = LogStore::default();
        for index in 0..(MAX_LOGS + 5) {
            let level = if index % 2 == 0 {
                LogLevel::Info
            } else {
                LogLevel::Debug
            };
            store.push(level, "bulk", None, None, "line", None);
        }

        let stats = store.stats();
        assert_eq!(stats.total, MAX_LOGS);
        assert_eq!(stats.dropped, 5);
        assert_eq!(stats.latest_id, Some((MAX_LOGS + 5) as u64));

        let first = store.page(None, 0, 1);
        assert_eq!(first.entries[0].id, 6);
        let last = store.page(None, MAX_LOGS - 1, 1);
        assert_eq!(last.entries[0].id, (MAX_LOGS + 5) as u64);
    }
}
