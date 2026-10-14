//! `kb.db`: cursors and the session index. Disposable by design: everything in
//! it can be rebuilt from `raw/`, so it holds no user-authored data.

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};

use super::index::SessionRecord;
use super::Source;

/// Migration N upgrades schema version N-1 to N. Append only; never edit a
/// released entry.
const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE meta (
      key   TEXT PRIMARY KEY,
      value TEXT NOT NULL
    );
    CREATE TABLE files (
      id             INTEGER PRIMARY KEY,
      source         TEXT NOT NULL CHECK (source IN ('claude','codex')),
      source_path    TEXT NOT NULL UNIQUE,
      mirror_rel     TEXT NOT NULL UNIQUE,
      src_size       INTEGER NOT NULL,
      src_mtime_ns   INTEGER NOT NULL,
      head_hash      BLOB    NOT NULL,
      head_len       INTEGER NOT NULL,
      mirror_offset  INTEGER NOT NULL,
      is_compressed  INTEGER NOT NULL DEFAULT 0,
      source_status  TEXT NOT NULL DEFAULT 'present' CHECK (source_status IN ('present','deleted')),
      deleted_at     TEXT,
      first_seen_at  TEXT NOT NULL,
      last_synced_at TEXT NOT NULL
    );
    CREATE TABLE sessions (
      id                 INTEGER PRIMARY KEY,
      file_id            INTEGER NOT NULL UNIQUE REFERENCES files(id) ON DELETE CASCADE,
      source             TEXT NOT NULL,
      session_id         TEXT NOT NULL,
      client             TEXT,
      cwd                TEXT,
      project_name       TEXT,
      title              TEXT,
      title_source       TEXT,
      fallback_title     TEXT,
      started_at         TEXT,
      last_activity_at   TEXT,
      message_count      INTEGER NOT NULL DEFAULT 0,
      parent_session_id  TEXT,
      is_subagent        INTEGER NOT NULL DEFAULT 0,
      indexer_state      TEXT NOT NULL DEFAULT '{}',
      unknown_types      TEXT NOT NULL DEFAULT '{}'
    );
    CREATE INDEX idx_sessions_session_id ON sessions(source, session_id);
    CREATE INDEX idx_sessions_activity   ON sessions(last_activity_at DESC);
    CREATE TABLE sync_runs (
      id            INTEGER PRIMARY KEY,
      started_at    TEXT NOT NULL,
      finished_at   TEXT,
      trigger       TEXT NOT NULL,
      summary_json  TEXT,
      ok            INTEGER
    );
    ",
];

const SYNC_RUNS_KEPT: i64 = 500;

pub fn latest_version() -> i64 {
    i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX)
}

fn err(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
}

/// Open (creating if needed) and migrate. Refuses a database written by a newer
/// program version instead of guessing at it.
pub fn open(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| err("cannot create data directory", e))?;
    }
    let mut conn = Connection::open(path).map_err(|e| err("cannot open kb.db", e))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| err("busy_timeout", e))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
    )
    .map_err(|e| err("pragma", e))?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// For GUI queries. Never migrates; `Ok(None)` when no database exists yet.
pub fn open_readonly(path: &Path) -> Result<Option<Connection>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| err("cannot open kb.db read-only", e))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| err("busy_timeout", e))?;
    Ok(Some(conn))
}

fn migrate(conn: &mut Connection) -> Result<(), String> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| err("user_version", e))?;
    if current > latest_version() {
        return Err(format!(
            "kb.db schema v{current} is newer than this program supports (v{}); \
             update the app instead of downgrading",
            latest_version()
        ));
    }
    if current == latest_version() {
        return Ok(());
    }
    let tx = conn.transaction().map_err(|e| err("begin migration", e))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        tx.execute_batch(sql)
            .map_err(|e| err(&format!("migration v{}", index + 1), e))?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {}", latest_version()))
        .map_err(|e| err("set user_version", e))?;
    tx.commit().map_err(|e| err("commit migration", e))
}

#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: i64,
    pub source: Source,
    pub source_path: String,
    pub mirror_rel: String,
    pub src_size: i64,
    pub src_mtime_ns: i64,
    pub head_hash: Vec<u8>,
    pub head_len: i64,
    pub mirror_offset: i64,
    pub is_compressed: bool,
    pub deleted: bool,
}

const FILE_COLUMNS: &str = "id, source, source_path, mirror_rel, src_size, src_mtime_ns, \
     head_hash, head_len, mirror_offset, is_compressed, source_status";

fn file_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    let source: String = row.get(1)?;
    let status: String = row.get(10)?;
    Ok(FileRow {
        id: row.get(0)?,
        source: Source::parse(&source).unwrap_or(Source::Claude),
        source_path: row.get(2)?,
        mirror_rel: row.get(3)?,
        src_size: row.get(4)?,
        src_mtime_ns: row.get(5)?,
        head_hash: row.get(6)?,
        head_len: row.get(7)?,
        mirror_offset: row.get(8)?,
        is_compressed: row.get::<_, i64>(9)? != 0,
        deleted: status == "deleted",
    })
}

pub fn load_files(conn: &Connection) -> Result<Vec<FileRow>, String> {
    let mut stmt = conn
        .prepare(&format!("SELECT {FILE_COLUMNS} FROM files"))
        .map_err(|e| err("prepare files", e))?;
    let rows = stmt
        .query_map([], file_from_row)
        .map_err(|e| err("query files", e))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| err("read files", e))
}

/// The cursor fields that change on every sync of a file.
#[derive(Debug, Clone)]
pub struct Cursor {
    pub src_size: i64,
    pub src_mtime_ns: i64,
    pub head_hash: Vec<u8>,
    pub head_len: i64,
    pub mirror_offset: i64,
}

pub struct NewFile<'a> {
    pub source: Source,
    pub source_path: &'a str,
    pub mirror_rel: &'a str,
    pub is_compressed: bool,
}

pub fn insert_file(
    tx: &Transaction<'_>,
    file: &NewFile<'_>,
    cursor: &Cursor,
    now: &str,
) -> Result<i64, String> {
    tx.execute(
        "INSERT INTO files (source, source_path, mirror_rel, src_size, src_mtime_ns, head_hash, \
         head_len, mirror_offset, is_compressed, source_status, first_seen_at, last_synced_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'present',?10,?10)",
        params![
            file.source.as_str(),
            file.source_path,
            file.mirror_rel,
            cursor.src_size,
            cursor.src_mtime_ns,
            cursor.head_hash,
            cursor.head_len,
            cursor.mirror_offset,
            i64::from(file.is_compressed),
            now
        ],
    )
    .map_err(|e| err("insert file", e))?;
    Ok(tx.last_insert_rowid())
}

/// Advance a file's cursor; also marks the source present again.
pub fn update_cursor(
    tx: &Transaction<'_>,
    file_id: i64,
    cursor: &Cursor,
    now: &str,
) -> Result<(), String> {
    tx.execute(
        "UPDATE files SET src_size=?2, src_mtime_ns=?3, head_hash=?4, head_len=?5, \
         mirror_offset=?6, source_status='present', deleted_at=NULL, last_synced_at=?7 \
         WHERE id=?1",
        params![
            file_id,
            cursor.src_size,
            cursor.src_mtime_ns,
            cursor.head_hash,
            cursor.head_len,
            cursor.mirror_offset,
            now
        ],
    )
    .map_err(|e| err("update cursor", e))?;
    Ok(())
}

pub fn mark_present_without_content_change(
    conn: &Connection,
    file_id: i64,
    now: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE files SET source_status='present', deleted_at=NULL, last_synced_at=?2 \
         WHERE id=?1 AND source_status!='present'",
        params![file_id, now],
    )
    .map_err(|e| err("mark present", e))?;
    Ok(())
}

pub fn set_source_path(tx: &Transaction<'_>, file_id: i64, new_path: &str) -> Result<(), String> {
    tx.execute(
        "UPDATE files SET source_path=?2, source_status='present', deleted_at=NULL WHERE id=?1",
        params![file_id, new_path],
    )
    .map_err(|e| err("move source path", e))?;
    Ok(())
}

pub fn mark_deleted(tx: &Transaction<'_>, file_id: i64, now: &str) -> Result<(), String> {
    tx.execute(
        "UPDATE files SET source_status='deleted', deleted_at=?2 WHERE id=?1 \
         AND source_status='present'",
        params![file_id, now],
    )
    .map_err(|e| err("mark deleted", e))?;
    Ok(())
}

/// Indexer snapshot for one file, as stored.
pub struct StoredSession {
    pub indexer_state: String,
}

pub fn load_indexer_state(
    conn: &Connection,
    file_id: i64,
) -> Result<Option<StoredSession>, String> {
    conn.query_row(
        "SELECT indexer_state FROM sessions WHERE file_id=?1",
        [file_id],
        |r| {
            Ok(StoredSession {
                indexer_state: r.get(0)?,
            })
        },
    )
    .optional()
    .map_err(|e| err("load indexer state", e))
}

/// Insert or replace the session row of a file. Returns true when it was new.
pub fn upsert_session(
    tx: &Transaction<'_>,
    file_id: i64,
    source: Source,
    record: &SessionRecord,
    indexer_state: &str,
    unknown_types: &str,
) -> Result<bool, String> {
    let existed: bool = tx
        .query_row("SELECT 1 FROM sessions WHERE file_id=?1", [file_id], |_| {
            Ok(())
        })
        .optional()
        .map_err(|e| err("check session", e))?
        .is_some();
    tx.execute(
        "INSERT INTO sessions (file_id, source, session_id, client, cwd, project_name, title, \
         title_source, fallback_title, started_at, last_activity_at, message_count, \
         parent_session_id, is_subagent, indexer_state, unknown_types) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?7,?9,?10,?11,?12,?13,?14,?15) \
         ON CONFLICT(file_id) DO UPDATE SET session_id=excluded.session_id, \
           client=excluded.client, cwd=excluded.cwd, project_name=excluded.project_name, \
           fallback_title=excluded.fallback_title, \
           title=CASE WHEN sessions.title_source IN ('threads','session_index') \
                      THEN sessions.title ELSE excluded.title END, \
           title_source=CASE WHEN sessions.title_source IN ('threads','session_index') \
                      THEN sessions.title_source ELSE excluded.title_source END, \
           started_at=excluded.started_at, last_activity_at=excluded.last_activity_at, \
           message_count=excluded.message_count, parent_session_id=excluded.parent_session_id, \
           is_subagent=excluded.is_subagent, indexer_state=excluded.indexer_state, \
           unknown_types=excluded.unknown_types",
        params![
            file_id,
            source.as_str(),
            record.session_id,
            record.client,
            record.cwd,
            record.project_name,
            record.title,
            record.title_source,
            record.started_at,
            record.last_activity_at,
            i64::try_from(record.message_count).unwrap_or(i64::MAX),
            record.parent_session_id,
            i64::from(record.is_subagent),
            indexer_state,
            unknown_types
        ],
    )
    .map_err(|e| err("upsert session", e))?;
    Ok(!existed)
}

/// A Codex session's identity and current title, for the native-title pass.
pub struct CodexTitleRow {
    pub id: i64,
    pub session_id: String,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub fallback_title: Option<String>,
}

pub fn load_codex_titles(conn: &Connection) -> Result<Vec<CodexTitleRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, title, title_source, fallback_title FROM sessions \
             WHERE source='codex'",
        )
        .map_err(|e| err("prepare titles", e))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(CodexTitleRow {
                id: r.get(0)?,
                session_id: r.get(1)?,
                title: r.get(2)?,
                title_source: r.get(3)?,
                fallback_title: r.get(4)?,
            })
        })
        .map_err(|e| err("query titles", e))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| err("read titles", e))
}

pub fn set_title(
    tx: &Transaction<'_>,
    id: i64,
    title: Option<&str>,
    title_source: &str,
) -> Result<(), String> {
    tx.execute(
        "UPDATE sessions SET title=?2, title_source=?3 WHERE id=?1",
        params![id, title, title_source],
    )
    .map_err(|e| err("set title", e))?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()
        .map_err(|e| err("read meta", e))
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )
    .map_err(|e| err("write meta", e))?;
    Ok(())
}

pub fn delete_meta(conn: &Connection, key: &str) -> Result<(), String> {
    conn.execute("DELETE FROM meta WHERE key=?1", [key])
        .map_err(|e| err("delete meta", e))?;
    Ok(())
}

pub fn begin_run(conn: &Connection, trigger: &str, now: &str) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO sync_runs (started_at, trigger) VALUES (?1, ?2)",
        params![now, trigger],
    )
    .map_err(|e| err("begin run", e))?;
    Ok(conn.last_insert_rowid())
}

pub fn finish_run(
    conn: &Connection,
    run_id: i64,
    now: &str,
    summary_json: &str,
    ok: bool,
) -> Result<(), String> {
    conn.execute(
        "UPDATE sync_runs SET finished_at=?2, summary_json=?3, ok=?4 WHERE id=?1",
        params![run_id, now, summary_json, i64::from(ok)],
    )
    .map_err(|e| err("finish run", e))?;
    conn.execute(
        "DELETE FROM sync_runs WHERE id <= (SELECT MAX(id) FROM sync_runs) - ?1",
        [SYNC_RUNS_KEPT],
    )
    .map_err(|e| err("prune runs", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_database_is_migrated_to_latest() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("kb.db")).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, latest_version());
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn reopening_is_idempotent_and_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kb.db");
        drop(open(&path).unwrap());
        drop(open(&path).unwrap());

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        drop(conn);
        let error = open(&path).unwrap_err();
        assert!(error.contains("newer"), "{error}");
    }

    #[test]
    fn readonly_open_of_missing_database_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(open_readonly(&dir.path().join("nope.db"))
            .unwrap()
            .is_none());
    }
}
