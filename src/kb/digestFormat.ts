import type { TFunction } from "i18next";
import type { KbDigestPaused, KbDigestStatus, KbDigestTestReport } from "./types";
import type { Verdict } from "./messages";

/**
 * "420k tokens" in English, "42 万 token" in Chinese: the unit and the scale
 * both come from the language file, so a language writes tokens its own way.
 */
export const formatTokens = (tokens: number, t: TFunction): string => {
  const scale = Number(t("kb.digest.tokenScale")) || 1000;
  const amount = tokens / scale;
  const shown = amount < 10 ? amount.toFixed(1).replace(/\.0$/, "") : String(Math.round(amount));
  return t("kb.digest.tokens", { amount: shown });
};

/** Minutes as "about 45 min" / "about 3 h". */
export const formatMinutes = (minutes: number, t: TFunction): string =>
  minutes < 90
    ? t("kb.digest.minutes", { count: Math.max(1, Math.round(minutes)) })
    : t("kb.digest.hours", { count: Math.round(minutes / 60) });

/** The home card's one-line reason for a paused digest. */
export const pausedText = (paused: KbDigestPaused, until: string, engine: string, t: TFunction): string =>
  t(`kb.digest.paused.${paused.reason}`, {
    until,
    engine: t(`kb.digest.engine.${engine}`),
  });

/** What the settings page's test button says. */
export const describeDigestTest = (report: KbDigestTestReport, t: TFunction): Verdict => {
  if (report.ok) {
    return {
      tone: "ok",
      text: t("kb.digest.test.ok", {
        seconds: Math.max(1, Math.round(report.duration_ms / 1000)),
        input: report.input_tokens,
        output: report.output_tokens,
        model: report.model ?? t("kb.digest.test.defaultModel"),
      }),
    };
  }
  const kind = report.kind ?? "failed";
  return {
    tone: "error",
    text: t(`kb.digest.test.${kind}`, { message: report.message ?? "" }),
  };
};

/** "10-17 06:40" in the reader's own order and language; the year is left out. */
export const formatShortTime = (iso: string): string => {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString(undefined, {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
};

/** The home page's one-line digest summary, and whether it asks for attention. */
export const digestSummary = (
  digest: KbDigestStatus,
  t: TFunction,
): { text: string; attention: boolean } => {
  const next = digest.next_run_at != null ? formatShortTime(digest.next_run_at) : null;
  if (digest.running != null) {
    return {
      text: t("kb.summary.digest.running", {
        done: digest.running.finished,
        total: digest.running.total,
      }),
      attention: false,
    };
  }
  if (!digest.enabled) return { text: t("kb.summary.digest.off"), attention: false };
  if (digest.paused != null) return { text: t("kb.summary.digest.paused"), attention: true };
  if (digest.failures.length > 0) {
    return { text: t("kb.summary.digest.failures", { count: digest.failures.length }), attention: true };
  }
  const last = digest.last_round;
  const state = last == null ? "never" : last.state === "done" ? "done" : "open";
  return {
    text: t(next != null ? `kb.summary.digest.${state}Next` : `kb.summary.digest.${state}`, { time: next }),
    attention: false,
  };
};
