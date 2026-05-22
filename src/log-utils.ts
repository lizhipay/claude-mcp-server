import type { LogLevel, LogStats } from "./tauri";

export type LogLevelFilter = LogLevel | "all";

export const LOG_ROW_HEIGHT = 42;
export const LOG_OVERSCAN_ROWS = 18;
export const LOG_DETAIL_MAX_CHARS = 12_000;

export function getLogCountForLevel(stats: LogStats, level: LogLevelFilter): number {
  if (level === "all") return stats.total;
  return stats[level];
}

export function getVirtualLogWindow(
  scrollTop: number,
  viewportHeight: number,
  total: number,
  rowHeight = LOG_ROW_HEIGHT,
  overscan = LOG_OVERSCAN_ROWS,
) {
  if (total <= 0) {
    return { offset: 0, limit: 0, translateY: 0, totalHeight: 0 };
  }

  const visibleRows = Math.max(1, Math.ceil(Math.max(viewportHeight, rowHeight) / rowHeight));
  const offset = Math.max(0, Math.floor(Math.max(0, scrollTop) / rowHeight) - overscan);
  const limit = Math.min(total - offset, visibleRows + overscan * 2);

  return {
    offset,
    limit,
    translateY: offset * rowHeight,
    totalHeight: total * rowHeight,
  };
}

export function formatLogDetail(detail: unknown, maxChars = LOG_DETAIL_MAX_CHARS): string {
  if (detail == null) {
    return "暂无详情";
  }

  const text =
    typeof detail === "string" ? detail : JSON.stringify(detail, null, 2) ?? String(detail);

  if (text.length <= maxChars) {
    return text;
  }

  return `${text.slice(0, maxChars)}\n...详情过长，已截断显示`;
}
