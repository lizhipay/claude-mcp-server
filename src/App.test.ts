import { describe, expect, it } from "vitest";
import { findSelectedChatUpdatedAt, formatTrend } from "./App";
import { formatLogDetail, getLogCountForLevel, getVirtualLogWindow } from "./log-utils";
import { maskSecretForDisplay } from "./ui-utils";

describe("maskSecretForDisplay", () => {
  it("keeps empty key friendly", () => {
    expect(maskSecretForDisplay("")).toBe("还没有填写密钥哦");
  });

  it("masks long keys", () => {
    expect(maskSecretForDisplay("sk-ant-1234567890abcdef")).toBe("sk-a…cdef");
  });
});

describe("log utilities", () => {
  const stats = {
    total: 100,
    dropped: 0,
    debug: 10,
    info: 20,
    warn: 30,
    error: 40,
    latest_id: 100,
  };

  it("counts current log level", () => {
    expect(getLogCountForLevel(stats, "all")).toBe(100);
    expect(getLogCountForLevel(stats, "error")).toBe(40);
  });

  it("calculates a small virtual render window", () => {
    const window = getVirtualLogWindow(42 * 500, 420, 1_000_000);
    expect(window.offset).toBeLessThan(500);
    expect(window.limit).toBeLessThan(80);
    expect(window.totalHeight).toBe(42_000_000);
  });

  it("keeps virtual page values non-negative after filtering or resize changes", () => {
    const window = getVirtualLogWindow(42 * 500, -20_132, 100);
    expect(window.offset).toBe(99);
    expect(window.limit).toBe(1);
    expect(window.translateY).toBe(4_158);
  });

  it("ignores non-finite virtual list inputs", () => {
    const window = getVirtualLogWindow(Number.NaN, Number.NaN, Number.NaN);
    expect(window).toEqual({ offset: 0, limit: 0, translateY: 0, totalHeight: 0 });
  });

  it("formats detail lazily with truncation", () => {
    expect(formatLogDetail({ ok: true })).toContain('"ok": true');
    expect(formatLogDetail("abcdef", 3)).toContain("已截断显示");
  });
});

describe("usage utilities", () => {
  it("shows stable zero trend when yesterday has no data", () => {
    expect(formatTrend(12_000, 0)).toBe("0%");
  });

  it("formats daily trend percentages", () => {
    expect(formatTrend(150, 100)).toBe("+50.0%");
    expect(formatTrend(75, 100)).toBe("-25.0%");
  });
});

describe("chat utilities", () => {
  it("tracks only the selected chat update timestamp", () => {
    const snapshot = {
      updated_at: 30,
      sessions: [
        {
          root_job_id: "job-1",
          latest_job_id: "job-1",
          workdir: "/tmp/a",
          status: "succeeded",
          created_at: 1,
          updated_at: 10,
          expires_at: 100,
          job_count: 1,
          title: "A",
          resumable: true,
        },
        {
          root_job_id: "job-2",
          latest_job_id: "job-2",
          workdir: "/tmp/b",
          status: "running",
          created_at: 2,
          updated_at: 30,
          expires_at: 100,
          job_count: 1,
          title: "B",
          resumable: false,
        },
      ],
    };

    expect(findSelectedChatUpdatedAt(snapshot, "job-1")).toBe(10);
    expect(findSelectedChatUpdatedAt({ ...snapshot, updated_at: 40 }, "job-1")).toBe(10);
    expect(findSelectedChatUpdatedAt(snapshot, "missing")).toBe(0);
  });
});
