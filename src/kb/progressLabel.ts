import type { TFunction } from "i18next";
import { formatBytes } from "./format";
import type { KbProgress } from "./types";

/** Percentage of a byte-counted operation, or `null` when the total is unknown. */
export const percentOf = (done: number, total: number): number | null =>
  total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : null;

export const progressLabel = (p: KbProgress, t: TFunction): string => {
  if (p.op === "sync") {
    return t("kb.progress.collect", { done: p.done, total: p.total });
  }
  const key = p.phase === "verify" ? "kb.progress.verify" : "kb.progress.copy";
  return t(key, {
    done: p.done,
    total: p.total,
    bytes: formatBytes(p.bytes_done),
    percent: percentOf(p.bytes_done, p.bytes_total) ?? 0,
  });
};
