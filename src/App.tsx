import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { InputHTMLAttributes, ReactNode } from "react";
import {
  AlertTriangle,
  Anchor,
  ArrowDown,
  ArrowUp,
  BarChart3,
  Check,
  CircleAlert,
  Copy,
  Database,
  Eye,
  EyeOff,
  FileText,
  Globe,
  Hash,
  Info,
  KeyRound,
  LayoutDashboard,
  Play,
  RefreshCw,
  Save,
  ScrollText,
  Server,
  Settings2,
  ShieldCheck,
  Sparkles,
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
  RuntimeStatsSnapshot,
  ServerStatus,
  TokenUsageSnapshot,
} from "./tauri";
import { api } from "./tauri";
import mascotAvatar from "./assets/reference/brand-avatar.png";
import mascotBunny from "./assets/reference/service-bunny.png";
import mascotPeek from "./assets/reference/main-peek.png";
import mascotUsage from "./assets/reference/usage-peek.png";
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
  agent_runtime: "sdk",
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

const defaultRuntimeStats: RuntimeStatsSnapshot = {
  total_jobs: 0,
  queued_jobs: 0,
  running_jobs: 0,
  succeeded_jobs: 0,
  failed_jobs: 0,
  cancelled_jobs: 0,
  active_upstream_requests: 0,
  logs_retained: 0,
  logs_dropped: 0,
  logs_pending: 0,
  token_pending: 0,
  token_updated_at: null,
};

function App() {
  const [config, setConfig] = useState<AppConfig>(defaultConfig);
  const [apiKey, setApiKey] = useState("");
  const [lastSavedAt, setLastSavedAt] = useState("");
  const [status, setStatus] = useState<ServerStatus>(defaultStatus);
  const [logStats, setLogStats] = useState<LogStats>(emptyLogStats);
  const [logPage, setLogPage] = useState<LogPage>(emptyLogPage);
  const [usage, setUsage] = useState<TokenUsageSnapshot>(defaultUsage);
  const [runtimeStats, setRuntimeStats] = useState<RuntimeStatsSnapshot>(defaultRuntimeStats);
  const [activeTab, setActiveTab] = useState<ActiveTab>("main");
  const [level, setLevel] = useState<LogLevelFilter>("all");
  const [isLogHovered, setIsLogHovered] = useState(false);
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

  const refreshStatus = useCallback(async () => {
    setStatus(await api.getStatus());
  }, []);

  const refreshRuntimeStats = useCallback(async () => {
    setRuntimeStats(await api.getRuntimeStats());
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
    refreshRuntimeStats().catch(handleError);

    const unlistenLogs = api.onLogStatsUpdated(() => {
      refreshLogStats().catch(handleError);
      refreshRuntimeStats().catch(handleError);
    });
    const unlistenUsage = api.onTokenUsage((snapshot) => {
      setUsage(snapshot);
      refreshRuntimeStats().catch(handleError);
    });
    const unlistenStatus = api.onServerStatus(setStatus);
    const unlistenRuntime = api.onRuntimeStats(() => {
      refreshRuntimeStats().catch(handleError);
    });
    return () => {
      unlistenLogs.then((dispose) => dispose()).catch(() => undefined);
      unlistenUsage.then((dispose) => dispose()).catch(() => undefined);
      unlistenStatus.then((dispose) => dispose()).catch(() => undefined);
      unlistenRuntime.then((dispose) => dispose()).catch(() => undefined);
    };
  }, [handleError, refreshLogStats, refreshRuntimeStats, refreshUsage]);

  useEffect(() => {
    if (activeTab !== "logs") return;
    refreshLogStats().catch(handleError);
  }, [activeTab, handleError, refreshLogStats]);

  useEffect(() => {
    if (activeTab !== "usage") return;
    refreshUsage().catch(handleError);
  }, [activeTab, handleError, refreshUsage]);

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
    if (activeTab !== "logs" || isLogHovered || !logListRef.current) return;
    const nextScrollTop = Math.max(
      0,
      activeLogTotal * LOG_ROW_HEIGHT - logListRef.current.clientHeight,
    );
    logListRef.current.scrollTop = nextScrollTop;
    setLogScrollTop(nextScrollTop);
  }, [activeLogTotal, activeTab, isLogHovered]);

  useEffect(() => {
    setSelectedLog(null);
    setSelectedLogDetail(null);
    setLogDetailError("");
  }, [level]);

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
      setLastSavedAt(
        new Date().toLocaleTimeString("zh-CN", {
          hour: "2-digit",
          minute: "2-digit",
          second: "2-digit",
          hour12: false,
        }),
      );
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

  const canCopy = Boolean(status.mcp_url);

  return (
    <main className="app-shell">
      <header className="hero-header">
        <div className="brand-lockup">
          <img className="brand-avatar" src={mascotAvatar} alt="" />
          <div className="brand-copy">
            <h1>
              Claude
              <br />
              MCP
            </h1>
            <span className="star star-a" />
          </div>
        </div>
        <nav className="tab-switch" aria-label="页面切换" role="tablist">
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
            <section className="glass-card config-card" aria-label="连接配置">
              <img className="peek-mascot" src={mascotPeek} alt="" />
              <div className="section-heading">
                <Settings2 size={20} />
                <div>
                  <h2>连接配置</h2>
                  <p>配置 MCP 服务连接信息，确保服务正常运行</p>
                </div>
              </div>

              <div className="form-card">
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
                  placeholder={config.has_api_key ? "••••••••••••••••••••••••" : "sk-ant-..."}
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
                  trailing={<Hash size={15} />}
                  onChange={(port) =>
                    setConfig((current) => ({
                      ...current,
                      port: Number(port.replace(/\D/g, "")) || 0,
                    }))
                  }
                />
                <div className="config-actions">
                  <button className="primary-button violet" disabled={busy} onClick={saveConfig}>
                    <Save size={17} />
                    保存配置
                  </button>
                  <button className="soft-button outline" disabled={busy} onClick={testConnection}>
                    <Zap size={17} />
                    测试连接
                  </button>
                  <div className="save-state">
                    <span className="state-dot" />
                    <strong>配置已保存</strong>
                    <small>{lastSavedAt ? `上次保存于 ${lastSavedAt}` : "本地配置已同步"}</small>
                  </div>
                </div>
              </div>
            </section>

            <section className="glass-card service-card" aria-label="MCP 服务">
              <img className="bunny-mascot" src={mascotBunny} alt="" />
              <div className="service-card-head">
                <div className="section-heading tight">
                  <Server size={21} />
                  <div>
                    <h2>MCP 服务</h2>
                    <p>管理 MCP 服务的运行状态</p>
                  </div>
                </div>
              </div>

              <div className="address-card">
                <div className="address-title">
                  <Globe size={14} />
                  <strong>MCP 地址</strong>
                  <span>本地 MCP 服务访问地址</span>
                </div>
                <div className="address-line">
                  <code>{status.mcp_url ?? "--"}</code>
                  <button
                    className="copy-button"
                    type="button"
                    title="复制 MCP 地址"
                    disabled={!canCopy}
                    onClick={copyMcpUrl}
                  >
                    {copied ? <Check size={16} /> : <Copy size={16} />}
                    复制
                  </button>
                </div>
                <div className="health-line">
                  <ShieldCheck size={15} />
                  {status.status === "running" ? "服务健康运行中，所有系统正常" : status.message}
                </div>
                <RuntimeStatsStrip stats={runtimeStats} />
              </div>

              <div className="service-actions">
                <button
                  className={`primary-button wide ${status.status === "running" ? "danger" : "violet"}`}
                  disabled={busy || status.status === "starting"}
                  onClick={toggleServer}
                >
                  {status.status === "running" ? <Square size={16} /> : <Play size={16} />}
                  {status.status === "running" ? "停止服务" : "启动服务"}
                </button>
                <button className="soft-button wide outline" onClick={refreshStatus}>
                  <RefreshCw size={17} />
                  刷新状态
                </button>
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
          <UsagePanel usage={usage} busy={busy} onClear={clearTokenUsage} />
        ) : (
          <section className="glass-card log-panel" role="tabpanel" aria-label="运行日志">
            <div className="log-panel-head">
              <div className="section-heading tight">
                <ScrollText size={21} />
                <div>
                  <h2>运行日志</h2>
                  <p>查看 MCP 服务的运行日志和事件记录</p>
                </div>
              </div>
              <div className="log-head-actions">
                {logStats.dropped > 0 ? <span className="sweep-note">早期日志已清理</span> : null}
                <button className="soft-button tiny outline" onClick={clearLogs}>
                  <Trash2 size={14} />
                  清空
                </button>
              </div>
            </div>

            <div className="log-toolbar">
              <div className="level-tabs" role="tablist" aria-label="日志级别">
                {logLevelOptions.map((option) => (
                  <button
                    key={option}
                    className={level === option ? "active" : ""}
                    role="tab"
                    aria-selected={level === option}
                    onClick={() => setLevel(option)}
                  >
                    {option === "all" ? <LayoutDashboard size={14} /> : null}
                    {option === "debug" ? <Sparkles size={14} /> : null}
                    {option === "info" ? <Info size={14} /> : null}
                    {option === "warn" ? <AlertTriangle size={14} /> : null}
                    {option === "error" ? <CircleAlert size={14} /> : null}
                    {filterLabel[option]}({getLogCountForLevel(logStats, option).toLocaleString()})
                  </button>
                ))}
              </div>
            </div>

            <div
              className="log-list"
              ref={logListRef}
              onScroll={(event) => setLogScrollTop(event.currentTarget.scrollTop)}
              onMouseEnter={() => setIsLogHovered(true)}
              onMouseLeave={() => setIsLogHovered(false)}
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
                          setSelectedLog((current) => (current?.id === entry.id ? null : entry))
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

function RuntimeStatsStrip({ stats }: { stats: RuntimeStatsSnapshot }) {
  const done = stats.succeeded_jobs + stats.failed_jobs + stats.cancelled_jobs;
  const items = [
    ["运行任务", stats.running_jobs],
    ["上游请求", stats.active_upstream_requests],
    ["已完成", done],
    ["日志待写", stats.logs_pending],
    ["Token 待写", stats.token_pending],
  ];
  return (
    <div className="runtime-strip" aria-label="运行态统计">
      {items.map(([label, value]) => (
        <span key={label}>
          <small>{label}</small>
          <strong>{formatNumber(Number(value))}</strong>
        </span>
      ))}
    </div>
  );
}

function UsagePanel({
  usage,
  busy,
  onClear,
}: {
  usage: TokenUsageSnapshot;
  busy: boolean;
  onClear: () => Promise<void>;
}) {
  const cacheTokens = usage.totals.cache_read_tokens + usage.totals.cache_write_tokens;
  const today = getUsageDay(usage, 0);
  const yesterday = getUsageDay(usage, 1);
  const cards = [
    {
      tone: "total",
      label: "全部用量",
      value: usage.totals.total_tokens,
      detail: `${formatNumber(usage.totals.requests)} 次请求`,
      trend: formatTrend(today?.total_tokens, yesterday?.total_tokens),
      icon: <Hash size={17} />,
      wave: true,
    },
    {
      tone: "input",
      label: "输入",
      value: usage.totals.input_tokens,
      detail: "Input",
      trend: formatTrend(today?.input_tokens, yesterday?.input_tokens),
      icon: <ArrowDown size={17} />,
    },
    {
      tone: "output",
      label: "输出",
      value: usage.totals.output_tokens,
      detail: "Output",
      trend: formatTrend(today?.output_tokens, yesterday?.output_tokens),
      icon: <ArrowUp size={17} />,
    },
    {
      tone: "cache",
      label: "缓存",
      value: cacheTokens,
      detail: "",
      trend: formatTrend(
        today ? today.cache_read_tokens + today.cache_write_tokens : 0,
        yesterday ? yesterday.cache_read_tokens + yesterday.cache_write_tokens : 0,
      ),
      icon: <Database size={17} />,
    },
  ];

  return (
    <section className="glass-card usage-panel" role="tabpanel" aria-label="用量统计">
      <div className="usage-head">
        <div className="section-heading tight">
          <BarChart3 size={21} />
          <div>
            <h2>用量统计</h2>
            <p>查看 Token 使用情况和每日汇总</p>
          </div>
        </div>
        <button
          className="soft-button tiny outline"
          disabled={busy || usage.totals.requests === 0}
          onClick={onClear}
        >
          <Trash2 size={14} />
          清空统计
        </button>
      </div>

      <div className="usage-summary" aria-label="Token 汇总">
        {cards.map((card) => (
          <article className={`usage-summary-card ${card.tone}`} key={card.label}>
            <div className="usage-summary-icon">{card.icon}</div>
            <div className="usage-card-body">
              <p>{card.label}</p>
              <strong>{formatNumber(card.value)}</strong>
              {card.detail ? <small>{card.detail}</small> : null}
              {card.wave ? <UsageWave /> : null}
              <TrendPill value={card.trend} />
            </div>
          </article>
        ))}
      </div>

      <div className="usage-table-stage">
        <img className="usage-table-mascot" src={mascotUsage} alt="" />
        <div className="usage-table-wrap">
          <table className="usage-table">
            <thead>
              <tr>
                <th>日期</th>
                <th>请求</th>
                <th>输入</th>
                <th>输出</th>
                <th>总量</th>
              </tr>
            </thead>
            <tbody>
              {usage.days.length === 0 ? (
                <tr className="usage-empty-row">
                  <td colSpan={5}>
                    <div className="usage-empty">
                      <img src={mascotUsage} alt="" />
                      <p>暂无用量数据</p>
                    </div>
                  </td>
                </tr>
              ) : (
                usage.days.map((day) => (
                  <tr key={day.date}>
                    <td>{day.date}</td>
                    <td>{formatNumber(day.requests)}</td>
                    <td>
                      <strong>{formatNumber(day.input_tokens)}</strong>
                      <small>
                        <span>缓存读 {formatNumber(day.cache_read_tokens)}</span>
                        <span>缓存写 {formatNumber(day.cache_write_tokens)}</span>
                      </small>
                    </td>
                    <td>{formatNumber(day.output_tokens)}</td>
                    <td>{formatNumber(day.total_tokens)}</td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
    </section>
  );
}

function UsageWave() {
  return (
    <svg className="usage-wave" viewBox="0 0 150 42" aria-hidden="true">
      <path d="M2 30 C14 16, 23 36, 35 24 S55 20, 66 30 S83 38, 96 18 S118 22, 128 11 S140 7, 148 2" />
    </svg>
  );
}

function TrendPill({ value }: { value: string }) {
  const direction = value.startsWith("+") ? "up" : value.startsWith("-") ? "down" : "flat";
  const display = value.replace(/^[+-]/, "");
  return (
    <span className={`usage-trend ${direction}`}>
      <span>较昨日</span>
      {direction !== "flat" ? <b>{direction === "up" ? "↑" : "↓"}</b> : null}
      <strong>{display}</strong>
    </span>
  );
}

function LabeledInput({
  icon,
  label,
  hint,
  value,
  placeholder,
  type = "text",
  inputMode,
  trailing,
  onChange,
}: {
  icon: ReactNode;
  label: string;
  hint?: string;
  value: string;
  placeholder: string;
  type?: string;
  inputMode?: InputHTMLAttributes<HTMLInputElement>["inputMode"];
  trailing?: ReactNode;
  onChange: (value: string) => void;
}) {
  const [visible, setVisible] = useState(false);
  const realType = type === "password" && !visible ? "password" : "text";
  return (
    <label className="field">
      <span className="field-label">
        <span>
          {icon}
          {label}
        </span>
        {hint ? <small>{hint}</small> : null}
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
        ) : trailing ? (
          <span className="input-trailing">{trailing}</span>
        ) : null}
      </span>
    </label>
  );
}

function formatNumber(value: number) {
  return new Intl.NumberFormat("zh-CN").format(value || 0);
}

function getUsageDay(snapshot: TokenUsageSnapshot, daysAgo: number) {
  const date = new Date();
  date.setDate(date.getDate() - daysAgo);
  const key = [
    date.getFullYear(),
    String(date.getMonth() + 1).padStart(2, "0"),
    String(date.getDate()).padStart(2, "0"),
  ].join("-");
  return snapshot.days.find((day) => day.date === key);
}

export function formatTrend(today = 0, yesterday = 0) {
  if (!yesterday) return "0%";
  const percent = ((today - yesterday) / yesterday) * 100;
  if (!Number.isFinite(percent)) return "0%";
  const rounded = Math.round(percent * 10) / 10;
  return `${rounded > 0 ? "+" : ""}${rounded.toFixed(1)}%`;
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

export default App;
