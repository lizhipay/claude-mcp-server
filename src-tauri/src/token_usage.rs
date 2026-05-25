use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::Local;
use crossbeam_queue::SegQueue;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::config;

const USAGE_FILE_NAME: &str = "token-usage.json";
const USAGE_EVENTS_FILE_NAME: &str = "token-usage-events.jsonl";
const USAGE_FLUSH_BATCH: usize = 1_024;
const USAGE_COMPACT_EVERY_EVENTS: usize = 5_000;
const USAGE_EVENT_EMIT_MS: u64 = 250;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsageTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DailyTokenUsage {
    pub date: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsageSnapshot {
    pub totals: TokenUsageTotals,
    pub days: Vec<DailyTokenUsage>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TokenUsageFile {
    totals: TokenUsageTotals,
    days: BTreeMap<String, DailyTokenUsage>,
    updated_at: Option<String>,
    #[serde(default)]
    last_event_id: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TokenUsageDelta {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenUsageEvent {
    id: u64,
    date: String,
    updated_at: String,
    delta: TokenUsageDelta,
}

pub struct TokenUsageStore {
    path: Option<PathBuf>,
    events_path: Option<PathBuf>,
    data: Arc<Mutex<TokenUsageFile>>,
    pending_events: Arc<SegQueue<TokenUsageEvent>>,
    pending_count: Arc<AtomicUsize>,
    next_event_id: AtomicU64,
    flushed_since_snapshot: Arc<AtomicUsize>,
    flushing: Arc<AtomicBool>,
    flush_lock: Arc<Mutex<()>>,
    last_emit_ms: Arc<AtomicU64>,
    emit_pending: Arc<AtomicBool>,
    app_handle: Arc<Mutex<Option<AppHandle>>>,
}

impl TokenUsageStore {
    pub fn new() -> Self {
        let path = token_usage_path().ok();
        let events_path = path
            .as_ref()
            .map(|path| path.with_file_name(USAGE_EVENTS_FILE_NAME));
        let mut data = path
            .as_deref()
            .map(load_usage_file_from)
            .unwrap_or_default();
        if let Some(events_path) = &events_path {
            replay_usage_events_from(&mut data, events_path);
        }
        let next_event_id = data.last_event_id;
        Self {
            path,
            events_path,
            data: Arc::new(Mutex::new(data)),
            pending_events: Arc::new(SegQueue::new()),
            pending_count: Arc::new(AtomicUsize::new(0)),
            next_event_id: AtomicU64::new(next_event_id),
            flushed_since_snapshot: Arc::new(AtomicUsize::new(0)),
            flushing: Arc::new(AtomicBool::new(false)),
            flush_lock: Arc::new(Mutex::new(())),
            last_emit_ms: Arc::new(AtomicU64::new(0)),
            emit_pending: Arc::new(AtomicBool::new(false)),
            app_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_app_handle(&self, app_handle: AppHandle) {
        *self
            .app_handle
            .lock()
            .expect("token usage app handle poisoned") = Some(app_handle);
    }

    pub fn snapshot(&self) -> TokenUsageSnapshot {
        self.data.lock().expect("token usage poisoned").snapshot()
    }

    pub fn clear(&self) -> anyhow::Result<TokenUsageSnapshot> {
        let _flush_guard = self.flush_lock.lock().expect("token usage flush poisoned");
        while self.pending_events.pop().is_some() {
            self.pending_count.fetch_sub(1, Ordering::Relaxed);
        }
        let snapshot = {
            let mut data = self.data.lock().expect("token usage poisoned");
            *data = TokenUsageFile {
                updated_at: Some(now_timestamp()),
                last_event_id: self.next_event_id.load(Ordering::Relaxed),
                ..TokenUsageFile::default()
            };
            self.persist_locked(&data)?;
            data.snapshot()
        };
        if let Some(events_path) = &self.events_path {
            let _ = fs::remove_file(events_path);
        }
        self.flushed_since_snapshot.store(0, Ordering::Relaxed);
        self.emit_now(&snapshot);
        Ok(snapshot)
    }

    pub fn record_usage(&self, usage: &Value) -> anyhow::Result<TokenUsageSnapshot> {
        let delta = normalize_usage(usage);
        let date = today();
        let updated_at = now_timestamp();
        let (event, snapshot) = {
            let mut data = self.data.lock().expect("token usage poisoned");
            let id = self.next_event_id.fetch_add(1, Ordering::Relaxed) + 1;
            let event = TokenUsageEvent {
                id,
                date,
                updated_at,
                delta,
            };
            data.record(&event.date, event.updated_at.clone(), event.delta);
            data.last_event_id = event.id;
            (event, data.snapshot())
        };
        self.pending_events.push(event);
        self.pending_count.fetch_add(1, Ordering::Relaxed);
        self.schedule_flush();
        self.schedule_emit();
        Ok(snapshot)
    }

    pub fn pending_count(&self) -> usize {
        self.pending_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn flush_pending(&self) -> anyhow::Result<()> {
        self.flush_usage_events(usize::MAX)?;
        self.compact_snapshot()
    }

    fn persist_locked(&self, data: &TokenUsageFile) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        persist_usage_snapshot(path, data)?;
        Ok(())
    }

    fn schedule_flush(&self) {
        if self.flushing.swap(true, Ordering::AcqRel) {
            return;
        }

        let events_path = self.events_path.clone();
        let snapshot_path = self.path.clone();
        let data = self.data.clone();
        let pending_events = self.pending_events.clone();
        let pending_count = self.pending_count.clone();
        let flushed_since_snapshot = self.flushed_since_snapshot.clone();
        let flushing = self.flushing.clone();
        let flush_lock = self.flush_lock.clone();
        std::thread::spawn(move || loop {
            let drained = flush_usage_events_batch(
                events_path.as_deref(),
                snapshot_path.as_deref(),
                &data,
                &pending_events,
                &pending_count,
                &flushed_since_snapshot,
                &flush_lock,
                USAGE_FLUSH_BATCH,
            );
            if drained == 0 {
                flushing.store(false, Ordering::Release);
                if pending_count.load(Ordering::Relaxed) > 0
                    && !flushing.swap(true, Ordering::AcqRel)
                {
                    continue;
                }
                break;
            }
            if pending_count.load(Ordering::Relaxed) > 0 {
                std::thread::yield_now();
            }
        });
    }

    fn flush_usage_events(&self, max: usize) -> anyhow::Result<()> {
        while flush_usage_events_batch(
            self.events_path.as_deref(),
            self.path.as_deref(),
            &self.data,
            &self.pending_events,
            &self.pending_count,
            &self.flushed_since_snapshot,
            &self.flush_lock,
            max,
        ) > 0
        {}
        Ok(())
    }

    fn compact_snapshot(&self) -> anyhow::Result<()> {
        let _guard = self.flush_lock.lock().expect("token usage flush poisoned");
        let Some(path) = &self.path else {
            return Ok(());
        };
        let snapshot = self.data.lock().expect("token usage poisoned").clone();
        persist_usage_snapshot(path, &snapshot)?;
        if let Some(events_path) = &self.events_path {
            let _ = fs::remove_file(events_path);
        }
        self.flushed_since_snapshot.store(0, Ordering::Relaxed);
        Ok(())
    }

    fn schedule_emit(&self) {
        let Some(handle) = self
            .app_handle
            .lock()
            .expect("token usage app handle poisoned")
            .clone()
        else {
            return;
        };

        let now = current_millis();
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= USAGE_EVENT_EMIT_MS {
            self.last_emit_ms.store(now, Ordering::Relaxed);
            let snapshot = self.snapshot();
            let _ = handle.emit("token-usage-updated", snapshot);
            return;
        }

        if self.emit_pending.swap(true, Ordering::AcqRel) {
            return;
        }

        let data = self.data.clone();
        let pending = self.emit_pending.clone();
        let last_emit = self.last_emit_ms.clone();
        let delay = USAGE_EVENT_EMIT_MS.saturating_sub(now.saturating_sub(last));
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay.max(1))).await;
            let snapshot = data.lock().expect("token usage poisoned").snapshot();
            last_emit.store(current_millis(), Ordering::Relaxed);
            pending.store(false, Ordering::Release);
            let _ = handle.emit("token-usage-updated", snapshot);
        });
    }

    fn emit_now(&self, snapshot: &TokenUsageSnapshot) {
        self.last_emit_ms.store(current_millis(), Ordering::Relaxed);
        if let Some(handle) = self
            .app_handle
            .lock()
            .expect("token usage app handle poisoned")
            .clone()
        {
            let _ = handle.emit("token-usage-updated", snapshot);
        }
    }
}

impl Default for TokenUsageStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TokenUsageStore {
    fn drop(&mut self) {
        let _ = self.flush_usage_events(usize::MAX);
        let _ = self.compact_snapshot();
    }
}

impl TokenUsageFile {
    fn snapshot(&self) -> TokenUsageSnapshot {
        let mut days: Vec<_> = self.days.values().cloned().collect();
        days.sort_by(|a, b| b.date.cmp(&a.date));
        TokenUsageSnapshot {
            totals: self.totals.clone(),
            days,
            updated_at: self.updated_at.clone(),
        }
    }

    fn record(&mut self, date: &str, updated_at: String, delta: TokenUsageDelta) {
        self.totals.add(delta);
        let day = self
            .days
            .entry(date.to_string())
            .or_insert_with(|| DailyTokenUsage {
                date: date.to_string(),
                ..DailyTokenUsage::default()
            });
        day.add(delta);
        self.updated_at = Some(updated_at);
    }

    fn record_event(&mut self, event: &TokenUsageEvent) {
        if event.id <= self.last_event_id {
            return;
        }
        self.record(&event.date, event.updated_at.clone(), event.delta);
        self.last_event_id = event.id;
    }
}

impl TokenUsageTotals {
    fn add(&mut self, delta: TokenUsageDelta) {
        self.requests += 1;
        self.input_tokens += delta.input_tokens;
        self.output_tokens += delta.output_tokens;
        self.cache_read_tokens += delta.cache_read_tokens;
        self.cache_write_tokens += delta.cache_write_tokens;
        self.total_tokens += delta.total_tokens;
    }
}

impl DailyTokenUsage {
    fn add(&mut self, delta: TokenUsageDelta) {
        self.requests += 1;
        self.input_tokens += delta.input_tokens;
        self.output_tokens += delta.output_tokens;
        self.cache_read_tokens += delta.cache_read_tokens;
        self.cache_write_tokens += delta.cache_write_tokens;
        self.total_tokens += delta.total_tokens;
    }
}

fn token_usage_path() -> anyhow::Result<PathBuf> {
    Ok(config::config_path()?.with_file_name(USAGE_FILE_NAME))
}

fn load_usage_file_from(path: &Path) -> TokenUsageFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<TokenUsageFile>(&raw).ok())
        .unwrap_or_default()
}

fn replay_usage_events_from(data: &mut TokenUsageFile, path: &Path) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Ok(event) = serde_json::from_str::<TokenUsageEvent>(line) {
            data.record_event(&event);
        }
    }
}

fn flush_usage_events_batch(
    events_path: Option<&Path>,
    snapshot_path: Option<&Path>,
    data: &Mutex<TokenUsageFile>,
    pending_events: &SegQueue<TokenUsageEvent>,
    pending_count: &AtomicUsize,
    flushed_since_snapshot: &AtomicUsize,
    flush_lock: &Mutex<()>,
    max: usize,
) -> usize {
    let _guard = flush_lock.lock().expect("token usage flush poisoned");
    let mut batch = Vec::new();
    while batch.len() < max {
        let Some(event) = pending_events.pop() else {
            break;
        };
        pending_count.fetch_sub(1, Ordering::Relaxed);
        batch.push(event);
    }
    let drained = batch.len();
    if drained == 0 {
        return 0;
    }

    if let Some(events_path) = events_path {
        if let Err(error) = append_usage_events(events_path, &batch) {
            eprintln!("failed to append token usage events: {error}");
        }
    }

    let flushed = flushed_since_snapshot.fetch_add(drained, Ordering::Relaxed) + drained;
    if flushed >= USAGE_COMPACT_EVERY_EVENTS {
        if let Some(snapshot_path) = snapshot_path {
            let snapshot = data.lock().expect("token usage poisoned").clone();
            if let Err(error) = persist_usage_snapshot(snapshot_path, &snapshot) {
                eprintln!("failed to compact token usage snapshot: {error}");
            } else if let Some(events_path) = events_path {
                let _ = fs::remove_file(events_path);
                flushed_since_snapshot.store(0, Ordering::Relaxed);
            }
        }
    }

    drained
}

fn append_usage_events(path: &Path, events: &[TokenUsageEvent]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    for event in events {
        writeln!(file, "{}", serde_json::to_string(event)?)?;
    }
    restrict_usage_permissions(path)?;
    Ok(())
}

fn persist_usage_snapshot(path: &Path, data: &TokenUsageFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(data)?)?;
    restrict_usage_permissions(path)?;
    Ok(())
}

fn normalize_usage(usage: &Value) -> TokenUsageDelta {
    let output_tokens = first_number_field(usage, &["output_tokens", "completion_tokens"]);
    let explicit_cache_read = first_number_field(
        usage,
        &[
            "cache_read_input_tokens",
            "cache_read_tokens",
            "cache_read",
            "cached_tokens",
        ],
    );
    let nested_cache_read = nested_number_field(usage, &["input_token_details", "cache_read"])
        + nested_number_field(usage, &["prompt_tokens_details", "cached_tokens"]);
    let cache_read_tokens = if explicit_cache_read > 0 {
        explicit_cache_read
    } else {
        nested_cache_read
    };
    let explicit_cache_write = first_number_field(
        usage,
        &[
            "cache_creation_input_tokens",
            "cache_write_tokens",
            "cache_creation_tokens",
            "cache_write",
        ],
    );
    let nested_cache_write = usage.get("cache_creation").map(sum_numbers).unwrap_or(0)
        + nested_number_field(usage, &["input_token_details", "cache_creation"])
        + nested_number_field(usage, &["prompt_tokens_details", "cache_creation_tokens"]);
    let cache_write_tokens = if explicit_cache_write > 0 {
        explicit_cache_write
    } else {
        nested_cache_write
    };
    let input_tokens = if usage.get("input_tokens").is_some() {
        number_field(usage, "input_tokens")
    } else {
        number_field(usage, "prompt_tokens").saturating_sub(cache_read_tokens + cache_write_tokens)
    };
    let fallback_total = input_tokens + output_tokens + cache_read_tokens + cache_write_tokens;
    let total_tokens = usage
        .get("total_tokens")
        .map(number_value)
        .unwrap_or(fallback_total);

    TokenUsageDelta {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        total_tokens,
    }
}

fn number_field(value: &Value, key: &str) -> u64 {
    value.get(key).map(number_value).unwrap_or_default()
}

fn first_number_field(value: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .map(|key| number_field(value, key))
        .find(|amount| *amount > 0)
        .unwrap_or_default()
}

fn nested_number_field(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(value, |current, key| current.get(key))
        .map(number_value)
        .unwrap_or_default()
}

fn number_value(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        .unwrap_or_default()
}

fn sum_numbers(value: &Value) -> u64 {
    match value {
        Value::Number(_) | Value::String(_) => number_value(value),
        Value::Array(values) => values.iter().map(sum_numbers).sum(),
        Value::Object(map) => map.values().map(sum_numbers).sum(),
        _ => 0,
    }
}

fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn now_timestamp() -> String {
    Local::now().to_rfc3339()
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(unix)]
fn restrict_usage_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_usage_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_store(path: PathBuf) -> TokenUsageStore {
        TokenUsageStore {
            events_path: Some(path.with_file_name(USAGE_EVENTS_FILE_NAME)),
            path: Some(path),
            data: Arc::new(Mutex::new(TokenUsageFile::default())),
            pending_events: Arc::new(SegQueue::new()),
            pending_count: Arc::new(AtomicUsize::new(0)),
            next_event_id: AtomicU64::new(0),
            flushed_since_snapshot: Arc::new(AtomicUsize::new(0)),
            flushing: Arc::new(AtomicBool::new(false)),
            flush_lock: Arc::new(Mutex::new(())),
            last_emit_ms: Arc::new(AtomicU64::new(0)),
            emit_pending: Arc::new(AtomicBool::new(false)),
            app_handle: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn normalizes_common_usage_fields() {
        let usage = json!({
            "input_tokens": 100,
            "output_tokens": 30,
            "cache_read_input_tokens": 7,
            "cache_creation_input_tokens": 11
        });

        let normalized = normalize_usage(&usage);

        assert_eq!(normalized.input_tokens, 100);
        assert_eq!(normalized.output_tokens, 30);
        assert_eq!(normalized.cache_read_tokens, 7);
        assert_eq!(normalized.cache_write_tokens, 11);
        assert_eq!(normalized.total_tokens, 148);
    }

    #[test]
    fn normalizes_nested_cache_creation_when_explicit_field_is_missing() {
        let usage = json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 20,
                "ephemeral_1h_input_tokens": "30"
            }
        });

        let normalized = normalize_usage(&usage);

        assert_eq!(normalized.cache_write_tokens, 50);
        assert_eq!(normalized.total_tokens, 65);
    }

    #[test]
    fn normalizes_openai_compatible_cache_fields_without_double_counting() {
        let usage = json!({
            "prompt_tokens": 100,
            "completion_tokens": 30,
            "prompt_tokens_details": {
                "cached_tokens": 70,
                "cache_creation_tokens": 20
            }
        });

        let normalized = normalize_usage(&usage);

        assert_eq!(normalized.input_tokens, 10);
        assert_eq!(normalized.output_tokens, 30);
        assert_eq!(normalized.cache_read_tokens, 70);
        assert_eq!(normalized.cache_write_tokens, 20);
        assert_eq!(normalized.total_tokens, 130);
    }

    #[test]
    fn prefers_upstream_total_when_present() {
        let usage = json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 12
        });

        assert_eq!(normalize_usage(&usage).total_tokens, 12);
    }

    #[test]
    fn aggregates_totals_and_daily_rows() {
        let mut data = TokenUsageFile::default();
        data.record(
            "2026-05-22",
            "2026-05-22T10:00:00+08:00".to_string(),
            normalize_usage(&json!({"input_tokens": 10, "output_tokens": 2})),
        );
        data.record(
            "2026-05-22",
            "2026-05-22T10:01:00+08:00".to_string(),
            normalize_usage(&json!({"input_tokens": 20, "cache_read_input_tokens": 3})),
        );

        let snapshot = data.snapshot();

        assert_eq!(snapshot.totals.requests, 2);
        assert_eq!(snapshot.totals.input_tokens, 30);
        assert_eq!(snapshot.totals.cache_read_tokens, 3);
        assert_eq!(snapshot.days[0].requests, 2);
        assert_eq!(snapshot.days[0].total_tokens, 35);
    }

    #[test]
    fn clear_resets_memory_and_persisted_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token-usage.json");
        let store = test_store(path.clone());

        store
            .record_usage(&json!({"input_tokens": 3, "output_tokens": 4}))
            .unwrap();
        store.flush_pending().unwrap();
        let cleared = store.clear().unwrap();

        assert_eq!(cleared.totals.requests, 0);
        let raw = fs::read_to_string(path).unwrap();
        let persisted: TokenUsageFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(persisted.totals.requests, 0);
    }

    #[test]
    fn persists_and_loads_usage_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token-usage.json");
        let store = test_store(path.clone());

        store
            .record_usage(&json!({"input_tokens": 8, "output_tokens": 5}))
            .unwrap();
        store.flush_pending().unwrap();
        let loaded = load_usage_file_from(&path).snapshot();

        assert_eq!(loaded.totals.requests, 1);
        assert_eq!(loaded.totals.total_tokens, 13);
        assert_eq!(loaded.days.len(), 1);
    }

    #[test]
    fn replays_append_log_without_double_counting_snapshot_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token-usage.json");
        let events_path = dir.path().join(USAGE_EVENTS_FILE_NAME);
        let mut data = TokenUsageFile::default();
        data.record_event(&TokenUsageEvent {
            id: 1,
            date: "2026-05-25".to_string(),
            updated_at: "2026-05-25T10:00:00+08:00".to_string(),
            delta: normalize_usage(&json!({"input_tokens": 1})),
        });
        persist_usage_snapshot(&path, &data).unwrap();
        append_usage_events(
            &events_path,
            &[
                TokenUsageEvent {
                    id: 1,
                    date: "2026-05-25".to_string(),
                    updated_at: "2026-05-25T10:00:00+08:00".to_string(),
                    delta: normalize_usage(&json!({"input_tokens": 1})),
                },
                TokenUsageEvent {
                    id: 2,
                    date: "2026-05-25".to_string(),
                    updated_at: "2026-05-25T10:01:00+08:00".to_string(),
                    delta: normalize_usage(&json!({"output_tokens": 2})),
                },
            ],
        )
        .unwrap();

        let mut loaded = load_usage_file_from(&path);
        replay_usage_events_from(&mut loaded, &events_path);
        let snapshot = loaded.snapshot();

        assert_eq!(snapshot.totals.requests, 2);
        assert_eq!(snapshot.totals.input_tokens, 1);
        assert_eq!(snapshot.totals.output_tokens, 2);
    }

    #[test]
    fn records_usage_concurrently_without_losing_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token-usage.json");
        let store = Arc::new(test_store(path));
        let mut workers = Vec::new();
        for _ in 0..10 {
            let store = store.clone();
            workers.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    store
                        .record_usage(&json!({"input_tokens": 2, "output_tokens": 3}))
                        .unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        store.flush_pending().unwrap();
        let snapshot = store.snapshot();

        assert_eq!(snapshot.totals.requests, 1_000);
        assert_eq!(snapshot.totals.input_tokens, 2_000);
        assert_eq!(snapshot.totals.output_tokens, 3_000);
        assert_eq!(snapshot.totals.total_tokens, 5_000);
        assert_eq!(store.pending_count(), 0);
    }
}
