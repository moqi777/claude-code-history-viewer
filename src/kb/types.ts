import type { ClaudeSession } from "@/types";

export type KbSource = "claude" | "codex";
/** `elsewhere`: imported from another device; its source file was never on this computer. */
export type KbSourceStatus = "present" | "deleted" | "elsewhere";

/** A collected session: `ClaudeSession`-compatible, plus what only the KB knows. */
export interface KbSession extends ClaudeSession {
  cwd: string | null;
  source_status: KbSourceStatus;
  /** The device an imported session came from. */
  origin?: string | null;
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
  joined_twins?: number;
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

export interface KbDigestConfig {
  enabled: boolean;
  engine: KbDigestEngine;
  /** Empty = the CLI's own default model. */
  model: string;
  /** Absolute path of the CLI; background tasks have no `PATH`. */
  cli_path: string | null;
  /** Local `HH:MM`: where one round ends and the next begins. */
  time: string;
  max_sessions_per_round: number;
  recent_days: number;
  quiet_minutes: number;
  min_messages: number;
  max_chunks_per_session: number;
  /** Codex only; empty = its default. Kept in `config.json`, not shown. */
  codex_reasoning: string;
}

export type KbDigestEngine = "claude" | "codex";

export interface KbConfig {
  version: number;
  data_dir: string;
  interval_secs: number;
  schedule_enabled: boolean;
  sources: { claude: KbSourceConfig; codex: KbSourceConfig };
  exclude_prefixes: string[];
  /** Working directories whose sessions stay backed up but are not searched or listed. */
  exclude_cwd_prefixes: string[];
  mirror: KbMirrorConfig;
  digest: KbDigestConfig;
  semantic: { enabled: boolean; source: string };
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
  op: "sync" | "backup" | "migrate" | "export" | "import";
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
  onlyElsewhere: boolean;
  includeSubagents: boolean;
  includeEmpty: boolean;
  query: string;
}

export interface KbSessionQuery {
  source?: KbSource;
  only_deleted?: boolean;
  only_elsewhere?: boolean;
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

// ---------------------------------------------------------------------------
// Full-text search
// ---------------------------------------------------------------------------

export interface KbSearchParams {
  query: string;
  source?: KbSource;
  client?: string;
  project?: string;
  since?: string;
  until?: string;
  limit?: number;
  per_session?: number;
  include_subagents?: boolean;
  /** List sub-agent sessions as entries of their own. */
  expand_subagents?: boolean;
  mode?: "auto" | "all" | "any";
}

export interface KbHit {
  seq: number;
  kind: "user" | "assistant" | "tool_use" | "tool_result" | "tool_error";
  at: string | null;
  /** Viewer message id (Claude `uuid`); null for Codex. */
  msg_key: string | null;
  snippet: string;
  /** Set when the hit is in a sub-agent session folded into this entry. */
  subagent_session_id?: string | null;
  /** Matched ranges in `snippet`, as character offsets `[start, end)`. */
  highlights: [number, number][];
  show: string;
}

export interface KbHitSession {
  source: KbSource;
  session_id: string;
  title: string | null;
  client: string | null;
  project: string | null;
  cwd: string | null;
  started_at: string | null;
  last_activity_at: string | null;
  source_status: KbSourceStatus;
  origin?: string | null;
  is_subagent: boolean;
  parent_session_id: string | null;
  message_count: number;
}

export interface KbSessionHit {
  rank: number;
  score: number;
  /** The session has notes written by the daily digest. */
  has_note?: boolean;
  session: KbHitSession;
  resume: { available: boolean; command: string | null; note: string | null };
  paths: { mirror: string; original: string };
  hit_messages: number;
  /** With semantic search on: how the session was found. */
  match?: "keyword" | "semantic" | "both";
  semantic_score?: number;
  /** Matching messages in sub-agent sessions folded into this entry. */
  subagent_hits?: number;
  hits: KbHit[];
}

export interface KbSearchResponse {
  query: string;
  tokens: string[];
  mode_used: "all" | "any" | "hybrid";
  read_mode: "normal" | "immutable";
  data_as_of: string | null;
  matched_sessions: number;
  returned: number;
  /** AI-written notes, best first; shown ahead of the messages. */
  notes?: KbNote[];
  results: KbSessionHit[];
  hint: string | null;
  /** Whether the vectors were used, and if not, why. */
  semantic?: KbSemanticUse;
}

export type KbSemanticUseState =
  | "used"
  | "off"
  | "model_missing"
  | "model_corrupt"
  | "index_building"
  | "index_empty"
  | "skipped"
  | "error";

export interface KbSemanticUse {
  state: KbSemanticUseState;
  indexed: number;
  total: number;
  note: string;
}

export type KbSearchOutcome =
  | { state: "ok"; response: KbSearchResponse }
  | { state: "not_ready"; message: string }
  | { state: "no_data"; message: string };

// ---------------------------------------------------------------------------
// Agent skill
// ---------------------------------------------------------------------------

export type KbSkillTarget = "claude" | "codex";

export interface KbSkillStatus {
  target: KbSkillTarget;
  tool_found: boolean;
  dir: string;
  state: "not_installed" | "current" | "stale" | "foreign";
  installed_version: string | null;
  installed_exe: string | null;
  installed_exe_exists: boolean | null;
  stale_reason: "version" | "path" | "missing_exe" | null;
}

export interface KbSkillTest {
  ok: boolean;
  query: string | null;
  matched_sessions: number | null;
  millis: number;
  message: string | null;
}

// ---------------------------------------------------------------------------
// Notes and the daily digest
// ---------------------------------------------------------------------------

export interface KbNoteEvidence {
  seq_from: number;
  seq_to: number;
  /** Claude only; Codex messages are found by `at` and `kind`. */
  msg_key: string | null;
  at: string | null;
  kind: KbHit["kind"] | null;
}

/** One entry of a session's notes, written by the digest. */
export interface KbNote {
  kind: "note";
  rank: number;
  score: number;
  entry_id: string;
  /** The digest round that wrote it (a date, or `manual-…`). */
  round: string;
  /** `claude/<model>`. */
  engine: string;
  title: string;
  problem: string;
  cause: string;
  solution: string;
  conclusion: string;
  keywords: string[];
  evidence: KbNoteEvidence;
  digested_at: string;
  session: KbHitSession;
  paths: { mirror: string; original: string };
  show: string;
  resume: { available: boolean; command: string | null; note: string | null };
}

export interface KbNotesPage {
  total: number;
  notes: KbNote[];
}

export interface KbNotesQuery {
  query?: string;
  source?: KbSource;
  session_id?: string;
  since?: string;
  limit?: number;
  offset?: number;
}

export interface KbSessionNotes {
  notes: KbNote[];
  /** `null` when the session was never digested. */
  status: "notes" | "nothing" | "partial" | null;
  digested_at: string | null;
}

export interface KbDigestRound {
  round_id: string;
  kind: "daily" | "manual";
  state: "open" | "done" | "superseded";
  planned_at: string;
  completed_at: string | null;
  done: number;
  failed: number;
  skipped: number;
  pending: number;
  input_tokens: number;
  output_tokens: number;
  duration_ms: number;
}

export interface KbDigestPaused {
  reason: "quota" | "auth" | "config";
  until: string;
  message: string;
}

export interface KbDigestFailure {
  source: KbSource;
  session_id: string;
  title: string | null;
  attempts: number;
  error: string | null;
}

export interface KbDigestRunning {
  pid: number;
  started_at: string;
  round_id: string | null;
  total: number;
  finished: number;
  current_title: string | null;
}

export interface KbDigestStatus {
  enabled: boolean;
  engine: KbDigestEngine;
  model: string;
  time: string;
  running: KbDigestRunning | null;
  paused: KbDigestPaused | null;
  next_run_at: string | null;
  last_round: KbDigestRound | null;
  failures: KbDigestFailure[];
  /** Sessions the next automatic round would digest. */
  pending_auto: number;
  /** Undigested sessions older than the automatic window (manual only). */
  pending_older: number;
  /** Until the first round: what it will cost, in input tokens. */
  first_round_tokens: number | null;
}

export interface KbDigestEstimate {
  sessions: number;
  calls: number;
  input_tokens: number;
  minutes: number;
  engine: KbDigestEngine;
  model: string;
}

export type KbDigestRunRequest =
  | { kind: "all" }
  | { kind: "session"; source: KbSource; session_id: string };

export type KbDigestRunStarted =
  | { state: "started" }
  | { state: "busy" }
  | { state: "nothing"; message: string };

export interface KbDigestTestReport {
  ok: boolean;
  kind: "quota" | "auth" | "config" | "timeout" | "failed" | "invalid_answer" | null;
  message: string | null;
  cli: string | null;
  duration_ms: number;
  input_tokens: number;
  output_tokens: number;
  model: string | null;
}

// ---------------------------------------------------------------------------
// Export and import
// ---------------------------------------------------------------------------

export interface KbExportReport {
  path: string;
  export_id: string;
  file_bytes: number;
  uncompressed_bytes: number;
  files: number;
  sessions: number;
  notes: number;
  sessions_with_notes: number;
  raw_files: number;
  note_files: number;
  duration_ms: number;
  paused_collection: boolean;
}

export interface KbTransferCounts {
  sessions: number;
  notes: number;
  sessions_with_notes: number;
  raw_files: number;
  note_files: number;
}

export interface KbLocalState {
  empty: boolean;
  sessions: number;
  notes: number;
  raw_files: number;
  note_files: number;
}

export interface KbImportInspect {
  export_id: string;
  exported_at: string;
  app_version: string;
  schema_version: number;
  device_name: string;
  device_os: string;
  counts: KbTransferCounts;
  uncompressed_bytes: number;
  file_bytes: number;
  local: KbLocalState;
  needed_bytes: number;
  free_bytes: number | null;
  problem: string | null;
}

export interface KbMergeSummary {
  sessions_added: number;
  files_added: number;
  identical: number;
  extended: number;
  kept_both_live: number;
  kept_both_diverged: number;
  kept_both_other: number;
  row_collisions: number;
  notes_added: number;
  notes_identical: number;
  notes_conflicts: number;
}

export interface KbImportPrepared {
  export_id: string;
  mode: "empty" | "merge";
  device_name: string;
  counts: KbTransferCounts;
  summary: KbMergeSummary | null;
  added_exclusions: string[];
  local: KbLocalState;
}

export interface KbImportCheck {
  name: string;
  expected: number;
  actual: number;
}

export interface KbImportReport {
  mode: "empty" | "merge";
  export_id: string;
  device_name: string;
  exported_at: string;
  summary: KbMergeSummary | null;
  checks: KbImportCheck[];
  all_match: boolean;
  backup_db: string | null;
  backup_config: string | null;
  added_exclusions: string[];
  config: KbConfig;
  config_written: boolean;
  warnings: string[];
  vectors_adopted?: boolean;
  duration_ms: number;
}

// ---------------------------------------------------------------------------
// Semantic search
// ---------------------------------------------------------------------------

export type KbModelStatus =
  | { state: "missing" }
  | { state: "corrupt"; file: string; reason: string }
  | { state: "ready" };

export interface KbEmbedState {
  /** `idle`, `running`, `postponed_memory`, `paused_model`, `error`. */
  status: string;
  total: number;
  done: number;
  updated_at: string;
  message: string;
  postponed_rounds: number;
  indexed: number;
  building: boolean;
}

export interface KbSemanticStatus {
  enabled: boolean;
  source: string;
  sources: { id: string; host: string }[];
  model_name: string;
  model_bytes: number;
  model: KbModelStatus;
  downloading: boolean;
  index: KbEmbedState;
  vectors_bytes: number;
  models_dir: string;
}

export interface KbSemanticTest {
  ok: boolean;
  seconds: number;
  dim: number;
  message: string;
}

export interface KbDownloadProgress {
  file: string;
  done_bytes: number;
  total_bytes: number;
}
