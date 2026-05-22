import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

export type ServiceStatus = "stopped" | "starting" | "running" | "error";
export type LogLevel = "debug" | "info" | "warn" | "error";

export interface AppConfig {
  api_url: string;
  model: string;
  port: number;
  has_api_key: boolean;
}

export interface SaveConfigPayload {
  api_url: string;
  api_key?: string;
  model: string;
  port: number;
}

export interface ServerStatus {
  status: ServiceStatus;
  mcp_url: string | null;
  health_url: string | null;
  message: string;
}

export interface LogEntry {
  id: number;
  time: string;
  level: LogLevel;
  source: string;
  request_id?: string | null;
  task_id?: string | null;
  summary: string;
  detail?: Record<string, unknown> | null;
}

export interface LogSnapshot {
  entries: LogEntry[];
  dropped: number;
}

export interface TokenUsageTotals {
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  total_tokens: number;
}

export interface DailyTokenUsage {
  date: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  total_tokens: number;
}

export interface TokenUsageSnapshot {
  totals: TokenUsageTotals;
  days: DailyTokenUsage[];
  updated_at?: string | null;
}

type Unlisten = () => void;

const mockConfig: AppConfig = {
  api_url: "https://api.anthropic.com",
  model: "claude-sonnet-4-7",
  port: 8765,
  has_api_key: false,
};

const mockStatus: ServerStatus = {
  status: "stopped",
  mcp_url: null,
  health_url: null,
  message: "休息中",
};

const emptyTokenUsage: TokenUsageSnapshot = {
  totals: {
    requests: 0,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    total_tokens: 0,
  },
  days: [],
  updated_at: null,
};

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function invoke<T>(command: string, args?: Record<string, unknown>, fallback?: () => T): Promise<T> {
  if (!isTauriRuntime()) {
    return Promise.resolve(fallback ? fallback() : (undefined as T));
  }
  return tauriInvoke<T>(command, args);
}

export const api = {
  getConfig: () => invoke<AppConfig>("get_config", undefined, () => mockConfig),
  saveConfig: (payload: SaveConfigPayload) =>
    invoke<AppConfig>("save_config", { payload }, () => ({
      api_url: payload.api_url,
      model: payload.model,
      port: payload.port,
      has_api_key: Boolean(payload.api_key),
    })),
  testApiConnection: () => invoke<string>("test_api_connection", undefined, () => "Tauri 里才能测试连接哦"),
  startServer: () => invoke<ServerStatus>("start_mcp_server", undefined, () => mockStatus),
  stopServer: () => invoke<ServerStatus>("stop_mcp_server", undefined, () => mockStatus),
  getStatus: () => invoke<ServerStatus>("get_server_status", undefined, () => mockStatus),
  getLogs: () => invoke<LogSnapshot>("get_logs", undefined, () => ({ entries: [], dropped: 0 })),
  clearLogs: () => invoke<LogSnapshot>("clear_logs", undefined, () => ({ entries: [], dropped: 0 })),
  getTokenUsage: () =>
    invoke<TokenUsageSnapshot>("get_token_usage", undefined, () => emptyTokenUsage),
  clearTokenUsage: () =>
    invoke<TokenUsageSnapshot>("clear_token_usage", undefined, () => emptyTokenUsage),
  onLog: (handler: (entry: LogEntry) => void) =>
    isTauriRuntime()
      ? tauriListen<LogEntry>("log-entry", (event) => handler(event.payload))
      : Promise.resolve<Unlisten>(() => undefined),
  onTokenUsage: (handler: (snapshot: TokenUsageSnapshot) => void) =>
    isTauriRuntime()
      ? tauriListen<TokenUsageSnapshot>("token-usage-updated", (event) => handler(event.payload))
      : Promise.resolve<Unlisten>(() => undefined),
};
