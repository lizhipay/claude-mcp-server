use std::sync::Arc;

use reqwest::Client;
use tauri::AppHandle;

use crate::{
    jobs::JobStore, logs::LogStore, server::ServerController, token_usage::TokenUsageStore,
};

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub logs: LogStore,
    pub jobs: JobStore,
    pub server: ServerController,
    pub token_usage: TokenUsageStore,
    pub http: Client,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                logs: LogStore::default(),
                jobs: JobStore::default(),
                server: ServerController::default(),
                token_usage: TokenUsageStore::default(),
                http: Client::new(),
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

    pub fn http(&self) -> &Client {
        &self.inner.http
    }

    pub fn set_app_handle(&self, app_handle: AppHandle) {
        self.inner.logs.set_app_handle(app_handle.clone());
        self.inner.token_usage.set_app_handle(app_handle);
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
