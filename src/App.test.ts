import { describe, expect, it } from "vitest";
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
    expect(window.offset).toBe(100);
    expect(window.limit).toBe(0);
    expect(window.translateY).toBe(4_200);
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
