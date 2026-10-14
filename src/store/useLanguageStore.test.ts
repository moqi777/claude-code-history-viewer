import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  saved: {} as Record<string, unknown>,
  isTauri: true,
  locale: "zh-Hans-CN",
  changeLanguage: vi.fn(async () => undefined),
}));

vi.mock("@/services/storage", () => ({
  storageAdapter: {
    load: async () => ({
      get: async (key: string) => mocks.saved[key] ?? null,
      set: async (key: string, value: unknown) => {
        mocks.saved[key] = value;
      },
      save: async () => undefined,
    }),
  },
}));
vi.mock("@/utils/platform", () => ({ isTauri: () => mocks.isTauri }));
vi.mock("@tauri-apps/plugin-os", () => ({ locale: async () => mocks.locale }));
vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));
vi.mock("../i18n", () => ({
  default: { language: "en-US", changeLanguage: mocks.changeLanguage, t: (k: string) => k },
  languageLocaleMap: { en: "en-US", ko: "ko-KR", ja: "ja-JP", "zh-CN": "zh-CN", "zh-TW": "zh-TW" },
}));

import { useLanguageStore } from "./useLanguageStore";

describe("useLanguageStore.loadLanguage", () => {
  beforeEach(() => {
    mocks.saved = {};
    mocks.isTauri = true;
    mocks.locale = "zh-Hans-CN";
    mocks.changeLanguage.mockClear();
    localStorage.clear();
  });

  it("first launch follows the system language even though the WebView cached en-US", async () => {
    localStorage.setItem("i18nextLng", "en-US"); // what the detector writes on first start
    await useLanguageStore.getState().loadLanguage();
    expect(useLanguageStore.getState().language).toBe("zh-CN");
    expect(mocks.saved.language).toBe("zh-CN");
  });

  it("a saved choice beats the system language", async () => {
    mocks.saved.language = "ja";
    await useLanguageStore.getState().loadLanguage();
    expect(useLanguageStore.getState().language).toBe("ja");
  });

  it("a traditional-Chinese system maps to zh-TW", async () => {
    mocks.locale = "zh-Hant-TW";
    await useLanguageStore.getState().loadLanguage();
    expect(useLanguageStore.getState().language).toBe("zh-TW");
  });

  it("outside Tauri the browser's own stored language is still honoured", async () => {
    mocks.isTauri = false;
    localStorage.setItem("i18nextLng", "ko");
    await useLanguageStore.getState().loadLanguage();
    expect(useLanguageStore.getState().language).toBe("ko");
  });
});
