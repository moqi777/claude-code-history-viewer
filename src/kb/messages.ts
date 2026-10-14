import type { TFunction } from "i18next";
import { formatBytes } from "./format";
import type {
  KbDataDirReport,
  KbMigrateCheck,
  KbMirrorReport,
  KbScheduleEnvReport,
  KbSkillTest,
  KbSourceProbe,
  KbUnsupportedReason,
} from "./types";

/** One plain sentence for a test button, plus how loudly to show it. */
export interface Verdict {
  tone: "ok" | "warn" | "error";
  text: string;
}

export const describeSource = (probe: KbSourceProbe, t: TFunction): Verdict => {
  const name = t(`kb.sessions.filter.${probe.source}`);
  if (!probe.exists) {
    return { tone: "warn", text: t("kb.test.sources.missing", { name, root: probe.root ?? "—" }) };
  }
  return {
    tone: probe.sessions > 0 ? "ok" : "warn",
    text: t("kb.test.sources.found", {
      name,
      sessions: probe.sessions,
      size: formatBytes(probe.bytes),
    }),
  };
};

export const describeDataDir = (report: KbDataDirReport, t: TFunction): Verdict => {
  if (report.error === "not_absolute") return { tone: "error", text: t("kb.test.dataDir.notAbsolute") };
  if (report.error === "not_a_folder") return { tone: "error", text: t("kb.test.dataDir.notAFolder") };
  if (report.overlaps_source != null) {
    return {
      tone: "error",
      text: t("kb.test.dataDir.overlapsSource", { path: report.overlaps_source }),
    };
  }
  if (report.db_too_new) return { tone: "error", text: t("kb.test.dataDir.dbTooNew") };
  if (!report.writable) {
    return {
      tone: "error",
      text: t("kb.test.dataDir.notWritable", { message: report.error ?? "" }),
    };
  }
  const free = report.free_bytes != null ? formatBytes(report.free_bytes) : "?";
  if (report.existing != null) {
    return {
      tone: "ok",
      text: t("kb.test.dataDir.existing", {
        sessions: report.existing.sessions,
        size: formatBytes(report.existing.bytes),
        free,
      }),
    };
  }
  return { tone: "ok", text: t("kb.test.dataDir.ok", { free }) };
};

const unsupported = (reason: KbUnsupportedReason, t: TFunction): string => {
  switch (reason.kind) {
    case "internal":
      return t("kb.test.mirror.internal");
    case "network":
      return t("kb.test.mirror.network");
    case "cloud":
      return t("kb.test.mirror.cloud");
  }
};

export const describeMirror = (report: KbMirrorReport, t: TFunction): Verdict => {
  switch (report.state) {
    case "connected":
      return {
        tone: "ok",
        text: t("kb.test.mirror.connected", {
          name: report.volume.name ?? report.volume.mount_point,
          free: report.free_bytes != null ? formatBytes(report.free_bytes) : "?",
        }),
      };
    case "not_writable":
      return { tone: "error", text: t("kb.test.mirror.notWritable", { message: report.message }) };
    case "not_connected":
      return { tone: "warn", text: t("kb.test.mirror.notConnected") };
    case "unsupported":
      return { tone: "error", text: unsupported(report.reason, t) };
  }
};

export const describeScheduleEnv = (report: KbScheduleEnvReport, t: TFunction): Verdict => {
  if (!report.supported) return { tone: "error", text: t("kb.test.schedule.unsupported") };
  if (!report.launchctl_available) return { tone: "error", text: t("kb.test.schedule.noLaunchctl") };
  if (!report.launch_agents_writable) {
    return { tone: "error", text: t("kb.test.schedule.notWritable") };
  }
  switch (report.stability) {
    case "stable":
      return { tone: "ok", text: t("kb.test.schedule.ok", { exe: report.exe }) };
    case "not_in_app":
      return { tone: "warn", text: t("kb.test.schedule.devBuild") };
    case "translocated":
      return { tone: "warn", text: t("kb.test.schedule.translocated") };
    case "disk_image":
      return { tone: "warn", text: t("kb.test.schedule.diskImage") };
  }
};

export const describeMigrateCheck = (check: KbMigrateCheck, t: TFunction): Verdict => {
  if (check.problem != null) {
    return { tone: "error", text: t(`kb.move.problem.${check.problem}`) };
  }
  const needed = formatBytes(check.needed_bytes);
  const free = check.free_bytes != null ? formatBytes(check.free_bytes) : "?";
  switch (check.target_state) {
    case "resume":
      return { tone: "warn", text: t("kb.move.resume", { needed, free }) };
    case "existing_data":
      return { tone: "warn", text: t("kb.move.existingData") };
    default:
      return { tone: "ok", text: t("kb.move.ready", { needed, free }) };
  }
};

export const describeSkillTest = (result: KbSkillTest, t: TFunction): Verdict =>
  result.ok
    ? {
        tone: "ok",
        text: t("kb.test.skill.ok", {
          count: result.matched_sessions ?? 0,
          millis: result.millis,
        }),
      }
    : { tone: "error", text: t("kb.test.skill.failed", { message: result.message ?? "" }) };
