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
  errors: { path: string; stage: string; message: string }[];
  duration_ms: number;
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
  state: string | null;
  last_exit_code: number | null;
  runs: number | null;
}

export interface KbOverview {
  data_dir: string;
  db_exists: boolean;
  sessions_by_source: Partial<Record<KbSource, number>>;
  deleted_sources: number;
  last_run: KbLastRun | null;
  running: boolean;
  schedule: KbScheduleStatus;
  interval_secs: number;
}

export type KbSourceFilter = "all" | KbSource | "deleted";

export interface KbSessionQuery {
  source?: KbSource;
  only_deleted?: boolean;
  include_subagents?: boolean;
  include_empty?: boolean;
  limit?: number;
  offset?: number;
}
