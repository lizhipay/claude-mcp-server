import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { InputHTMLAttributes, ReactNode } from "react";
import {
  AlertTriangle,
  Anchor,
  ArrowDown,
  ArrowUp,
  BarChart3,
  Check,
  Copy,
  Database,
  Eye,
  EyeOff,
  FileText,
  Globe,
  Hash,
  KeyRound,
  LayoutDashboard,
  Play,
  RefreshCw,
  Save,
  ScrollText,
  Server,
  Settings2,
  Square,
  Tag,
  Trash2,
  Zap,
} from "lucide-react";
import type {
  AppConfig,
  LogEntry,
  LogListEntry,
  LogLevel,
  LogPage,
  LogStats,
  ServerStatus,
  TokenUsageSnapshot,
} from "./tauri";
import { api } from "./tauri";
import {
  formatLogDetail,
  getLogCountForLevel,
  getVirtualLogWindow,
  LOG_ROW_HEIGHT,
  type LogLevelFilter,
} from "./log-utils";

const defaultConfig: AppConfig = {
  api_url: "https://api.anthropic.com",
  model: "claude-opus-4-7",
  port: 8765,
  has_api_key: false,
};

const defaultStatus: ServerStatus = {
  status: "stopped",
  mcp_url: null,
  health_url: null,
  message: "已停止",
};

const defaultUsage: TokenUsageSnapshot = {
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

const logLevelOptions: LogLevelFilter[] = ["all", "debug", "info", "warn", "error"];

const filterLabel: Record<LogLevelFilter, string> = {
  all: "全部",
  debug: "Debug",
  info: "Info",
  warn: "Warn",
  error: "Error",
};

const statusCopy = {
  stopped: { label: "已停止", className: "idle" },
  starting: { label: "启动中...", className: "starting" },
  running: { label: "运行中", className: "running" },
  error: { label: "异常", className: "error" },
} as const;

type ActiveTab = "main" | "usage" | "logs";

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

function App() {
  const [config, setConfig] = useState<AppConfig>(defaultConfig);
  const [apiKey, setApiKey] = useState("");
  const [status, setStatus] = useState<ServerStatus>(defaultStatus);
  const [logStats, setLogStats] = useState<LogStats>(emptyLogStats);
  const [logPage, setLogPage] = useState<LogPage>(emptyLogPage);
  const [usage, setUsage] = useState<TokenUsageSnapshot>(defaultUsage);
  const [activeTab, setActiveTab] = useState<ActiveTab>("main");
  const [level, setLevel] = useState<LogLevelFilter>("all");
  const [autoScroll, setAutoScroll] = useState(true);
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [toast, setToast] = useState("");
  const [error, setError] = useState("");
  const [logScrollTop, setLogScrollTop] = useState(0);
  const [logViewportHeight, setLogViewportHeight] = useState(0);
  const [selectedLog, setSelectedLog] = useState<LogListEntry | null>(null);
  const [selectedLogDetail, setSelectedLogDetail] = useState<LogEntry | null>(null);
  const [logDetailLoading, setLogDetailLoading] = useState(false);
  const [logDetailError, setLogDetailError] = useState("");
  const logListRef = useRef<HTMLDivElement | null>(null);

  const activeLogTotal = getLogCountForLevel(logStats, level);
  const logWindow = useMemo(
    () => getVirtualLogWindow(logScrollTop, logViewportHeight, activeLogTotal),
    [activeLogTotal, logScrollTop, logViewportHeight],
  );

  const refreshLogStats = useCallback(async () => {
    setLogStats(await api.getLogStats());
  }, []);

  const refreshUsage = useCallback(async () => {
    setUsage(await api.getTokenUsage());
  }, []);

  const showToast = useCallback((message: string) => {
    setToast(message);
    window.setTimeout(() => setToast(""), 1600);
  }, []);

  const handleError = useCallback((message: unknown) => {
    setError(message instanceof Error ? message.message : String(message));
  }, []);

  useEffect(() => {
    api
      .getConfig()
      .then(setConfig)
      .catch(handleError);
    api
      .getStatus()
      .then(setStatus)
      .catch(handleError);
    refreshLogStats().catch(handleError);
    refreshUsage().catch(handleError);

    const unlistenLogs = api.onLogStatsUpdated(() => {
      refreshLogStats().catch(handleError);
    });
    const unlistenUsage = api.onTokenUsage(setUsage);
    const unlistenStatus = api.onServerStatus(setStatus);
    return () => {
      unlistenLogs.then((dispose) => dispose()).catch(() => undefined);
      unlistenUsage.then((dispose) => dispose()).catch(() => undefined);
      unlistenStatus.then((dispose) => dispose()).catch(() => undefined);
    };
  }, [handleError, refreshLogStats, refreshUsage]);

  useEffect(() => {
    if (activeTab !== "logs") return;
    refreshLogStats().catch(handleError);
  }, [activeTab, handleError, refreshLogStats]);

  useEffect(() => {
    if (activeTab !== "logs") return;
    const element = logListRef.current;
    if (!element) return;

    const updateSize = () => setLogViewportHeight(element.clientHeight);
    updateSize();
    const observer = new ResizeObserver(updateSize);
    observer.observe(element);
    return () => observer.disconnect();
  }, [activeTab]);

  useEffect(() => {
    if (activeTab !== "logs") return;
    if (activeLogTotal === 0 || logWindow.limit === 0) {
      setLogPage(emptyLogPage);
      return;
    }

    let cancelled = false;
    const selectedLevel = level === "all" ? null : level;
    api
      .getLogPage(selectedLevel, logWindow.offset, logWindow.limit)
      .then((page) => {
        if (!cancelled) setLogPage(page);
      })
      .catch((err) => {
        if (!cancelled) handleError(err);
      });
    return () => {
      cancelled = true;
    };
  }, [
    activeLogTotal,
    activeTab,
    handleError,
    level,
    logStats.latest_id,
    logWindow.limit,
    logWindow.offset,
  ]);

  useEffect(() => {
    if (activeTab !== "logs" || !autoScroll || !logListRef.current) return;
    const nextScrollTop = Math.max(
      0,
      activeLogTotal * LOG_ROW_HEIGHT - logListRef.current.clientHeight,
    );
    logListRef.current.scrollTop = nextScrollTop;
    setLogScrollTop(nextScrollTop);
  }, [activeLogTotal, activeTab, autoScroll]);

  useEffect(() => {
    setSelectedLog(null);
    setSelectedLogDetail(null);
    setLogDetailError("");
    if (!autoScroll && logListRef.current) {
      logListRef.current.scrollTop = 0;
      setLogScrollTop(0);
    }
  }, [level, autoScroll]);

  useEffect(() => {
    if (!selectedLog) return;
    setSelectedLogDetail(null);
    setLogDetailError("");
    if (!selectedLog.has_detail) {
      setLogDetailLoading(false);
      return;
    }

    let cancelled = false;
    setLogDetailLoading(true);
    api
      .getLogDetail(selectedLog.id)
      .then((entry) => {
        if (!cancelled) setSelectedLogDetail(entry);
      })
      .catch((err) => {
        if (!cancelled) setLogDetailError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLogDetailLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [selectedLog]);

  async function saveConfig() {
    setBusy(true);
    setError("");
    try {
      const saved = await api.saveConfig({
        api_url: config.api_url,
        api_key: apiKey || undefined,
        model: config.model,
        port: Number(config.port),
      });
      setConfig(saved);
      setApiKey("");
      showToast("已保存");
    } catch (err) {
      handleError(err);
    } finally {
      setBusy(false);
    }
  }

  async function testConnection() {
    setBusy(true);
    setError("");
    try {
      const message = await api.testApiConnection();
      await refreshUsage();
      showToast(message || "连接成功");
    } catch (err) {
      handleError(err);
    } finally {
      setBusy(false);
    }
  }

  async function toggleServer() {
    setBusy(true);
    setError("");
    try {
      setStatus((current) => ({ ...current, status: "starting", message: "启动中..." }));
      const next =
        status.status === "running" ? await api.stopServer() : await api.startServer();
      setStatus(next);
      await refreshLogStats();
    } catch (err) {
      setStatus((current) => ({ ...current, status: "error", message: "启动失败" }));
      handleError(err);
    } finally {
      setBusy(false);
    }
  }

  async function copyMcpUrl() {
    if (!status.mcp_url) return;
    await navigator.clipboard.writeText(status.mcp_url);
    setCopied(true);
    showToast("已复制");
    window.setTimeout(() => setCopied(false), 1500);
  }

  async function clearLogs() {
    const stats = await api.clearLogs();
    setLogStats(stats);
    setLogPage(emptyLogPage);
    setSelectedLog(null);
    setSelectedLogDetail(null);
    setLogDetailError("");
    if (logListRef.current) {
      logListRef.current.scrollTop = 0;
    }
    setLogScrollTop(0);
  }

  async function clearTokenUsage() {
    setBusy(true);
    setError("");
    try {
      setUsage(await api.clearTokenUsage());
      showToast("统计已清空");
    } catch (err) {
      handleError(err);
    } finally {
      setBusy(false);
    }
  }

  const statusMeta = statusCopy[status.status];
  const canCopy = Boolean(status.mcp_url);

  return (
    <main className="app-shell">
      <header className="titlebar">
        <div className="brand">
          <BrandMark />
          <h1>Claude MCP</h1>
        </div>
        <nav className="tab-switch segmented" aria-label="页面切换" role="tablist">
          <button
            className={activeTab === "main" ? "active" : ""}
            role="tab"
            aria-selected={activeTab === "main"}
            onClick={() => setActiveTab("main")}
          >
            <LayoutDashboard size={15} />
            主控台
          </button>
          <button
            className={activeTab === "usage" ? "active" : ""}
            role="tab"
            aria-selected={activeTab === "usage"}
            onClick={() => setActiveTab("usage")}
          >
            <BarChart3 size={15} />
            用量统计
          </button>
          <button
            className={activeTab === "logs" ? "active" : ""}
            role="tab"
            aria-selected={activeTab === "logs"}
            onClick={() => setActiveTab("logs")}
          >
            <ScrollText size={15} />
            运行日志
          </button>
        </nav>
      </header>

      <div className="tab-panels">
        {activeTab === "main" ? (
          <div className="tab-panel main-panel" role="tabpanel" aria-label="主控台">
            <section className="card workspace-card" aria-label="配置与服务">
              <div className="section-title">
                <Settings2 size={18} />
                <h2>连接配置</h2>
              </div>

              <div className="field-grid">
                <LabeledInput
                  icon={<Globe size={15} />}
                  label="API 地址"
                  value={config.api_url}
                  placeholder="https://api.anthropic.com"
                  onChange={(api_url) => setConfig((current) => ({ ...current, api_url }))}
                />
                <LabeledInput
                  icon={<KeyRound size={15} />}
                  label="API 密钥"
                  value={apiKey}
                  type="password"
                  placeholder={config.has_api_key ? "已保存于本地配置" : "sk-ant-..."}
                  onChange={setApiKey}
                />
                <LabeledInput
                  icon={<Tag size={15} />}
                  label="模型名称"
                  value={config.model}
                  placeholder="claude-opus-4-7"
                  onChange={(model) => setConfig((current) => ({ ...current, model }))}
                />
                <LabeledInput
                  icon={<Anchor size={15} />}
                  label="端口号"
                  value={String(config.port)}
                  inputMode="numeric"
                  placeholder="8765"
                  onChange={(port) =>
                    setConfig((current) => ({
                      ...current,
                      port: Number(port.replace(/\D/g, "")) || 0,
                    }))
                  }
                />
              </div>

              <div className="button-row">
                <button className="soft-button" disabled={busy} onClick={saveConfig}>
                  <Save size={15} />
                  保存
                </button>
                <button className="soft-button ghost" disabled={busy} onClick={testConnection}>
                  <Zap size={15} />
                  测试连接
                </button>
              </div>

              <div className="section-divider" />

              <div className="service-panel">
                <div className="service-head">
                  <div className="service-title">
                    <Server size={18} />
                    <h2>MCP 服务</h2>
                  </div>
                  <span className={`status-pill ${statusMeta.className}`}>
                    <span className="pulse" />
                    {statusMeta.label}
                  </span>
                </div>

                <div className="mcp-address">
                  <span className="address-label">MCP 地址</span>
                  <code>{status.mcp_url ?? "--"}</code>
                  <button
                    className="icon-button"
                    type="button"
                    title="复制 MCP 地址"
                    disabled={!canCopy}
                    onClick={copyMcpUrl}
                  >
                    {copied ? <Check size={16} /> : <Copy size={16} />}
                  </button>
                </div>

                <div className="button-row service-actions">
                  <button
                    className={`primary-button ${status.status === "running" ? "danger" : ""}`}
                    disabled={busy || status.status === "starting"}
                    onClick={toggleServer}
                  >
                    {status.status === "running" ? <Square size={15} /> : <Play size={15} />}
                    {status.status === "running" ? "停止服务" : "启动服务"}
                  </button>
                  <button className="soft-button ghost" onClick={refreshLogStats}>
                    <RefreshCw size={15} />
                    刷新
                  </button>
                </div>
              </div>
            </section>

            {error ? (
              <aside className="error-card" role="alert">
                <AlertTriangle size={18} />
                <p>{error}</p>
                <button onClick={() => setError("")}>关闭</button>
              </aside>
            ) : null}
          </div>
        ) : activeTab === "usage" ? (
          <UsagePanel
            usage={usage}
            busy={busy}
            onRefresh={refreshUsage}
            onClear={clearTokenUsage}
          />
        ) : (
          <section className="card log-panel" role="tabpanel" aria-label="运行日志">
            <div className="log-panel-head">
              <div className="section-title compact">
                <ScrollText size={18} />
                <h2>运行日志</h2>
              </div>
              <div className="log-head-meta">
                <span>
                  显示 {activeLogTotal.toLocaleString()} / {logStats.total.toLocaleString()}
                </span>
                {logStats.dropped > 0 ? <span className="sweep-note">早期日志已清理</span> : null}
              </div>
            </div>

            <div className="log-toolbar">
              <div className="segmented level-tabs" role="tablist" aria-label="日志级别">
                {logLevelOptions.map((option) => (
                  <button
                    key={option}
                    className={level === option ? "active" : ""}
                    role="tab"
                    aria-selected={level === option}
                    onClick={() => setLevel(option)}
                  >
                    {filterLabel[option]}
                  </button>
                ))}
              </div>
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={autoScroll}
                  onChange={(event) => setAutoScroll(event.currentTarget.checked)}
                />
                自动滚动
              </label>
              <button className="soft-button tiny" onClick={clearLogs}>
                <Trash2 size={14} />
                清空
              </button>
            </div>

            <div
              className="log-list"
              ref={logListRef}
              onScroll={(event) => setLogScrollTop(event.currentTarget.scrollTop)}
            >
              {activeLogTotal === 0 ? (
                <div className="empty-log">
                  <FileText size={34} />
                  <p>暂无日志</p>
                </div>
              ) : (
                <div className="log-spacer" style={{ height: logWindow.totalHeight }}>
                  <div
                    className="log-window"
                    style={{ transform: `translateY(${logWindow.translateY}px)` }}
                  >
                    {logPage.entries.map((entry) => (
                      <LogRow
                        key={entry.id}
                        entry={entry}
                        selected={selectedLog?.id === entry.id}
                        onSelect={() =>
                          setSelectedLog((current) =>
                            current?.id === entry.id ? null : entry,
                          )
                        }
                      />
                    ))}
                  </div>
                </div>
              )}
            </div>
            {selectedLog ? (
              <LogDetailPane
                entry={selectedLog}
                detail={selectedLogDetail}
                loading={logDetailLoading}
                error={logDetailError}
                onClose={() => setSelectedLog(null)}
              />
            ) : null}
          </section>
        )}
      </div>

      {toast ? <div className="toast">{toast}</div> : null}
    </main>
  );
}

function UsagePanel({
  usage,
  busy,
  onRefresh,
  onClear,
}: {
  usage: TokenUsageSnapshot;
  busy: boolean;
  onRefresh: () => Promise<void>;
  onClear: () => Promise<void>;
}) {
  const cacheTokens = usage.totals.cache_read_tokens + usage.totals.cache_write_tokens;
  const cards = [
    {
      label: "全部用量",
      value: usage.totals.total_tokens,
      meta: `${formatNumber(usage.totals.requests)} 次请求`,
      icon: <Hash size={17} />,
    },
    {
      label: "输入",
      value: usage.totals.input_tokens,
      meta: "Input",
      icon: <ArrowDown size={17} />,
    },
    {
      label: "输出",
      value: usage.totals.output_tokens,
      meta: "Output",
      icon: <ArrowUp size={17} />,
    },
    {
      label: "缓存",
      value: cacheTokens,
      meta: `读 ${formatNumber(usage.totals.cache_read_tokens)} / 写 ${formatNumber(
        usage.totals.cache_write_tokens,
      )}`,
      icon: <Database size={17} />,
    },
  ];

  return (
    <section className="card usage-panel" role="tabpanel" aria-label="用量统计">
      <div className="usage-head">
        <div className="section-title compact">
          <BarChart3 size={18} />
          <h2>用量统计</h2>
        </div>
        {usage.updated_at ? <span className="usage-updated">{formatUpdatedAt(usage.updated_at)}</span> : null}
      </div>

      <div className="usage-summary">
        {cards.map((card) => (
          <article className="usage-summary-card" key={card.label}>
            <div className="usage-summary-icon">{card.icon}</div>
            <div>
              <p>{card.label}</p>
              <strong>{formatNumber(card.value)}</strong>
              <span>{card.meta}</span>
            </div>
          </article>
        ))}
      </div>

      <div className="usage-table-wrap">
        <table className="usage-table">
          <thead>
            <tr>
              <th>日期</th>
              <th>请求</th>
              <th>输入</th>
              <th>输出</th>
              <th>缓存读</th>
              <th>缓存写</th>
              <th>合计</th>
            </tr>
          </thead>
          <tbody>
            {usage.days.length === 0 ? (
              <tr className="usage-empty-row">
                <td colSpan={7}>
                  <div className="empty-log usage-empty">
                    <FileText size={34} />
                    <p>暂无用量数据</p>
                  </div>
                </td>
              </tr>
            ) : (
              usage.days.map((day) => (
                <tr key={day.date}>
                  <td>{day.date}</td>
                  <td>{formatNumber(day.requests)}</td>
                  <td>{formatNumber(day.input_tokens)}</td>
                  <td>{formatNumber(day.output_tokens)}</td>
                  <td>{formatNumber(day.cache_read_tokens)}</td>
                  <td>{formatNumber(day.cache_write_tokens)}</td>
                  <td>{formatNumber(day.total_tokens)}</td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>

      <div className="button-row usage-actions">
        <button className="soft-button ghost" disabled={busy} onClick={onRefresh}>
          <RefreshCw size={15} />
          刷新
        </button>
        <button
          className="soft-button ghost"
          disabled={busy || usage.totals.requests === 0}
          onClick={onClear}
        >
          <Trash2 size={15} />
          清空统计
        </button>
      </div>
    </section>
  );
}

function LabeledInput({
  icon,
  label,
  value,
  placeholder,
  type = "text",
  inputMode,
  onChange,
}: {
  icon: ReactNode;
  label: string;
  value: string;
  placeholder: string;
  type?: string;
  inputMode?: InputHTMLAttributes<HTMLInputElement>["inputMode"];
  onChange: (value: string) => void;
}) {
  const [visible, setVisible] = useState(false);
  const realType = type === "password" && !visible ? "password" : "text";
  return (
    <label className="field">
      <span className="field-label">
        {icon}
        {label}
      </span>
      <span className="input-shell">
        <input
          type={realType}
          value={value}
          inputMode={inputMode}
          placeholder={placeholder}
          onChange={(event) => onChange(event.currentTarget.value)}
        />
        {type === "password" ? (
          <button
            type="button"
            className="peek-button"
            title={visible ? "隐藏密钥" : "显示密钥"}
            onClick={() => setVisible((v) => !v)}
          >
            {visible ? <EyeOff size={16} /> : <Eye size={16} />}
          </button>
        ) : null}
      </span>
    </label>
  );
}

function formatNumber(value: number) {
  return new Intl.NumberFormat("zh-CN").format(value || 0);
}

function formatUpdatedAt(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return "";
  }
  return `更新于 ${date.toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
  })}`;
}

function LogRow({
  entry,
  selected,
  onSelect,
}: {
  entry: LogListEntry;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      className={`log-row ${entry.level}${selected ? " selected" : ""}`}
      type="button"
      onClick={onSelect}
      title={entry.has_detail ? "查看详情" : "暂无详情"}
    >
      <span className="log-time">{entry.time}</span>
      <span className="log-source">{entry.source}</span>
      <span className="log-summary">{entry.summary}</span>
    </button>
  );
}

function LogDetailPane({
  entry,
  detail,
  loading,
  error,
  onClose,
}: {
  entry: LogListEntry;
  detail: LogEntry | null;
  loading: boolean;
  error: string;
  onClose: () => void;
}) {
  const content = useMemo(() => {
    if (loading) return "加载中...";
    if (error) return error;
    if (!entry.has_detail) return "暂无详情";
    return formatLogDetail(detail?.detail);
  }, [detail, entry.has_detail, error, loading]);

  return (
    <aside className="log-detail-pane">
      <header>
        <span>{entry.time}</span>
        <strong>{entry.source}</strong>
        <button type="button" onClick={onClose}>
          关闭
        </button>
      </header>
      <pre>{content}</pre>
    </aside>
  );
}

function BrandMark() {
  return (
    <svg className="brand-mark" viewBox="0 0 96 96" aria-hidden="true">
      <defs>
        <linearGradient id="brand-mark-bg" x1="14" y1="12" x2="82" y2="86">
          <stop offset="0" stopColor="#c9b9ff" />
          <stop offset="1" stopColor="#8f7ad6" />
        </linearGradient>
        <linearGradient id="brand-mark-line" x1="22" y1="20" x2="74" y2="76">
          <stop offset="0" stopColor="#3f3559" />
          <stop offset="1" stopColor="#2b2440" />
        </linearGradient>
      </defs>
      <rect x="8" y="8" width="80" height="80" rx="24" fill="url(#brand-mark-bg)" />
      <path
        className="brand-mark-face"
        d="M28 39 32 24l13 11h6l13-11 4 15c6 5 9 12 8 21-2 13-13 21-28 21s-26-8-28-21c-1-9 2-16 8-21Z"
      />
      <path
        className="brand-mark-line"
        d="M29 41 32 25l13 11h6l13-11 3 16"
      />
      <path
        className="brand-mark-line"
        d="M22 53c0-17 11-29 26-29s26 12 26 29"
      />
      <rect className="brand-mark-accent" x="18" y="51" width="7" height="16" rx="3" />
      <rect className="brand-mark-accent" x="71" y="51" width="7" height="16" rx="3" />
      <path className="brand-mark-line thin" d="M38 55h20M41 64h14" />
    </svg>
  );
}

export default App;
