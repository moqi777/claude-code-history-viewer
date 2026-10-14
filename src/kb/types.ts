import type { ClaudeSession } from "@/types";

export type KbSource = "claude" | "codex";
export type KbSourceStatus = "present" | "deleted";

/** A collected session: `ClaudeSession`-compatible, plus what only the KB knows. */
export interface KbSession extends ClaudeSession {
  cwd: string | null;
  source_status: KbSourceStatus;
  source_path: string;
  parent_session_id: string | null;
  is_subagent: boolean;
}

export interface KbError {
  path: string;
  stage: string;
  message: string;
}

export interface KbBackupSummary {
  status: "ok" | "skipped" | "error";
  reason?: string;
  copied_files: number;
  appended_files: number;
  copied_bytes: number;
  set_aside_files: number;
  snapshot_taken: boolean;
  errors: KbError[];
  duration_ms: number;
}

export interface KbRunSummary {
  status: "ok" | "partial" | "skipped" | "error";
  reason?: string;
  trigger: string;
  scanned_files: number;
  changed_files: number;
  copied_bytes: number;
  new_sessions: number;
  deleted_sources: number;
  moved_sources: number;
  rewritten_files: number;
  errors: KbError[];
  duration_ms: number;
  backup?: KbBackupSummary;
}

export interface KbLastRun {
  started_at: string;
  finished_at: string | null;
  trigger: string;
  ok: boolean | null;
  summary: KbRunSummary | null;
}

export interface KbScheduleStatus {
  supported: boolean;
  label: string;
  plist_path: string;
  installed: boolean;
  loaded: boolean;
  interval_secs: number | null;
  program: string | null;
  plist_program: string | null;
  /** `false` = the installed task points at an executable that is gone. */
  program_exists: boolean | null;
  state: string | null;
  last_exit_code: number | null;
  runs: number | null;
}

export type KbExeStability = "stable" | "not_in_app" | "translocated" | "disk_image";

export interface KbBackupMeta {
  last_at: string | null;
  last_result: string | null;
  last_skipped_at: string | null;
  last_bytes: number | null;
  /** `[done, total]` bytes of a running backup. */
  progress: [number, number] | null;
}

export interface KbScheduleNotice {
  at: string;
  from: string | null;
  to: string;
  reason: string;
}

export type KbActivity = "idle" | "collecting" | "backing_up";

export interface KbOverview {
  data_dir: string;
  db_exists: boolean;
  sessions_by_source: Partial<Record<KbSource, number>>;
  deleted_sources: number;
  last_run: KbLastRun | null;
  running: boolean;
  activity: KbActivity;
  backup: KbBackupMeta;
  schedule_notice: KbScheduleNotice | null;
  data_bytes: number;
  schedule: KbScheduleStatus;
  interval_secs: number;
  schedule_enabled: boolean;
  exe_stability: KbExeStability;
  agentkb_version: string;
  base_version: string;
  mirror: { enabled: boolean; target_dir: string | null; volume_name: string | null };
}

export interface KbVolume {
  uuid: string;
  mount_point: string;
  name: string | null;
  internal: boolean;
  fs_type: string;
  writable: boolean;
}

export type KbUnsupportedReason =
  | { kind: "internal"; name: string | null }
  | { kind: "network"; fs_type: string }
  | { kind: "cloud" };

export type KbBackupPresence =
  | { state: "disabled" }
  | { state: "connected"; volume: KbVolume }
  | { state: "not_connected"; reason: string }
  | { state: "unsupported"; reason: KbUnsupportedReason };

export type KbMirrorReport =
  | { state: "connected"; volume: KbVolume; free_bytes: number | null; existing_bytes: number }
  | { state: "not_writable"; volume: KbVolume; message: string }
  | { state: "not_connected"; reason: string }
  | { state: "unsupported"; reason: KbUnsupportedReason };

// ---- configuration (mirror of config.rs) ----

export interface KbSourceConfig {
  enabled: boolean;
  root: string | null;
}

export interface KbMirrorConfig {
  enabled: boolean;
  target_dir: string | null;
  volume_uuid: string | null;
  volume_name: string | null;
}

export interface KbConfig {
  version: number;
  data_dir: string;
  interval_secs: number;
  schedule_enabled: boolean;
  sources: { claude: KbSourceConfig; codex: KbSourceConfig };
  exclude_prefixes: string[];
  mirror: KbMirrorConfig;
  [extra: string]: unknown;
}

export type KbConfigState =
  | { state: "missing" }
  | { state: "valid" }
  | { state: "invalid"; message: string; path: string }
  | { state: "newer"; file_version: number; path: string };

export type KbConfigStateInfo = KbConfigState & { config: KbConfig; config_path: string };

export interface KbSaveOutcome {
  config: KbConfig;
  schedule: KbScheduleStatus | null;
  schedule_error: string | null;
}

export interface KbSourceProbe {
  source: KbSource;
  root: string | null;
  exists: boolean;
  sessions: number;
  bytes: number;
}

export interface KbDataDirReport {
  path: string;
  exists: boolean;
  will_create: boolean;
  writable: boolean;
  free_bytes: number | null;
  existing: { sessions: number; bytes: number; has_db: boolean; has_raw: boolean } | null;
  db_too_new: boolean;
  overlaps_source: string | null;
  error: string | null;
}

export interface KbScheduleEnvReport {
  supported: boolean;
  launchctl_available: boolean;
  launch_agents_writable: boolean;
  exe: string;
  stability: KbExeStability;
  ok: boolean;
}

export interface KbWizardResult {
  sync: KbRunSummary;
  schedule: KbScheduleStatus | null;
  schedule_error: string | null;
  backup_started: boolean;
}

export type KbTargetState = "empty" | "resume" | "existing_data" | "not_empty";

export interface KbMigrateCheck {
  target_state: KbTargetState;
  needed_bytes: number;
  free_bytes: number | null;
  problem: string | null;
}

export interface KbMoveOutcome {
  report: { files: number; bytes: number; verified_files: number; duration_ms: number } | null;
  schedule_error: string | null;
}

export interface KbProgress {
  op: "sync" | "backup" | "migrate";
  phase: string;
  done: number;
  total: number;
  bytes_done: number;
  bytes_total: number;
}

export interface KbFacets {
  clients: string[];
  projects: [string, string][];
}

export type KbSourceFilter = "all" | KbSource;
export type KbTimeRange = "all" | "7d" | "30d";

export interface KbFilters {
  source: KbSourceFilter;
  client: string | null;
  project: string | null;
  range: KbTimeRange;
  onlyDeleted: boolean;
  includeSubagents: boolean;
  includeEmpty: boolean;
  query: string;
}

export interface KbSessionQuery {
  source?: KbSource;
  only_deleted?: boolean;
  include_subagents?: boolean;
  include_empty?: boolean;
  client?: string;
  project?: string;
  cwd?: string;
  since?: string;
  until?: string;
  query?: string;
  limit?: number;
  offset?: number;
}
