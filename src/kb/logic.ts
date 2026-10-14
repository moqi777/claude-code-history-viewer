import type { KbDataDirReport } from "./types";

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
