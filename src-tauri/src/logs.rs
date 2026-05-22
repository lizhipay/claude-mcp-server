use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
};

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter};

const MAX_LOGS: usize = 5_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
pub struct LogSnapshot {
    pub entries: Vec<LogEntry>,
    pub dropped: usize,
}

#[derive(Default)]
pub struct LogStore {
    entries: Mutex<VecDeque<LogEntry>>,
    dropped: AtomicUsize,
    next_id: AtomicU64,
    app_handle: Mutex<Option<AppHandle>>,
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
            let mut entries = self.entries.lock().expect("logs poisoned");
            if entries.len() >= MAX_LOGS {
                entries.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            entries.push_back(entry.clone());
        }
        if let Some(handle) = self
            .app_handle
            .lock()
            .expect("log app handle poisoned")
            .clone()
        {
            let _ = handle.emit("log-entry", &entry);
        }
        entry
    }

    pub fn snapshot(&self) -> LogSnapshot {
        LogSnapshot {
            entries: self
                .entries
                .lock()
                .expect("logs poisoned")
                .iter()
                .cloned()
                .collect(),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }

    pub fn clear(&self) -> LogSnapshot {
        self.entries.lock().expect("logs poisoned").clear();
        self.dropped.store(0, Ordering::Relaxed);
        self.snapshot()
    }
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
        assert_eq!(store.clear().entries.len(), 0);
    }
}
