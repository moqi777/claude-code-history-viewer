//! Notes: what the daily digest wrote about each session.
//!
//! The files under `<data dir>/notes/<source>/<session id>.json` are the
//! source of truth, exactly like `raw/`: an AI wrote them, and regenerating
//! them costs the user's quota. `kb.db` only holds a derived copy (`notes`,
//! `notes_fts`, `digest_sessions`) that can be rebuilt from the files at any
//! time ([`rebuild_all`]) and is checked against them by a content hash
//! ([`reconcile`]).
//!
//! A note points back at its evidence by message `seq` (plus the `msg_key` of
//! the first message). The numbering is the one `index::messages` documents;
//! changing it invalidates existing notes.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::fsutil;
use super::index::tokenize;
use super::paths::DataPaths;

pub const NOTES_SCHEMA: u32 = 1;
/// Bump when the way notes are indexed changes; the notes (only) are then
/// re-indexed from the files on the next run.
pub const NOTES_INDEX_VERSION: &str = "1";
const NOTES_INDEX_VERSION_KEY: &str = "notes_index_version";

const NOTES_DDL: &str = "
    CREATE TABLE notes (
      id          INTEGER PRIMARY KEY,
      source      TEXT NOT NULL CHECK (source IN ('claude','codex')),
      session_id  TEXT NOT NULL,
      entry_id    TEXT NOT NULL,
      title       TEXT NOT NULL,
      problem     TEXT NOT NULL,
      cause       TEXT NOT NULL,
      solution    TEXT NOT NULL,
      conclusion  TEXT NOT NULL,
      keywords    TEXT NOT NULL,
      seq_from    INTEGER NOT NULL,
      seq_to      INTEGER NOT NULL,
      msg_key     TEXT,
      msg_at      TEXT,
      msg_kind    TEXT,
      round_id    TEXT NOT NULL,
      digested_at TEXT NOT NULL,
      engine      TEXT NOT NULL,
      model       TEXT NOT NULL,
      UNIQUE (source, session_id, entry_id)
    );
    CREATE INDEX idx_notes_session ON notes(source, session_id);
    CREATE VIRTUAL TABLE notes_fts USING fts5(
      head, kw, body,
      content='', contentless_delete=1,
      tokenize='unicode61 remove_diacritics 0'
    );
    CREATE TABLE digest_sessions (
      source        TEXT NOT NULL,
      session_id    TEXT NOT NULL,
      status        TEXT NOT NULL CHECK (status IN ('notes','nothing','partial')),
      digested_seq  INTEGER NOT NULL,
      digested_at   TEXT NOT NULL,
      note_hash     TEXT NOT NULL,
      PRIMARY KEY (source, session_id)
    );
";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub seq_from: u64,
    pub seq_to: u64,
    /// The `msg_key` of the `seq_from` message (Claude only): the opener
    /// looks the message up by it first and falls back to `seq`.
    #[serde(default)]
    pub msg_key: Option<String>,
    /// When that message was sent, and what it was (`user`, `assistant`…):
    /// Codex messages have no key, so the opener finds them by time and role.
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInfo {
    pub cli: String,
    pub cli_version: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub problem: String,
    pub cause: String,
    pub solution: String,
    pub conclusion: String,
    pub keywords: Vec<String>,
    pub evidence: Evidence,
    pub round: String,
    pub digested_at: String,
    pub engine: EngineInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryItem {
    pub round: String,
    pub at: String,
    pub seq_from: u64,
    pub seq_to: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// `notes` (has entries), `nothing` (digested, nothing worth keeping) or
/// `partial` (a long session digested only up to `digested_through_seq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotesFile {
    pub schema: u32,
    pub source: String,
    pub session_id: String,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub status: String,
    pub digested_through_seq: u64,
    pub digested_at: String,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub history: Vec<HistoryItem>,
    /// Fields a newer program wrote; kept so an older one does not drop them.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl NotesFile {
    pub fn empty(source: &str, session_id: &str) -> Self {
        Self {
            schema: NOTES_SCHEMA,
            source: source.to_string(),
            session_id: session_id.to_string(),
            session_title: None,
            cwd: None,
            status: "nothing".into(),
            digested_through_seq: 0,
            digested_at: String::new(),
            entries: Vec::new(),
            history: Vec::new(),
            extra: Map::new(),
        }
    }

    /// Next unused `n<k>` id: ids only grow, a replaced entry keeps its own.
    pub fn next_entry_id(&self) -> String {
        let highest = self
            .entries
            .iter()
            .filter_map(|e| e.id.strip_prefix('n').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        format!("n{}", highest + 1)
    }
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// The note file of a session, `None` when it was never digested.
pub fn read(data: &DataPaths, source: &str, session_id: &str) -> Result<Option<NotesFile>, String> {
    let path = data.notes_file(source, session_id)?;
    match std::fs::read(&path) {
        Ok(bytes) => parse(&bytes, source, session_id)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn parse(bytes: &[u8], source: &str, session_id: &str) -> Result<NotesFile, String> {
    let file: NotesFile =
        serde_json::from_slice(bytes).map_err(|e| format!("not a note file: {e}"))?;
    if file.schema > NOTES_SCHEMA {
        return Err(format!(
            "written by a newer AgentKB (note schema {})",
            file.schema
        ));
    }
    if file.source != source || file.session_id != session_id {
        return Err("the file names a different session than its path".to_string());
    }
    if !matches!(file.status.as_str(), "notes" | "nothing" | "partial") {
        return Err(format!("unknown status {:?}", file.status));
    }
    Ok(file)
}

/// Write atomically; returns the content hash to record in the database.
pub fn write(data: &DataPaths, file: &NotesFile) -> Result<String, String> {
    let path = data.notes_file(&file.source, &file.session_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    text.push('\n');
    fsutil::write_atomic(&path, text.as_bytes())?;
    Ok(hash_bytes(text.as_bytes()))
}

fn db_err(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
}

/// Replace the derived rows of one session with the content of its file.
pub fn index_file(conn: &Connection, file: &NotesFile, hash: &str) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| db_err("begin notes index", e))?;
    delete_rows(&tx, &file.source, &file.session_id)?;
    for entry in &file.entries {
        let keywords = serde_json::to_string(&entry.keywords).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO notes (source, session_id, entry_id, title, problem, cause, solution, \
             conclusion, keywords, seq_from, seq_to, msg_key, msg_at, msg_kind, round_id, digested_at, engine, model) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                file.source,
                file.session_id,
                entry.id,
                entry.title,
                entry.problem,
                entry.cause,
                entry.solution,
                entry.conclusion,
                keywords,
                i64::try_from(entry.evidence.seq_from).unwrap_or(i64::MAX),
                i64::try_from(entry.evidence.seq_to).unwrap_or(i64::MAX),
                entry.evidence.msg_key,
                entry.evidence.at,
                entry.evidence.kind,
                entry.round,
                entry.digested_at,
                entry.engine.cli,
                entry.engine.model,
            ],
        )
        .map_err(|e| db_err("insert note", e))?;
        let id = tx.last_insert_rowid();
        let head = tokenize::index_text(&format!("{} {}", entry.title, entry.conclusion));
        let kw = tokenize::index_text(&entry.keywords.join(" "));
        let body = tokenize::index_text(&format!(
            "{} {} {}",
            entry.problem, entry.cause, entry.solution
        ));
        tx.execute(
            "INSERT INTO notes_fts (rowid, head, kw, body) VALUES (?1,?2,?3,?4)",
            params![id, head, kw, body],
        )
        .map_err(|e| db_err("index note", e))?;
    }
    tx.execute(
        "INSERT INTO digest_sessions (source, session_id, status, digested_seq, digested_at, note_hash) \
         VALUES (?1,?2,?3,?4,?5,?6) \
         ON CONFLICT(source, session_id) DO UPDATE SET status=excluded.status, \
           digested_seq=excluded.digested_seq, digested_at=excluded.digested_at, \
           note_hash=excluded.note_hash",
        params![
            file.source,
            file.session_id,
            file.status,
            i64::try_from(file.digested_through_seq).unwrap_or(i64::MAX),
            file.digested_at,
            hash
        ],
    )
    .map_err(|e| db_err("record digest state", e))?;
    tx.commit().map_err(|e| db_err("commit notes index", e))
}

fn delete_rows(conn: &Connection, source: &str, session_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM notes_fts WHERE rowid IN (SELECT id FROM notes WHERE source=?1 AND session_id=?2)",
        params![source, session_id],
    )
    .map_err(|e| db_err("unindex notes", e))?;
    conn.execute(
        "DELETE FROM notes WHERE source=?1 AND session_id=?2",
        params![source, session_id],
    )
    .map_err(|e| db_err("delete notes", e))?;
    conn.execute(
        "DELETE FROM digest_sessions WHERE source=?1 AND session_id=?2",
        params![source, session_id],
    )
    .map_err(|e| db_err("delete digest state", e))?;
    Ok(())
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Rebuilt {
    pub files: usize,
    pub entries: usize,
    /// Files that could not be used (path and reason); never deleted.
    pub skipped: Vec<(String, String)>,
}

/// Every note file in `notes/`, as `(source, session id, path)`. Symlinks and
/// anything with an unsafe name are ignored.
fn note_files(data: &DataPaths) -> Vec<(String, String, std::path::PathBuf)> {
    let mut out = Vec::new();
    for source in ["claude", "codex"] {
        let Ok(dir) = std::fs::read_dir(data.notes().join(source)) else {
            continue;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if super::paths::is_safe_id(id) {
                out.push((source.to_string(), id.to_string(), path));
            }
        }
    }
    out.sort();
    out
}

/// Drop the derived tables and rebuild them from `notes/`. The only way notes
/// get back into a fresh `kb.db`. Recreating the tables (instead of emptying
/// them) also repairs a database whose note tables were lost.
pub fn rebuild_all(conn: &Connection, data: &DataPaths) -> Result<Rebuilt, String> {
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS notes_fts; DROP TABLE IF EXISTS notes; \
         DROP TABLE IF EXISTS digest_sessions; {NOTES_DDL}"
    ))
    .map_err(|e| db_err("recreate note tables", e))?;
    let mut report = Rebuilt::default();
    for (source, id, path) in note_files(data) {
        match load_one(conn, &source, &id, &path) {
            Ok(entries) => {
                report.files += 1;
                report.entries += entries;
            }
            Err(reason) => report.skipped.push((path.display().to_string(), reason)),
        }
    }
    super::db::set_meta(conn, NOTES_INDEX_VERSION_KEY, NOTES_INDEX_VERSION)?;
    Ok(report)
}

fn load_one(conn: &Connection, source: &str, id: &str, path: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let file = parse(&bytes, source, id)?;
    index_file(conn, &file, &hash_bytes(&bytes))?;
    Ok(file.entries.len())
}

/// Make the database agree with the files: reload files whose hash differs
/// from the recorded one (or that the database never saw) and forget sessions
/// whose file is gone. Cheap enough to run on every sync. Also rebuilds
/// everything when the note tables are missing or the index version is old.
pub fn reconcile(conn: &Connection, data: &DataPaths) -> Result<Rebuilt, String> {
    let has_tables: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('notes','notes_fts','digest_sessions')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|e| db_err("look for note tables", e))?
        == 3;
    let version = super::db::get_meta(conn, NOTES_INDEX_VERSION_KEY)?;
    if !has_tables || version.as_deref() != Some(NOTES_INDEX_VERSION) {
        return rebuild_all(conn, data);
    }
    let mut known: HashMap<(String, String), String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT source, session_id, note_hash FROM digest_sessions")
            .map_err(|e| db_err("read digest state", e))?;
        let rows = stmt
            .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))
            .map_err(|e| db_err("read digest state", e))?;
        for row in rows {
            let (key, hash) = row.map_err(|e| db_err("read digest state", e))?;
            known.insert(key, hash);
        }
    }
    let mut report = Rebuilt::default();
    for (source, id, path) in note_files(data) {
        let recorded = known.remove(&(source.clone(), id.clone()));
        let current = std::fs::read(&path).map(|b| hash_bytes(&b));
        if matches!((&recorded, &current), (Some(a), Ok(b)) if a == b) {
            continue;
        }
        match load_one(conn, &source, &id, &path) {
            Ok(entries) => {
                report.files += 1;
                report.entries += entries;
            }
            Err(reason) => report.skipped.push((path.display().to_string(), reason)),
        }
    }
    // What is left had a database row but no file any more.
    for (source, id) in known.into_keys() {
        delete_rows(conn, &source, &id)?;
    }
    Ok(report)
}

/// How far a session has been digested, `None` when it never was.
pub fn digested_seq(
    conn: &Connection,
    source: &str,
    session_id: &str,
) -> Result<Option<u64>, String> {
    conn.query_row(
        "SELECT digested_seq FROM digest_sessions WHERE source=?1 AND session_id=?2",
        params![source, session_id],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .map(|v| v.map(|n| u64::try_from(n).unwrap_or(0)))
    .map_err(|e| db_err("read digested seq", e))
}

#[cfg(test)]
pub mod testing {
    use super::*;

    pub fn sample(source: &str, id: &str, entries: usize) -> NotesFile {
        let mut file = NotesFile::empty(source, id);
        file.status = if entries == 0 { "nothing" } else { "notes" }.into();
        file.digested_through_seq = 42;
        file.digested_at = "2026-10-16T03:00:00Z".into();
        for n in 1..=entries {
            file.entries.push(Entry {
                id: format!("n{n}"),
                title: format!("死锁排查 {n}"),
                problem: "线程池里出现死锁".into(),
                cause: "两个锁的获取顺序相反".into(),
                solution: "统一加锁顺序".into(),
                conclusion: "按固定顺序加锁".into(),
                keywords: vec!["死锁".into(), "deadlock".into()],
                evidence: Evidence {
                    seq_from: 3,
                    seq_to: 9,
                    msg_key: Some("uuid-3".into()),
                    at: Some("2026-10-16T01:00:00Z".into()),
                    kind: Some("user".into()),
                },
                round: "2026-10-16".into(),
                digested_at: "2026-10-16T03:00:00Z".into(),
                engine: EngineInfo {
                    cli: "claude".into(),
                    cli_version: "2.1.300".into(),
                    model: "m".into(),
                },
            });
        }
        file
    }
}

#[cfg(test)]
mod tests {
    use super::testing::sample;
    use super::*;

    fn setup() -> (tempfile::TempDir, DataPaths, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let data = DataPaths::new(dir.path().join("AgentKB"));
        let conn = super::super::db::open(&data.db()).unwrap();
        (dir, data, conn)
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_written_note_reads_back_identically_and_unknown_fields_survive() {
        let (_dir, data, _conn) = setup();
        let mut file = sample("claude", "abc-1", 2);
        file.extra
            .insert("from_the_future".into(), Value::from("keep me"));
        write(&data, &file).unwrap();
        let back = read(&data, "claude", "abc-1").unwrap().unwrap();
        assert_eq!(back, file);
        assert!(read(&data, "claude", "missing").unwrap().is_none());
    }

    #[test]
    fn a_file_for_another_session_or_a_newer_schema_is_refused() {
        let (_dir, data, _conn) = setup();
        let mut file = sample("claude", "abc-1", 1);
        write(&data, &file).unwrap();
        let path = data.notes_file("claude", "abc-1").unwrap();
        std::fs::copy(&path, data.notes().join("claude/other.json")).unwrap();
        assert!(read(&data, "claude", "other").is_err());
        file.schema = 99;
        write(&data, &file).unwrap();
        assert!(read(&data, "claude", "abc-1").is_err());
    }

    #[test]
    fn indexing_a_file_fills_the_tables_and_replaces_older_rows() {
        let (_dir, data, conn) = setup();
        let file = sample("claude", "abc-1", 2);
        let hash = write(&data, &file).unwrap();
        index_file(&conn, &file, &hash).unwrap();
        assert_eq!((count(&conn, "notes"), count(&conn, "notes_fts")), (2, 2));
        assert_eq!(digested_seq(&conn, "claude", "abc-1").unwrap(), Some(42));

        let smaller = sample("claude", "abc-1", 1);
        index_file(&conn, &smaller, "h2").unwrap();
        assert_eq!((count(&conn, "notes"), count(&conn, "notes_fts")), (1, 1));
    }

    #[test]
    fn rebuild_restores_everything_from_the_files_even_when_the_tables_are_gone() {
        let (_dir, data, conn) = setup();
        for (source, id, n) in [
            ("claude", "a-1", 2),
            ("codex", "b_2", 1),
            ("claude", "c-3", 0),
        ] {
            let file = sample(source, id, n);
            let hash = write(&data, &file).unwrap();
            index_file(&conn, &file, &hash).unwrap();
        }
        conn.execute_batch("DROP TABLE notes_fts; DROP TABLE notes; DROP TABLE digest_sessions;")
            .unwrap();
        let report = rebuild_all(&conn, &data).unwrap();
        assert_eq!((report.files, report.entries), (3, 3));
        assert_eq!(count(&conn, "notes"), 3);
        assert_eq!(count(&conn, "digest_sessions"), 3);
        assert_eq!(digested_seq(&conn, "claude", "c-3").unwrap(), Some(42));
    }

    #[test]
    fn reconcile_reloads_changed_files_and_forgets_deleted_ones() {
        let (_dir, data, conn) = setup();
        reconcile(&conn, &data).unwrap();
        let file = sample("claude", "a-1", 1);
        let hash = write(&data, &file).unwrap();
        index_file(&conn, &file, &hash).unwrap();

        // Unchanged: nothing is touched.
        assert_eq!(reconcile(&conn, &data).unwrap().files, 0);
        // Edited behind our back: reloaded.
        write(&data, &sample("claude", "a-1", 2)).unwrap();
        assert_eq!(reconcile(&conn, &data).unwrap().files, 1);
        assert_eq!(count(&conn, "notes"), 2);
        // A file the database never saw: picked up.
        write(&data, &sample("codex", "z-9", 1)).unwrap();
        assert_eq!(reconcile(&conn, &data).unwrap().files, 1);
        assert_eq!(count(&conn, "digest_sessions"), 2);
        // Deleted file: its rows go.
        std::fs::remove_file(data.notes_file("claude", "a-1").unwrap()).unwrap();
        reconcile(&conn, &data).unwrap();
        assert_eq!(count(&conn, "digest_sessions"), 1);
        assert_eq!(count(&conn, "notes"), 1);
    }

    #[test]
    fn unreadable_files_are_reported_not_deleted() {
        let (_dir, data, conn) = setup();
        let dir = data.notes().join("claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad-1.json"), "{ not json").unwrap();
        let report = rebuild_all(&conn, &data).unwrap();
        assert_eq!(report.skipped.len(), 1);
        assert!(dir.join("bad-1.json").exists());
    }

    #[test]
    fn entry_ids_only_grow() {
        let mut file = sample("claude", "a", 3);
        assert_eq!(file.next_entry_id(), "n4");
        file.entries.remove(0);
        assert_eq!(file.next_entry_id(), "n4");
        assert_eq!(NotesFile::empty("claude", "a").next_entry_id(), "n1");
    }
}
