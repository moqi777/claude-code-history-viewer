import type { TFunction } from "i18next";
import { formatTime } from "@/utils/time";
import { BACKUP_STALE_DAYS, daysSince } from "./format";
import { percentOf } from "./progressLabel";
import type { KbBackupPresence, KbOverview } from "./types";
import type { Verdict } from "./messages";

/** The "external disk" line of the status area. */
export const describeBackup = (
  overview: KbOverview,
  presence: KbBackupPresence | null,
  t: TFunction,
  now = Date.now(),
): Verdict | null => {
  const { mirror, backup } = overview;
  if (!mirror.enabled) return { tone: "warn", text: t("kb.status.backup.off") };
  const name = mirror.volume_name ?? mirror.target_dir ?? "";

  if (overview.activity === "backing_up") {
    const [done, total] = backup.progress ?? [0, 0];
    return {
      tone: "ok",
      text: t("kb.status.backup.running", { name, percent: percentOf(done, total) ?? 0 }),
    };
  }
  if (backup.last_result?.startsWith("error:") === true) {
    return {
      tone: "error",
      text: t("kb.status.backup.error", { message: backup.last_result.slice("error:".length) }),
    };
  }
  const days = daysSince(backup.last_at, now);
  const last = backup.last_at != null ? formatTime(backup.last_at) : null;
  const connected = presence?.state === "connected";

  if (days != null && days >= BACKUP_STALE_DAYS) {
    return { tone: "warn", text: t("kb.status.backup.stale", { name, days }) };
  }
  if (last == null) {
    return {
      tone: connected ? "ok" : "warn",
      text: t(connected ? "kb.status.backup.never" : "kb.status.backup.neverNotConnected", { name }),
    };
  }
  return {
    tone: connected ? "ok" : "warn",
    text: t(connected ? "kb.status.backup.connected" : "kb.status.backup.notConnected", {
      name,
      time: last,
    }),
  };
};

export type ScheduleHealth = "unsupported" | "off" | "dead" | "active" | "installedNotLoaded";

export const scheduleHealth = (overview: KbOverview): ScheduleHealth => {
  const { schedule } = overview;
  if (!schedule.supported) return "unsupported";
  if (schedule.installed && schedule.program_exists === false) return "dead";
  if (schedule.loaded) return "active";
  if (schedule.installed) return "installedNotLoaded";
  return "off";
};
