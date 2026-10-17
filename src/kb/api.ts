import { api } from "@/services/api";
import type {
  KbDigestConfig,
  KbDigestEstimate,
  KbDigestRunRequest,
  KbDigestRunStarted,
  KbDigestStatus,
  KbDigestTestReport,
  KbNotesPage,
  KbNotesQuery,
  KbSessionNotes,
  KbBackupPresence,
  KbBackupSummary,
  KbConfig,
  KbConfigStateInfo,
  KbDataDirReport,
  KbExportReport,
  KbImportInspect,
  KbImportPrepared,
  KbImportReport,
  KbFacets,
  KbMigrateCheck,
  KbMirrorReport,
  KbMoveOutcome,
  KbOverview,
  KbRunSummary,
  KbSaveOutcome,
  KbScheduleEnvReport,
  KbScheduleStatus,
  KbSearchOutcome,
  KbSemanticStatus,
  KbSemanticTest,
  KbSearchParams,
  KbSkillStatus,
  KbSkillTarget,
  KbSkillTest,
  KbSession,
  KbSessionQuery,
  KbSourceProbe,
  KbVolume,
  KbWizardResult,
} from "./types";

export const kbApi = {
  getStatus: () => api<KbOverview>("kb_get_status"),
  getBackupPresence: () => api<KbBackupPresence>("kb_get_backup_presence"),
  syncNow: () => api<KbRunSummary>("kb_sync_now"),
  backupNow: () => api<KbBackupSummary>("kb_backup_now"),
  listSessions: (filter: KbSessionQuery) =>
    api<KbSession[]>("kb_list_sessions", { filter }),
  search: (params: KbSearchParams) => api<KbSearchOutcome>("kb_search", { params }),
  skillStatus: () => api<KbSkillStatus[]>("kb_skill_status"),
  skillInstall: (target: KbSkillTarget) => api<KbSkillStatus[]>("kb_skill_install", { target }),
  skillUninstall: (target: KbSkillTarget) =>
    api<KbSkillStatus[]>("kb_skill_uninstall", { target }),
  skillTest: () => api<KbSkillTest>("kb_skill_test"),
  getFacets: () => api<KbFacets>("kb_session_facets"),
  dismissNotice: (key: "schedule_repaired") => api<void>("kb_dismiss_notice", { key }),
  installSchedule: () => api<KbScheduleStatus>("kb_schedule_install"),
  uninstallSchedule: () => api<KbScheduleStatus>("kb_schedule_uninstall"),

  getConfigState: () => api<KbConfigStateInfo>("kb_get_config_state"),
  getConfig: () => api<KbConfig>("kb_get_config"),
  saveConfig: (config: KbConfig) => api<KbSaveOutcome>("kb_save_config", { config }),
  restoreDefaultConfig: () => api<string>("kb_restore_default_config"),

  probeSources: (config?: KbConfig) =>
    api<KbSourceProbe[]>("kb_probe_sources", { config: config ?? null }),
  testDataDir: (path: string) => api<KbDataDirReport>("kb_test_data_dir", { path }),
  testScheduleEnv: () => api<KbScheduleEnvReport>("kb_test_schedule_env"),
  listExternalVolumes: () => api<KbVolume[]>("kb_list_external_volumes"),
  testMirror: (targetDir: string, expectedUuid?: string | null) =>
    api<KbMirrorReport>("kb_test_mirror", {
      targetDir,
      expectedUuid: expectedUuid ?? null,
    }),

  exportStart: (dest: string, overwrite: boolean, withVectors = true) =>
    api<KbExportReport>("kb_export_start", { dest, overwrite, withVectors }),
  transferCancel: () => api<void>("kb_transfer_cancel"),
  importInspect: (path: string, dataDir?: string) =>
    api<KbImportInspect>("kb_import_inspect", { path, dataDir }),
  importPrepare: (path: string, adoptSettings: boolean, dataDir?: string) =>
    api<KbImportPrepared>("kb_import_prepare", { path, adoptSettings, dataDir }),
  importCommit: (path: string, adoptSettings: boolean, writeConfig: boolean, dataDir?: string) =>
    api<KbImportReport>("kb_import_commit", { path, adoptSettings, writeConfig, dataDir }),
  importDiscard: (path: string, dataDir?: string) =>
    api<void>("kb_import_discard", { path, dataDir }),

  wizardCommit: (draft: KbConfig) => api<KbWizardResult>("kb_wizard_commit", { draft }),
  changeDataDirCheck: (newDir: string) =>
    api<KbMigrateCheck>("kb_change_data_dir_check", { newDir }),
  changeDataDirRun: (newDir: string, useExisting: boolean) =>
    api<KbMoveOutcome>("kb_change_data_dir_run", { newDir, useExisting }),

  digestStatus: () => api<KbDigestStatus>("kb_digest_status"),
  digestEstimateAll: () => api<KbDigestEstimate>("kb_digest_estimate_all"),
  digestDailyTokens: () => api<number | null>("kb_digest_daily_tokens"),
  digestRun: (request: KbDigestRunRequest) =>
    api<KbDigestRunStarted>("kb_digest_run", { request }),
  digestCancel: () => api<boolean>("kb_digest_cancel"),
  digestFindCli: (engine: string) => api<string | null>("kb_digest_find_cli", { engine }),
  digestTest: (draft: KbDigestConfig) => api<KbDigestTestReport>("kb_digest_test", { draft }),
  semanticStatus: () => api<KbSemanticStatus>("kb_semantic_status"),
  semanticSet: (patch: { enabled?: boolean; source?: string }) =>
    api<KbSemanticStatus>("kb_semantic_set", {
      enabled: patch.enabled ?? null,
      source: patch.source ?? null,
    }),
  semanticDownload: () => api<void>("kb_semantic_download"),
  semanticDownloadCancel: () => api<void>("kb_semantic_download_cancel"),
  semanticImportFolder: (path: string) =>
    api<KbSemanticStatus>("kb_semantic_import_folder", { path }),
  semanticDeleteModel: () => api<KbSemanticStatus>("kb_semantic_delete_model"),
  semanticTest: () => api<KbSemanticTest>("kb_semantic_test"),
  semanticBuild: (everything: boolean) => api<void>("kb_semantic_build", { everything }),
  semanticBuildCancel: () => api<boolean>("kb_semantic_build_cancel"),
  semanticRebuild: () => api<void>("kb_semantic_rebuild"),
  notesList: (query: KbNotesQuery) => api<KbNotesPage>("kb_notes_list", { query }),
  notesForSession: (source: string, sessionId: string) =>
    api<KbSessionNotes>("kb_notes_for_session", { source, sessionId }),
};
