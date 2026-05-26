use std::{
    fs,
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const DEFAULT_API_URL: &str = "https://api.anthropic.com";
const DEFAULT_MODEL: &str = "claude-opus-4-7";
const LEGACY_DEFAULT_MODELS: &[&str] = &["claude-sonnet-4-7", "claude-sonnet-4-6"];
const DEFAULT_PORT: u16 = 8765;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub api_url: String,
    pub model: String,
    pub port: u16,
    pub has_api_key: bool,
    pub agent_runtime: AgentRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveConfigPayload {
    pub api_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub port: u16,
    #[serde(default)]
    pub agent_runtime: Option<AgentRuntime>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentRuntime {
    Sdk,
    Legacy,
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self::Sdk
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigFile {
    api_url: String,
    model: String,
    port: u16,
    #[serde(default)]
    agent_runtime: AgentRuntime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            port: DEFAULT_PORT,
            agent_runtime: AgentRuntime::Sdk,
            api_key: None,
        }
    }
}

impl ConfigFile {
    fn has_api_key(&self) -> bool {
        self.api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
    }

    fn into_app_config(self) -> AppConfig {
        let has_api_key = self.has_api_key();
        AppConfig {
            api_url: self.api_url,
            model: self.model,
            port: self.port,
            has_api_key,
            agent_runtime: self.agent_runtime,
        }
    }
}

pub fn config_path() -> anyhow::Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "zoe", "cclaude-mcp")
        .ok_or_else(|| anyhow::anyhow!("无法找到配置目录"))?;
    Ok(dirs.config_dir().join("config.json"))
}

pub fn load_config() -> AppConfig {
    load_config_file().into_app_config()
}

pub fn load_agent_runtime() -> AgentRuntime {
    load_config_file().agent_runtime
}

pub fn require_api_key() -> anyhow::Result<String> {
    load_config_file()
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("还没有填写密钥哦"))
}

fn load_config_file() -> ConfigFile {
    config_path()
        .ok()
        .map(|path| load_config_file_from(&path))
        .unwrap_or_default()
}

fn load_config_file_from(path: &Path) -> ConfigFile {
    let mut file = fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<ConfigFile>(&raw).ok())
        .unwrap_or_default();
    if LEGACY_DEFAULT_MODELS.contains(&file.model.as_str()) {
        file.model = DEFAULT_MODEL.to_string();
    }
    file
}

pub fn save_config(payload: SaveConfigPayload) -> anyhow::Result<AppConfig> {
    save_config_to_path(payload, &config_path()?)
}

fn save_config_to_path(payload: SaveConfigPayload, path: &Path) -> anyhow::Result<AppConfig> {
    validate_port(payload.port)?;
    let api_url = payload.api_url.trim();
    if api_url.is_empty() {
        anyhow::bail!("API 地址不能为空");
    }
    normalize_messages_url(api_url)?;
    let model = payload.model.trim();
    if model.is_empty() {
        anyhow::bail!("模型名称不能为空");
    }

    let previous = load_config_file_from(path);
    let api_key = if let Some(api_key) = payload
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(api_key.to_string())
    } else {
        previous.api_key
    };

    let file = ConfigFile {
        api_url: api_url.to_string(),
        model: model.to_string(),
        port: payload.port,
        agent_runtime: payload.agent_runtime.unwrap_or(previous.agent_runtime),
        api_key,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_config_file(path, &file)?;
    Ok(file.into_app_config())
}

fn write_config_file(path: &Path, file: &ConfigFile) -> anyhow::Result<()> {
    fs::write(path, serde_json::to_string_pretty(file)?)?;
    restrict_config_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_config_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_config_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

pub fn validate_port(port: u16) -> anyhow::Result<()> {
    if port < 1024 {
        anyhow::bail!("端口号需要是 1024~65535 之间的数字哦");
    }
    Ok(())
}

pub fn normalize_messages_url(input: &str) -> anyhow::Result<String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        anyhow::bail!("API 地址不能为空");
    }
    let url = if trimmed.ends_with("/v1/messages") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/messages")
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        anyhow::bail!("API 地址需要以 http:// 或 https:// 开头");
    }
    Ok(url)
}

pub fn normalize_api_base_url(input: &str) -> anyhow::Result<String> {
    let messages_url = normalize_messages_url(input)?;
    Ok(messages_url
        .trim_end_matches("/v1/messages")
        .trim_end_matches('/')
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_root_url() {
        assert_eq!(
            normalize_messages_url("https://example.com").unwrap(),
            "https://example.com/v1/messages"
        );
    }

    #[test]
    fn keeps_messages_url() {
        assert_eq!(
            normalize_messages_url("https://example.com/v1/messages").unwrap(),
            "https://example.com/v1/messages"
        );
    }

    #[test]
    fn normalizes_api_base_url() {
        assert_eq!(
            normalize_api_base_url("https://example.com/v1/messages").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_api_base_url("https://example.com").unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn default_model_is_opus() {
        assert_eq!(ConfigFile::default().model, "claude-opus-4-7");
        assert_eq!(ConfigFile::default().agent_runtime, AgentRuntime::Sdk);
    }

    #[test]
    fn migrates_legacy_default_models_to_opus() {
        for legacy_model in LEGACY_DEFAULT_MODELS {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            write_config_file(
                &path,
                &ConfigFile {
                    api_url: "https://example.com".to_string(),
                    model: legacy_model.to_string(),
                    port: 8765,
                    agent_runtime: AgentRuntime::Sdk,
                    api_key: None,
                },
            )
            .unwrap();

            assert_eq!(load_config_file_from(&path).model, "claude-opus-4-7");
        }
    }

    #[test]
    fn rejects_tiny_port() {
        assert!(validate_port(80).is_err());
    }

    #[test]
    fn saves_api_key_in_config_file_without_exposing_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        let saved = save_config_to_path(
            SaveConfigPayload {
                api_url: "https://example.com".to_string(),
                api_key: Some("sk-local-secret".to_string()),
                model: "claude-opus-4-7".to_string(),
                port: 8765,
                agent_runtime: None,
            },
            &path,
        )
        .unwrap();

        assert!(saved.has_api_key);
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("sk-local-secret"));
        assert!(!serde_json::to_value(saved)
            .unwrap()
            .to_string()
            .contains("sk-local-secret"));
    }

    #[test]
    fn blank_api_key_preserves_saved_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        save_config_to_path(
            SaveConfigPayload {
                api_url: "https://example.com".to_string(),
                api_key: Some("sk-existing".to_string()),
                model: "claude-opus-4-7".to_string(),
                port: 8765,
                agent_runtime: Some(AgentRuntime::Legacy),
            },
            &path,
        )
        .unwrap();
        let saved = save_config_to_path(
            SaveConfigPayload {
                api_url: "https://example.org".to_string(),
                api_key: Some("  ".to_string()),
                model: "claude-opus-4-7".to_string(),
                port: 8766,
                agent_runtime: None,
            },
            &path,
        )
        .unwrap();

        assert!(saved.has_api_key);
        assert_eq!(saved.agent_runtime, AgentRuntime::Legacy);
        let file = load_config_file_from(&path);
        assert_eq!(file.api_key.as_deref(), Some("sk-existing"));
        assert_eq!(file.port, 8766);
        assert_eq!(file.agent_runtime, AgentRuntime::Legacy);
    }

    #[test]
    fn missing_api_key_is_reported() {
        let file = ConfigFile::default();
        assert!(!file.has_api_key());
    }

    #[cfg(unix)]
    #[test]
    fn config_file_permissions_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config_file(
            &path,
            &ConfigFile {
                api_url: "https://example.com".to_string(),
                model: "claude-opus-4-7".to_string(),
                port: 8765,
                agent_runtime: AgentRuntime::Sdk,
                api_key: Some("sk-permission".to_string()),
            },
        )
        .unwrap();

        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
