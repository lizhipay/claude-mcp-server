import { describe, expect, it } from "vitest";
import { maskSecretForDisplay } from "./ui-utils";

describe("maskSecretForDisplay", () => {
  it("keeps empty key friendly", () => {
    expect(maskSecretForDisplay("")).toBe("还没有填写密钥哦");
  });

  it("masks long keys", () => {
    expect(maskSecretForDisplay("sk-ant-1234567890abcdef")).toBe("sk-a…cdef");
  });
});
