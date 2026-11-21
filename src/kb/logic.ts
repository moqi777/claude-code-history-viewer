import { KB_PACK_DEFAULT, KB_PACK_MAX_DAYS, type KbConfig, type KbDataDirReport, type KbPackConfig } from "./types";

/** Exclude paths, one absolute path per line. */
export const parseExcludes = (text: string): string[] =>
  text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");

export const excludesValid = (paths: string[]): boolean => paths.every((p) => p.startsWith("/"));

/** The data-folder test passed and the wizard may move on. */
export const dataDirAcceptable = (report: KbDataDirReport | null): boolean =>
  report != null &&
  report.writable &&
  report.error == null &&
  report.overlaps_source == null &&
  !report.db_too_new;

/** `HH:MM`, 24 hours. */
export const timeValid = (text: string): boolean => /^([01]\d|2[0-3]):[0-5]\d$/.test(text);

/** The digest settings the page exposes are within what the backend accepts. */
export const digestValid = (d: { time: string; max_sessions_per_round: number }): boolean =>
  timeValid(d.time) &&
  Number.isInteger(d.max_sessions_per_round) &&
  d.max_sessions_per_round >= 1 &&
  d.max_sessions_per_round <= 500;

/** Old backup compression settings; configs from before 0.8.0 have none. */
export const packOf = (config: KbConfig): KbPackConfig => config.pack ?? KB_PACK_DEFAULT;

export const packValid = (pack: KbPackConfig): boolean =>
  Number.isInteger(pack.after_days) && pack.after_days >= 1 && pack.after_days <= KB_PACK_MAX_DAYS;
