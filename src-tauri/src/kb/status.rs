//! Read-only views of `kb.db` for the status panel and the session list.
//! Never takes the sync lock and never writes, so it cannot delay a sync.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use super::backup;
use super::db;
use super::lock::{self, LockInfo};
use super::schedule;
use super::sync::KbEnv;

#[derive(Debug, Clone, Serialize)]
pub struct LastRun {
    pub started_at: String,
    pub finished_at: Option<String>,
    pub trigger: String,
    pub ok: Option<bool>,
    pub summary: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KbStatus {
    pub data_dir: String,
    pub db_exists: bool,
    pub sessions_by_source: BTreeMap<String, u64>,
    pub deleted_sources: u64,
    pub last_run: Option<LastRun>,
    /// A sync is in progress right now: the newest run has not finished and the
    /// process that wrote the lock file is still alive.
    pub running: bool,
    pub lock_holder: Option<LockInfo>,
    /// `idle`, `collecting` or `backing_up` — what the process holding the
    /// lock is doing right now.
    pub activity: &'static str,
    pub backup: BackupMeta,
    /// Set when the app re-pointed the scheduled task at itself.
    pub schedule_notice: Option<ScheduleNotice>,
    /// Duplicate backups removed since the user last dismissed the notice.
    pub duplicates_notice: Option<DuplicatesNotice>,
    /// Size of the whole data directory.
    pub data_bytes: u64,
}

/// Backup facts recorded in `kb.db` by whichever process ran the backup.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupMeta {
    pub last_at: Option<String>,
    pub last_result: Option<String>,
    pub last_skipped_at: Option<String>,
    pub last_bytes: Option<u64>,
    /// `bytes done / bytes total` of a running backup.
    pub progress: Option<(u64, u64)>,
    /// This computer's folder on the backup disk, as of the last backup.
    pub folder: Option<String>,
    /// A backup in the old layout (0.7.3 and before) is still on the disk.
    pub old_layout: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicatesNotice {
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleNotice {
    pub at: String,
    pub from: Option<String>,
    pub to: String,
    pub reason: String,
}

fn meta(conn: &Connection, key: &str) -> Option<String> {
    db::get_meta(conn, key)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
}

pub fn collect(env: &KbEnv) -> Result<KbStatus, String> {
    let mut status = KbStatus {
        data_dir: env.data.root().display().to_string(),
        db_exists: false,
        sessions_by_source: BTreeMap::new(),
        deleted_sources: 0,
        last_run: None,
        running: false,
        lock_holder: lock::read_info(&env.data.lock()),
        activity: "idle",
        backup: BackupMeta::default(),
        schedule_notice: None,
        duplicates_notice: None,
        data_bytes: super::fsutil::dir_size(env.data.root()),
    };
    let Some(conn) = db::open_readonly(&env.data.db())? else {
        return Ok(status);
    };
    status.db_exists = true;

    let mut stmt = conn
        .prepare("SELECT source, COUNT(*) FROM sessions WHERE cwd_excluded = 0 GROUP BY source")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (source, count) = row.map_err(|e| e.to_string())?;
        status
            .sessions_by_source
            .insert(source, u64::try_from(count).unwrap_or(0));
    }
    status.deleted_sources = conn
        .query_row(
            "SELECT COUNT(*) FROM files f JOIN sessions s ON s.file_id = f.id \
             WHERE f.source_status = 'deleted'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_or(0, |n| u64::try_from(n).unwrap_or(0));
    status.last_run = last_run(&conn);
    status.running = status
        .last_run
        .as_ref()
        .is_some_and(|run| run.finished_at.is_none())
        && status
            .lock_holder
            .as_ref()
            .is_some_and(|h| process_alive(h.pid));

    status.backup = BackupMeta {
        last_at: meta(&conn, backup::META_LAST_AT),
        last_result: meta(&conn, backup::META_LAST_RESULT),
        last_skipped_at: meta(&conn, backup::META_LAST_SKIPPED_AT),
        last_bytes: meta(&conn, backup::META_LAST_BYTES).and_then(|v| v.parse().ok()),
        progress: meta(&conn, backup::META_PROGRESS).and_then(|v| {
            let (done, total) = v.split_once('/')?;
            Some((done.parse().ok()?, total.parse().ok()?))
        }),
        folder: meta(&conn, backup::META_FOLDER),
        old_layout: meta(&conn, backup::META_OLD_LAYOUT).as_deref() == Some("1"),
    };
    // A process that died mid-backup leaves "running" behind; only believe it
    // while the lock holder is alive.
    let holder_alive = status
        .lock_holder
        .as_ref()
        .is_some_and(|h| process_alive(h.pid));
    let backing_up = holder_alive && meta(&conn, backup::META_STATE).as_deref() == Some("running");
    status.activity = if backing_up {
        "backing_up"
    } else if status.running {
        "collecting"
    } else {
        "idle"
    };
    status.running = status.running || backing_up;
    if !backing_up {
        status.backup.progress = None;
    }
    status.schedule_notice = meta(&conn, schedule::META_REPAIRED_AT).map(|at| ScheduleNotice {
        at,
        from: meta(&conn, schedule::META_REPAIRED_FROM),
        to: meta(&conn, schedule::META_REPAIRED_TO).unwrap_or_default(),
        reason: meta(&conn, schedule::META_REPAIRED_REASON).unwrap_or_default(),
    });
    status.duplicates_notice = super::dedupe::unseen(&conn).map(|r| DuplicatesNotice {
        files: r.files,
        bytes: r.bytes,
    });
    Ok(status)
}

fn last_run(conn: &Connection) -> Option<LastRun> {
    conn.query_row(
        "SELECT started_at, finished_at, trigger, ok, summary_json FROM sync_runs \
         ORDER BY id DESC LIMIT 1",
        [],
        |r| {
            let summary: Option<String> = r.get(4)?;
            Ok(LastRun {
                started_at: r.get(0)?,
                finished_at: r.get(1)?,
                trigger: r.get(2)?,
                ok: r.get::<_, Option<i64>>(3)?.map(|v| v != 0),
                summary: summary.and_then(|s| serde_json::from_str(&s).ok()),
            })
        },
    )
    .ok()
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(not(unix))]
fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        super::platform::process_alive(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        true
    }
}

/// A collected session shaped like the base project's `ClaudeSession`, so the
/// existing viewer can open it unchanged, plus what only the KB knows.
#[allow(clippy::struct_excessive_bools)] // mirrors the base `ClaudeSession` shape
#[derive(Debug, Clone, Serialize)]
pub struct KbSession {
    // --- ClaudeSession-compatible ---
    pub session_id: String,
    pub actual_session_id: String,
    pub file_path: String,
    pub project_name: String,
    pub message_count: u64,
    pub first_message_time: String,
    pub last_message_time: String,
    pub last_modified: String,
    pub has_tool_use: bool,
    pub has_errors: bool,
    pub summary: Option<String>,
    pub is_renamed: bool,
    pub provider: String,
    pub storage_type: &'static str,
    pub entrypoint: Option<String>,
    // --- KB only ---
    pub cwd: Option<String>,
    pub source_status: String,
    /// The device an imported session came from.
    pub origin: Option<String>,
    pub source_path: String,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SessionFilter {
    pub source: Option<String>,
    pub only_deleted: Option<bool>,
    pub include_subagents: Option<bool>,
    /// Sessions with no user/assistant messages (metadata-only files).
    pub include_empty: Option<bool>,
    /// Sessions whose working directory the user excluded.
    pub include_excluded: Option<bool>,
    /// Only sessions imported from another device whose source is not here.
    pub only_elsewhere: Option<bool>,
    /// `cli`, `vscode`, `exec`… as recorded by the indexer.
    pub client: Option<String>,
    pub project: Option<String>,
    /// Exact working directory (used to open a session in its project).
    pub cwd: Option<String>,
    /// Lower / upper bound on last activity, RFC 3339.
    pub since: Option<String>,
    pub until: Option<String>,
    /// Substring of the title.
    pub query: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

const DEFAULT_LIMIT: u32 = 500;
const MAX_LIMIT: u32 = 5000;

pub fn list_sessions(env: &KbEnv, filter: &SessionFilter) -> Result<Vec<KbSession>, String> {
    let Some(conn) = db::open_readonly(&env.data.db())? else {
        return Ok(Vec::new());
    };
    let limit = filter.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let origin = db::origin_column(&conn);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT s.source, s.session_id, f.mirror_rel, f.source_path, {status}, \
                    s.project_name, s.cwd, s.title, s.title_source, s.client, s.started_at, \
                    s.last_activity_at, f.last_synced_at, s.message_count, \
                    s.parent_session_id, s.is_subagent, {origin} \
             FROM sessions s JOIN files f ON f.id = s.file_id \
             WHERE (?1 IS NULL OR s.source = ?1) \
               AND (?2 = 0 OR f.source_status = 'deleted') \
               AND (?3 = 1 OR s.is_subagent = 0) \
               AND (?6 = 1 OR s.message_count > 0) \
               AND (?7 IS NULL OR s.client = ?7) \
               AND (?8 IS NULL OR s.project_name = ?8) \
               AND (?9 IS NULL OR COALESCE(s.last_activity_at, f.last_synced_at) >= ?9) \
               AND (?10 IS NULL OR COALESCE(s.last_activity_at, f.last_synced_at) <= ?10) \
               AND (?11 IS NULL OR instr(lower(COALESCE(s.title, '')), lower(?11)) > 0) \
               AND (?12 IS NULL OR s.cwd = ?12) \
               AND (?13 = 1 OR s.cwd_excluded = 0) \
               AND (?14 = 0 OR ({origin} IS NOT NULL AND f.source_status = 'present')) \
             ORDER BY COALESCE(s.last_activity_at, f.last_synced_at) DESC, s.id DESC \
             LIMIT ?4 OFFSET ?5",
            status = db::status_expr(origin)
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                filter.source,
                i64::from(filter.only_deleted.unwrap_or(false)),
                i64::from(filter.include_subagents.unwrap_or(false)),
                i64::from(limit),
                i64::from(filter.offset.unwrap_or(0)),
                i64::from(filter.include_empty.unwrap_or(false)),
                filter.client,
                filter.project,
                filter.since,
                filter.until,
                filter.query.as_deref().filter(|q| !q.is_empty()),
                filter.cwd,
                i64::from(filter.include_excluded.unwrap_or(false)),
                i64::from(filter.only_elsewhere.unwrap_or(false)),
            ],
            |r| {
                let mirror_rel: String = r.get(2)?;
                let started: Option<String> = r.get(10)?;
                let last: Option<String> = r.get(11)?;
                let synced: String = r.get(12)?;
                let file_path = env
                    .data
                    .mirror_path(&mirror_rel)
                    .map_or_else(|_| mirror_rel.clone(), |p| p.to_string_lossy().into_owned());
                let title_source: Option<String> = r.get(8)?;
                Ok(KbSession {
                    session_id: file_path.clone(),
                    actual_session_id: r.get(1)?,
                    file_path,
                    project_name: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    cwd: r.get(6)?,
                    summary: r.get(7)?,
                    is_renamed: matches!(
                        title_source.as_deref(),
                        Some("threads" | "session_index")
                    ),
                    entrypoint: r.get(9)?,
                    first_message_time: started.clone().unwrap_or_default(),
                    last_message_time: last.clone().unwrap_or_default(),
                    last_modified: last.or(started).unwrap_or(synced),
                    message_count: u64::try_from(r.get::<_, i64>(13)?).unwrap_or(0),
                    has_tool_use: false,
                    has_errors: false,
                    provider: r.get(0)?,
                    storage_type: "jsonl",
                    source_path: r.get(3)?,
                    source_status: r.get(4)?,
                    origin: r.get(16)?,
                    parent_session_id: r.get(14)?,
                    is_subagent: r.get::<_, i64>(15)? != 0,
                })
            },
        )
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Facets {
    pub clients: Vec<String>,
    /// `(source, project name)` pairs.
    pub projects: Vec<(String, String)>,
}

/// Values the filter drop-downs offer.
pub fn facets(env: &KbEnv) -> Result<Facets, String> {
    let Some(conn) = db::open_readonly(&env.data.db())? else {
        return Ok(Facets::default());
    };
    let mut out = Facets::default();
    let mut stmt = conn
        .prepare("SELECT DISTINCT client FROM sessions WHERE client IS NOT NULL AND cwd_excluded = 0 ORDER BY client")
        .map_err(|e| e.to_string())?;
    out.clients = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT source, project_name FROM sessions \
             WHERE project_name IS NOT NULL AND project_name <> '' AND cwd_excluded = 0 \
             ORDER BY project_name COLLATE NOCASE, source",
        )
        .map_err(|e| e.to_string())?;
    out.projects = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    Ok(out)
}
