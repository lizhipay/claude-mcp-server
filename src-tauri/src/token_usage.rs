use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::config;

const USAGE_FILE_NAME: &str = "token-usage.json";

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
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TokenUsageDelta {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
}

pub struct TokenUsageStore {
    path: Option<PathBuf>,
    data: Mutex<TokenUsageFile>,
    app_handle: Mutex<Option<AppHandle>>,
}

impl TokenUsageStore {
    pub fn new() -> Self {
        let path = token_usage_path().ok();
        let data = path
            .as_deref()
            .map(load_usage_file_from)
            .unwrap_or_default();
        Self {
            path,
            data: Mutex::new(data),
            app_handle: Mutex::new(None),
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
        let snapshot = {
            let mut data = self.data.lock().expect("token usage poisoned");
            *data = TokenUsageFile {
                updated_at: Some(now_timestamp()),
                ..TokenUsageFile::default()
            };
            self.persist_locked(&data)?;
            data.snapshot()
        };
        self.emit(&snapshot);
        Ok(snapshot)
    }

    pub fn record_usage(&self, usage: &Value) -> anyhow::Result<TokenUsageSnapshot> {
        let delta = normalize_usage(usage);
        let snapshot = {
            let mut data = self.data.lock().expect("token usage poisoned");
            data.record(&today(), now_timestamp(), delta);
            self.persist_locked(&data)?;
            data.snapshot()
        };
        self.emit(&snapshot);
        Ok(snapshot)
    }

    fn persist_locked(&self, data: &TokenUsageFile) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(data)?)?;
        restrict_usage_permissions(path)?;
        Ok(())
    }

    fn emit(&self, snapshot: &TokenUsageSnapshot) {
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

fn normalize_usage(usage: &Value) -> TokenUsageDelta {
    let input_tokens = number_field(usage, "input_tokens");
    let output_tokens = number_field(usage, "output_tokens");
    let cache_read_tokens = number_field(usage, "cache_read_input_tokens");
    let explicit_cache_write = number_field(usage, "cache_creation_input_tokens");
    let nested_cache_write = usage.get("cache_creation").map(sum_numbers).unwrap_or(0);
    let cache_write_tokens = if explicit_cache_write > 0 {
        explicit_cache_write
    } else {
        nested_cache_write
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
        let store = TokenUsageStore {
            path: Some(path.clone()),
            data: Mutex::new(TokenUsageFile::default()),
            app_handle: Mutex::new(None),
        };

        store
            .record_usage(&json!({"input_tokens": 3, "output_tokens": 4}))
            .unwrap();
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
        let store = TokenUsageStore {
            path: Some(path.clone()),
            data: Mutex::new(TokenUsageFile::default()),
            app_handle: Mutex::new(None),
        };

        store
            .record_usage(&json!({"input_tokens": 8, "output_tokens": 5}))
            .unwrap();
        let loaded = load_usage_file_from(&path).snapshot();

        assert_eq!(loaded.totals.requests, 1);
        assert_eq!(loaded.totals.total_tokens, 13);
        assert_eq!(loaded.days.len(), 1);
    }
}
