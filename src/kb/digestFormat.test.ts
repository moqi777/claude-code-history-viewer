import { describe, expect, it } from "vitest";
import { describeDigestTest, formatMinutes, formatTokens } from "./digestFormat";

// A tiny stand-in for i18next that knows two languages' token wording.
const lang = (scale: string, unit: (n: string) => string) =>
  ((key: string, options?: Record<string, unknown>) => {
    if (key === "kb.digest.tokenScale") return scale;
    if (key === "kb.digest.tokens") return unit(String(options?.amount));
    if (key === "kb.digest.minutes") return `${options?.count} min`;
    if (key === "kb.digest.hours") return `${options?.count} h`;
    return `${key} ${JSON.stringify(options ?? {})}`;
  }) as never;

const en = lang("1000", (n) => `${n}k tokens`);
const zh = lang("10000", (n) => `${n} 万 token`);

describe("formatTokens", () => {
  it("uses the scale and wording of the language", () => {
    expect(formatTokens(420_000, en)).toBe("420k tokens");
    expect(formatTokens(420_000, zh)).toBe("42 万 token");
    expect(formatTokens(12_400_000, zh)).toBe("1240 万 token");
  });

  it("keeps one decimal for small amounts and drops a trailing .0", () => {
    expect(formatTokens(4_200, en)).toBe("4.2k tokens");
    expect(formatTokens(4_000, en)).toBe("4k tokens");
    expect(formatTokens(0, en)).toBe("0k tokens");
  });
});

describe("formatMinutes", () => {
  it("switches to hours past an hour and a half", () => {
    expect(formatMinutes(0.2, en)).toBe("1 min");
    expect(formatMinutes(45, en)).toBe("45 min");
    expect(formatMinutes(190, en)).toBe("3 h");
  });
});

describe("describeDigestTest", () => {
  const base = {
    cli: "/x/claude", duration_ms: 11_400, input_tokens: 3200, output_tokens: 4, model: "m", message: null,
  };
  it("reports success with time and tokens", () => {
    const v = describeDigestTest({ ...base, ok: true, kind: null }, en);
    expect(v.tone).toBe("ok");
    expect(v.text).toContain('"seconds":11');
    expect(v.text).toContain('"input":3200');
  });

  it("names each kind of failure", () => {
    for (const kind of ["quota", "auth", "config", "timeout", "failed", "invalid_answer"] as const) {
      const v = describeDigestTest({ ...base, ok: false, kind, message: "boom" }, en);
      expect(v.tone).toBe("error");
      expect(v.text).toContain(`kb.digest.test.${kind}`);
    }
  });
});
