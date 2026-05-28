import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

export type ServiceStatus = "stopped" | "starting" | "running" | "error";
export type LogLevel = "debug" | "info" | "warn" | "error";
export type AgentRuntime = "sdk" | "legacy";

export interface AppConfig {
  api_url: string;
  model: string;
  port: number;
  has_api_key: boolean;
  agent_runtime: AgentRuntime;
}

export interface SaveConfigPayload {
  api_url: string;
  api_key?: string;
  model: string;
  port: number;
  agent_runtime?: AgentRuntime;
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
  detail?: unknown | null;
}

export interface LogListEntry {
  id: number;
  time: string;
  level: LogLevel;
  source: string;
  request_id?: string | null;
  task_id?: string | null;
  summary: string;
  has_detail: boolean;
}

export interface LogStats {
  total: number;
  dropped: number;
  debug: number;
  info: number;
  warn: number;
  error: number;
  latest_id?: number | null;
}

export interface LogPage {
  entries: LogListEntry[];
  total: number;
  offset: number;
  limit: number;
  dropped: number;
  latest_id?: number | null;
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

export interface RuntimeStatsSnapshot {
  total_jobs: number;
  queued_jobs: number;
  running_jobs: number;
  succeeded_jobs: number;
  failed_jobs: number;
  cancelled_jobs: number;
  active_upstream_requests: number;
  agent_bridge_active_jobs: number;
  agent_bridge_waiting_first_response: number;
  logs_retained: number;
  logs_dropped: number;
  logs_pending: number;
  token_pending: number;
  token_updated_at?: string | null;
}

export interface AgentRuntimeStatus {
  runtime: AgentRuntime;
  bridge_started: boolean;
  sdk_version?: string | null;
  native_binary_path?: string | null;
  bridge_script?: string | null;
  node_executable: string;
  active_sessions: number;
  active_jobs: number;
  waiting_first_response: number;
  last_error?: string | null;
}

export type JobStatus = "queued" | "running" | "succeeded" | "failed" | "cancelled";

export interface JobSummary {
  job_id: string;
  root_job_id: string;
  parent_job_id?: string | null;
  session_id?: string | null;
  resumable: boolean;
  expires_at?: number | null;
  status: JobStatus;
  complete: boolean;
  cwd: string;
  prompt_preview: string;
  created_at: number;
  started_at?: number | null;
  ended_at?: number | null;
  output_recent: string;
  output_truncated: boolean;
  error?: string | null;
}

export interface ChatJobRecord {
  job_id: string;
  parent_job_id?: string | null;
  prompt: string;
  status: JobStatus | string;
  created_at: number;
  started_at?: number | null;
  ended_at?: number | null;
  output?: string | null;
  error?: string | null;
}

export interface ChatSessionSummary {
  root_job_id: string;
  latest_job_id: string;
  session_id?: string | null;
  workdir: string;
  status: JobStatus | string;
  created_at: number;
  updated_at: number;
  expires_at: number;
  active_job_id?: string | null;
  job_count: number;
  title: string;
  resumable: boolean;
  blocked_reason?: string | null;
}

export interface ChatSessionDetail extends ChatSessionSummary {
  codex_context: string;
  jobs: ChatJobRecord[];
}

export interface ChatSessionsSnapshot {
  sessions: ChatSessionSummary[];
  updated_at: number;
}

type Unlisten = () => void;

const mockConfig: AppConfig = {
  api_url: "https://api.anthropic.com",
  model: "claude-opus-4-7",
  port: 8765,
  has_api_key: false,
  agent_runtime: "sdk",
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

const emptyLogStats: LogStats = {
  total: 0,
  dropped: 0,
  debug: 0,
  info: 0,
  warn: 0,
  error: 0,
  latest_id: null,
};

const emptyLogPage: LogPage = {
  entries: [],
  total: 0,
  offset: 0,
  limit: 0,
  dropped: 0,
  latest_id: null,
};

const emptyRuntimeStats: RuntimeStatsSnapshot = {
  total_jobs: 0,
  queued_jobs: 0,
  running_jobs: 0,
  succeeded_jobs: 0,
  failed_jobs: 0,
  cancelled_jobs: 0,
  active_upstream_requests: 0,
  agent_bridge_active_jobs: 0,
  agent_bridge_waiting_first_response: 0,
  logs_retained: 0,
  logs_dropped: 0,
  logs_pending: 0,
  token_pending: 0,
  token_updated_at: null,
};

const emptyAgentRuntimeStatus: AgentRuntimeStatus = {
  runtime: "sdk",
  bridge_started: false,
  sdk_version: null,
  native_binary_path: null,
  bridge_script: null,
  node_executable: "node",
  active_sessions: 0,
  active_jobs: 0,
  waiting_first_response: 0,
  last_error: null,
};

const emptyChatSessions: ChatSessionsSnapshot = {
  sessions: [],
  updated_at: 0,
};

function toPageNumber(value: number): number {
  return Math.max(0, Math.floor(Number.isFinite(value) ? value : 0));
}

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
      agent_runtime: payload.agent_runtime ?? "sdk",
    })),
  testApiConnection: () => invoke<string>("test_api_connection", undefined, () => "Tauri 里才能测试连接哦"),
  startServer: () => invoke<ServerStatus>("start_mcp_server", undefined, () => mockStatus),
  stopServer: () => invoke<ServerStatus>("stop_mcp_server", undefined, () => mockStatus),
  getStatus: () => invoke<ServerStatus>("get_server_status", undefined, () => mockStatus),
  getRuntimeStats: () =>
    invoke<RuntimeStatsSnapshot>("get_runtime_stats", undefined, () => emptyRuntimeStats),
  getAgentRuntimeStatus: () =>
    invoke<AgentRuntimeStatus>(
      "get_agent_runtime_status",
      undefined,
      () => emptyAgentRuntimeStatus,
    ),
  getLogStats: (query = "") =>
    invoke<LogStats>("get_log_stats", { query }, () => emptyLogStats),
  getLogPage: (level: LogLevel | null, offset: number, limit: number, query = "") =>
    invoke<LogPage>(
      "get_log_page",
      { level, offset: toPageNumber(offset), limit: toPageNumber(limit), query },
      () => emptyLogPage,
    ),
  getLogDetail: (id: number) =>
    invoke<LogEntry>(
      "get_log_detail",
      { id },
      () =>
        ({
          id,
          time: "",
          level: "info",
          source: "",
          summary: "",
          detail: null,
        }) satisfies LogEntry,
    ),
  clearLogs: () => invoke<LogStats>("clear_logs", undefined, () => emptyLogStats),
  getTokenUsage: () =>
    invoke<TokenUsageSnapshot>("get_token_usage", undefined, () => emptyTokenUsage),
  clearTokenUsage: () =>
    invoke<TokenUsageSnapshot>("clear_token_usage", undefined, () => emptyTokenUsage),
  getChatSessions: () =>
    invoke<ChatSessionsSnapshot>("get_chat_sessions", undefined, () => emptyChatSessions),
  getChatSession: (jobId: string, limit?: number) =>
    invoke<ChatSessionDetail>(
      "get_chat_session",
      { jobId, limit },
      () => ({
        root_job_id: jobId,
        latest_job_id: jobId,
        session_id: null,
        workdir: "",
        status: "succeeded",
        created_at: 0,
        updated_at: 0,
        expires_at: 0,
        active_job_id: null,
        job_count: 0,
        title: "本地预览",
        resumable: false,
        blocked_reason: "Tauri 运行时不可用",
        codex_context: "Tauri 运行时不可用",
        jobs: [],
      }),
    ),
  sendChatMessage: (jobId: string, prompt: string, workdir?: string) =>
    invoke<JobSummary>(
      "send_chat_message",
      { jobId, prompt, workdir },
      () => ({
        job_id: "preview-job",
        root_job_id: jobId,
        parent_job_id: jobId,
        session_id: null,
        resumable: false,
        expires_at: null,
        status: "queued",
        complete: false,
        cwd: workdir || "",
        prompt_preview: prompt,
        created_at: Date.now(),
        started_at: null,
        ended_at: null,
        output_recent: "",
        output_truncated: false,
        error: null,
      }),
    ),
  stopChatSession: (jobId: string) =>
    invoke<ChatSessionsSnapshot>("stop_chat_session", { jobId }, () => emptyChatSessions),
  deleteChatSession: (jobId: string) =>
    invoke<ChatSessionsSnapshot>("delete_chat_session", { jobId }, () => emptyChatSessions),
  onLogStatsUpdated: (handler: () => void) =>
    isTauriRuntime()
      ? tauriListen("log-stats-updated", () => handler())
      : Promise.resolve<Unlisten>(() => undefined),
  onTokenUsage: (handler: (snapshot: TokenUsageSnapshot) => void) =>
    isTauriRuntime()
      ? tauriListen<TokenUsageSnapshot>("token-usage-updated", (event) => handler(event.payload))
      : Promise.resolve<Unlisten>(() => undefined),
  onServerStatus: (handler: (status: ServerStatus) => void) =>
    isTauriRuntime()
      ? tauriListen<ServerStatus>("server-status-updated", (event) => handler(event.payload))
      : Promise.resolve<Unlisten>(() => undefined),
  onRuntimeStats: (handler: () => void) =>
    isTauriRuntime()
      ? tauriListen("runtime-stats-updated", () => handler())
      : Promise.resolve<Unlisten>(() => undefined),
  onChatSessions: (handler: (snapshot: ChatSessionsSnapshot) => void) =>
    isTauriRuntime()
      ? tauriListen<ChatSessionsSnapshot>("chat-sessions-updated", (event) => handler(event.payload))
      : Promise.resolve<Unlisten>(() => undefined),
};
