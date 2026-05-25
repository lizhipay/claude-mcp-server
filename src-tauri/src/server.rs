use axum::{extract::State, response::IntoResponse, routing::get, Json, Router};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use serde::Serialize;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{config::AppConfig, logs::LogLevel, mcp::ClaudeMcpService, state::AppState};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    pub status: ServiceStatus,
    pub mcp_url: Option<String>,
    pub health_url: Option<String>,
    pub message: String,
}

pub struct ServerHandle {
    port: u16,
    cancel: CancellationToken,
    join: JoinHandle<()>,
}

pub struct ServerState {
    status: ServerStatus,
    handle: Option<ServerHandle>,
}

pub struct ServerController {
    state: tokio::sync::Mutex<ServerState>,
}

impl Default for ServerController {
    fn default() -> Self {
        Self {
            state: tokio::sync::Mutex::new(ServerState {
                status: stopped_status(),
                handle: None,
            }),
        }
    }
}

impl ServerController {
    pub async fn start(&self, app_state: AppState, cfg: AppConfig) -> anyhow::Result<ServerStatus> {
        crate::config::validate_port(cfg.port)?;
        let mut locked = self.state.lock().await;
        if locked.handle.is_some() {
            return Ok(locked.status.clone());
        }
        locked.status = ServerStatus {
            status: ServiceStatus::Starting,
            mcp_url: None,
            health_url: None,
            message: "正在唤醒…".to_string(),
        };
        app_state.logs().clear_stats();
        app_state.logs().push(
            LogLevel::Info,
            "server",
            None,
            None,
            "准备启动 MCP 服务",
            Some(serde_json::json!({"host": "127.0.0.1", "port": cfg.port})),
        );

        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", cfg.port)).await {
            Ok(listener) => listener,
            Err(error) => {
                locked.status = ServerStatus {
                    status: ServiceStatus::Error,
                    mcp_url: None,
                    health_url: None,
                    message: format!("端口启动失败：{error}"),
                };
                app_state.logs().push(
                    LogLevel::Error,
                    "server",
                    None,
                    None,
                    "MCP 服务启动失败",
                    Some(serde_json::json!({"port": cfg.port, "error": error.to_string()})),
                );
                anyhow::bail!("这个端口已经被别人占用了，换一个试试？");
            }
        };

        let cancel = CancellationToken::new();
        let service_state = app_state.clone();
        let service: StreamableHttpService<ClaudeMcpService, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(ClaudeMcpService::new(service_state.clone())),
                Default::default(),
                StreamableHttpServerConfig::default()
                    .with_stateful_mode(false)
                    .with_sse_keep_alive(None)
                    .with_cancellation_token(cancel.child_token()),
            );
        let router = Router::new()
            .route("/health", get(health))
            .nest_service("/mcp", service)
            .with_state(app_state.clone());
        let shutdown = cancel.clone();
        let join = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
                .await;
        });

        let status = ServerStatus {
            status: ServiceStatus::Running,
            mcp_url: Some(format!("http://127.0.0.1:{}/mcp", cfg.port)),
            health_url: Some(format!("http://127.0.0.1:{}/health", cfg.port)),
            message: "元气满满运行中".to_string(),
        };
        locked.handle = Some(ServerHandle {
            port: cfg.port,
            cancel,
            join,
        });
        locked.status = status.clone();
        app_state.logs().push(
            LogLevel::Info,
            "server",
            None,
            None,
            "MCP 服务启动完成",
            Some(serde_json::json!({"mcp_url": status.mcp_url, "health_url": status.health_url})),
        );
        Ok(status)
    }

    pub async fn stop(&self, app_state: AppState) -> anyhow::Result<ServerStatus> {
        let handle = {
            let mut locked = self.state.lock().await;
            locked.handle.take()
        };

        if let Some(handle) = handle {
            app_state.logs().push(
                LogLevel::Info,
                "server",
                None,
                None,
                "准备关闭 MCP 服务",
                Some(serde_json::json!({"port": handle.port})),
            );
            handle.cancel.cancel();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle.join).await;
        }

        let mut locked = self.state.lock().await;
        locked.status = stopped_status();
        app_state
            .logs()
            .push(LogLevel::Info, "server", None, None, "MCP 服务已停止", None);
        Ok(locked.status.clone())
    }

    pub async fn status(&self) -> ServerStatus {
        self.state.lock().await.status.clone()
    }
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.server().status().await)
}

fn stopped_status() -> ServerStatus {
    ServerStatus {
        status: ServiceStatus::Stopped,
        mcp_url: None,
        health_url: None,
        message: "休息中".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[tokio::test]
    async fn serves_health_and_mcp_tools() {
        let state = AppState::new();
        let port = unused_port().await;
        let cfg = AppConfig {
            api_url: "https://api.example.com".to_string(),
            model: "claude-test".to_string(),
            port,
            has_api_key: false,
        };
        let status = state.server().start(state.clone(), cfg).await.unwrap();
        assert_eq!(status.status, ServiceStatus::Running);

        let client = reqwest::Client::new();
        let health: Value = client
            .get(status.health_url.clone().unwrap())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(health["status"], "running");

        let mcp_url = status.mcp_url.clone().unwrap();
        let init = post_mcp(
            &client,
            &mcp_url,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1.0"}
                }
            }),
        )
        .await;
        assert_eq!(init["id"], 1);
        assert!(init["result"]["capabilities"]["tools"].is_object());

        let tools = post_mcp(
            &client,
            &mcp_url,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        )
        .await;
        let tool_names: Vec<String> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(ToOwned::to_owned))
            .collect();
        assert!(
            tool_names.contains(&"code".to_string()),
            "tools: {tool_names:?}"
        );
        assert!(
            tool_names.contains(&"code_status".to_string()),
            "tools: {tool_names:?}"
        );
        assert!(
            tool_names.contains(&"code_wait".to_string()),
            "tools: {tool_names:?}"
        );
        assert!(
            tool_names.contains(&"code_batch_wait".to_string()),
            "tools: {tool_names:?}"
        );
        assert!(
            tool_names.contains(&"code_batch_result".to_string()),
            "tools: {tool_names:?}"
        );
        assert!(
            tool_names.contains(&"code_batch_poll".to_string()),
            "tools: {tool_names:?}"
        );

        let call = post_mcp(
            &client,
            &mcp_url,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "code_status",
                    "arguments": {"job_id": "missing"}
                }
            }),
        )
        .await;
        assert_eq!(call["id"], 3);
        assert_eq!(call["result"]["isError"], true);

        let batch = post_mcp(
            &client,
            &mcp_url,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "code_batch_result",
                    "arguments": {"job_ids": ["missing"]}
                }
            }),
        )
        .await;
        assert_eq!(batch["id"], 4);
        assert_eq!(batch["result"]["structuredContent"]["total"], 1);
        assert_eq!(
            batch["result"]["structuredContent"]["not_found"][0]["job_id"],
            "missing"
        );

        let poll = post_mcp(
            &client,
            &mcp_url,
            json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "code_batch_poll",
                    "arguments": {"job_ids": ["missing"], "timeout_seconds": 0}
                }
            }),
        )
        .await;
        assert_eq!(poll["id"], 5);
        assert_eq!(poll["result"]["structuredContent"]["ready_count"], 1);
        assert_eq!(
            poll["result"]["structuredContent"]["not_found"][0]["job_id"],
            "missing"
        );

        state.server().stop(state.clone()).await.unwrap();
    }

    async fn unused_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    async fn post_mcp(client: &reqwest::Client, url: &str, body: Value) -> Value {
        let response = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "unexpected status: {}",
            response.status()
        );
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = response.text().await.unwrap();
        if content_type.contains("text/event-stream") {
            parse_sse_json(&text)
        } else {
            serde_json::from_str(&text).unwrap()
        }
    }

    fn parse_sse_json(text: &str) -> Value {
        let data = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("data:"))
            .expect("SSE data line")
            .trim();
        serde_json::from_str(data).unwrap()
    }
}
