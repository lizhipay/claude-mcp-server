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
  const safeTotal = Math.max(0, Math.floor(Number.isFinite(total) ? total : 0));
  const safeRowHeight =
    Number.isFinite(rowHeight) && rowHeight > 0 ? rowHeight : LOG_ROW_HEIGHT;
  const safeOverscan = Math.max(0, Math.floor(Number.isFinite(overscan) ? overscan : 0));
  const safeScrollTop = Math.max(0, Number.isFinite(scrollTop) ? scrollTop : 0);
  const safeViewportHeight = Math.max(0, Number.isFinite(viewportHeight) ? viewportHeight : 0);

  if (safeTotal <= 0) {
    return { offset: 0, limit: 0, translateY: 0, totalHeight: 0 };
  }

  const visibleRows = Math.max(
    1,
    Math.ceil(Math.max(safeViewportHeight, safeRowHeight) / safeRowHeight),
  );
  const maxOffset = Math.max(0, safeTotal - visibleRows);
  const offset = Math.min(
    maxOffset,
    Math.max(0, Math.floor(safeScrollTop / safeRowHeight) - safeOverscan),
  );
  const limit = Math.max(0, Math.min(safeTotal - offset, visibleRows + safeOverscan * 2));

  return {
    offset,
    limit,
    translateY: offset * safeRowHeight,
    totalHeight: safeTotal * safeRowHeight,
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
