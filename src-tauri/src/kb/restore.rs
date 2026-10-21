//! Restore the library from a backup folder (`AgentKB-backup/` on the disk).
//!
//! [`inspect`] only looks. [`restore`] builds the whole new library in a work
//! folder inside the data directory (copy, verify, bring the database up to
//! date), and only when all of it is ready swaps it in with renames: what is
//! there now is moved into `before-restore-<time>/`, never deleted. A failure
//! before the swap changes nothing; a failure during it moves everything back.
//!
//! Not backed up, so not restored: the semantic index (`vectors/`, derived
//! data, built again), the model (`models/`, kept in place), the settings
//! (`config.json`, untouched) and the logs.
//!
//! The backup copy of `raw/` keeps growing after the database snapshot was
//! taken, so the snapshot's cursors may stop short of the end of the files.
//! The tail is indexed from the mirror during the restore ([`catch_up`]), so
//! the library is complete even when the original sources are not here.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::config::SourceRoots;
use super::db;
use super::fsutil;
use super::import::local_state;
use super::lock::{self, SyncLock};
use super::log::SyncLog;
use super::notes;
use super::paths::DataPaths;
use super::progress::ProgressSink;
use super::sync::{self, KbEnv};
use super::transfer::{hex, now_stamp, Cancel, Meter, RemoveOnDrop, TResult, TransferError};

const SPACE_MARGIN_PERCENT: u64 = 110;
const OP: &str = "restore";
/// The lock file of the vector builder lives inside `vectors/`; it stays put.
const EMBED_LOCK_NAME: &str = "embed.lock";

/// What a backup folder holds, as far as can be told without changing anything.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Inspect {
    pub sessions: u64,
    pub notes: u64,
    pub raw_files: u64,
    pub raw_bytes: u64,
    pub note_files: u64,
    /// When `kb.db` in the backup was last written (RFC 3339, UTC).
    pub snapshot_at: String,
    pub schema_version: i64,
    /// The snapshot is older than this program and will be migrated.
    pub needs_upgrade: bool,
    pub free_bytes: Option<u64>,
    pub needed_bytes: u64,
    /// What the library here holds now, which would be set aside.
    pub current_sessions: u64,
    pub current_notes: u64,
    pub current_empty: bool,
    /// Whose backup this is; filled in by the command, `None` for an old-layout one.
    pub owner: Option<crate::kb::backup_dir::Owner>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct RestoreReport {
    pub sessions: u64,
    pub notes: u64,
    pub raw_files: u64,
    pub raw_bytes: u64,
    /// Where the previous library went; `None` when there was nothing.
    pub set_aside: Option<String>,
    /// Files whose backup grew after the snapshot, and the bytes indexed from them.
    pub caught_up_files: u64,
    pub caught_up_bytes: u64,
    /// Files the database knows whose backup copy is missing.
    pub missing_files: u64,
    /// Sessions whose source file was recorded under another folder (another
    /// computer, another user name) and now point to where this computer keeps it.
    pub rehomed_files: u64,
    pub warnings: Vec<String>,
    pub duration_ms: u64,
}

// ---------------------------------------------------------------------------
// Looking
// ---------------------------------------------------------------------------

fn is_junk(rel: &str) -> bool {
    // exFAT and macOS leave `._name` companions next to every file.
    rel.split('/').any(|part| part.starts_with("._"))
}

fn backup_files(dir: &Path) -> Vec<(String, u64)> {
    let mut files: Vec<(String, u64)> = fsutil::walk_files(dir)
        .into_iter()
        .filter(|(rel, _)| !is_junk(rel))
        .collect();
    files.sort();
    files
}

fn check_folder(env: &KbEnv, folder: &Path) -> TResult<()> {
    if !folder.is_dir() {
        return Err(TransferError::new(
            "backup_incomplete",
            format!("{} is not a folder", folder.display()),
        ));
    }
    if !folder.join("kb.db").is_file() {
        return Err(TransferError::new(
            "backup_incomplete",
            "the folder has no kb.db (the database snapshot)",
        ));
    }
    if !folder.join("raw").is_dir() {
        return Err(TransferError::new(
            "backup_incomplete",
            "the folder has no raw/ (the backed-up sessions)",
        ));
    }
    let (a, b) = (
        folder
            .canonicalize()
            .unwrap_or_else(|_| folder.to_path_buf()),
        env.data
            .root()
            .canonicalize()
            .unwrap_or_else(|_| env.data.root().to_path_buf()),
    );
    if a.starts_with(&b) {
        return Err(TransferError::new(
            "bad_source",
            "the backup folder is inside the data folder it would replace",
        ));
    }
    Ok(())
}

struct DbFacts {
    schema_version: i64,
    sessions: u64,
    notes: u64,
}

/// Open the local copy read-only (never migrating), refuse a database that is
/// damaged or newer than this program, and count what it holds.
fn read_facts(copy: &Path) -> TResult<DbFacts> {
    let conn = Connection::open_with_flags(
        copy,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| TransferError::corrupt(format!("cannot open the database snapshot: {e}")))?;
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| TransferError::corrupt(format!("integrity check failed to run: {e}")))?;
    if verdict != "ok" {
        return Err(TransferError::corrupt(format!(
            "the database snapshot is damaged: {verdict}"
        )));
    }
    let schema_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| TransferError::corrupt(format!("cannot read the schema version: {e}")))?;
    if schema_version > db::latest_version() {
        return Err(TransferError::new(
            "too_new",
            format!(
                "the backup was made by a newer version (schema v{schema_version}, this program knows v{}); update the app first",
                db::latest_version()
            ),
        ));
    }
    let tables = |name: &str| -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .is_ok()
    };
    if schema_version == 0 || !tables("files") || !tables("sessions") {
        return Err(TransferError::corrupt(
            "kb.db is not an AgentKB database (no files/sessions tables)",
        ));
    }
    let count = |sql: &str| -> u64 {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0))
            .map_or(0, |n| u64::try_from(n).unwrap_or(0))
    };
    Ok(DbFacts {
        schema_version,
        sessions: count("SELECT COUNT(*) FROM sessions"),
        notes: if tables("notes") {
            count("SELECT COUNT(*) FROM (SELECT DISTINCT source, session_id FROM notes)")
        } else {
            0
        },
    })
}

fn modified_at(path: &Path) -> String {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .unwrap_or_default()
}

/// Look at a backup folder: refuse what cannot be restored, say what it holds.
/// Nothing under the data directory outlives the call.
pub fn inspect(env: &KbEnv, folder: &Path) -> TResult<Inspect> {
    check_folder(env, folder)?;
    let data = &env.data;
    fs::create_dir_all(data.root()).map_err(|e| TransferError::io(format!("data folder: {e}")))?;
    let work = data
        .root()
        .join(format!(".restore-look-{}", std::process::id()));
    let _guard = RemoveOnDrop(work.clone());
    fs::create_dir_all(&work).map_err(|e| TransferError::io(format!("work folder: {e}")))?;
    let copy = work.join("kb.db");
    // The disk never opens SQLite: the snapshot is copied here first.
    copy_verified(&folder.join("kb.db"), &copy, &mut |_| {})?;
    let facts = read_facts(&copy)?;
    let raw = backup_files(&folder.join("raw"));
    let notes_files = backup_files(&folder.join("notes"));
    let raw_bytes: u64 = raw.iter().map(|(_, n)| n).sum();
    let notes_bytes: u64 = notes_files.iter().map(|(_, n)| n).sum();
    let db_bytes = fs::metadata(&copy).map_or(0, |m| m.len());
    let current = local_state(data);
    Ok(Inspect {
        sessions: facts.sessions,
        notes: facts.notes,
        raw_files: raw.len() as u64,
        raw_bytes,
        note_files: notes_files.len() as u64,
        snapshot_at: modified_at(&folder.join("kb.db")),
        schema_version: facts.schema_version,
        needs_upgrade: facts.schema_version < db::latest_version(),
        free_bytes: fsutil::free_space(data.root()),
        needed_bytes: (raw_bytes + notes_bytes + 2 * db_bytes) / 100 * SPACE_MARGIN_PERCENT,
        current_sessions: current.sessions,
        current_notes: current.notes,
        current_empty: current.empty,
        owner: None,
    })
}

// ---------------------------------------------------------------------------
// Copying
// ---------------------------------------------------------------------------

/// Copy `src` to `dst`, hashing what was read, then read `dst` back and
/// compare size and SHA-256.
fn copy_verified(src: &Path, dst: &Path, on_bytes: &mut dyn FnMut(u64)) -> TResult<u64> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| TransferError::io(format!("cannot create {}: {e}", parent.display())))?;
    }
    let io =
        |what: &str, e: std::io::Error| TransferError::io(format!("{what} {}: {e}", src.display()));
    let mut input = File::open(src).map_err(|e| io("cannot open", e))?;
    let mut output = File::create(dst).map_err(|e| io("cannot write a copy of", e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        let n = input.read(&mut buf).map_err(|e| io("cannot read", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output
            .write_all(&buf[..n])
            .map_err(|e| io("cannot write a copy of", e))?;
        total += n as u64;
        on_bytes(n as u64);
    }
    output
        .sync_all()
        .map_err(|e| io("cannot flush a copy of", e))?;
    drop(output);
    let want = hex(&hasher.finalize());
    let got = fsutil::sha256_file(dst).map_err(|e| io("cannot read back the copy of", e))?;
    let size = fs::metadata(dst).map_or(0, |m| m.len());
    if got != want || size != total {
        return Err(TransferError::corrupt(format!(
            "the copy of {} does not match the original",
            src.display()
        )));
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// The swap
// ---------------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    pub static DIE_AT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

#[allow(unused_variables, clippy::unnecessary_wraps)]
fn step(name: &'static str) -> TResult<()> {
    #[cfg(test)]
    if DIE_AT.with(std::cell::Cell::get) == Some(name) {
        return Err(TransferError::io(format!("simulated crash at {name}")));
    }
    Ok(())
}

/// Renames done so far, so that a failure can undo them in reverse.
#[derive(Default)]
struct Moves(Vec<(PathBuf, PathBuf)>);

impl Moves {
    fn go(&mut self, from: &Path, to: &Path) -> TResult<()> {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                TransferError::io(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        fs::rename(from, to).map_err(|e| {
            TransferError::io(format!(
                "cannot move {} to {}: {e}",
                from.display(),
                to.display()
            ))
        })?;
        self.0.push((from.to_path_buf(), to.to_path_buf()));
        Ok(())
    }

    fn undo(&mut self) -> Vec<String> {
        let mut failed = Vec::new();
        while let Some((from, to)) = self.0.pop() {
            if let Err(e) = fs::rename(&to, &from) {
                failed.push(format!("{} -> {}: {e}", to.display(), from.display()));
            }
        }
        failed
    }
}

/// The entries of `vectors/` except the builder's lock and its Windows diagnostic.
fn vector_entries(data: &DataPaths) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(super::semantic::store::dir(data)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            name != EMBED_LOCK_NAME && !(cfg!(windows) && name == ".embed.lock.info")
        })
        .map(|e| e.path())
        .collect()
}

fn swap(data: &DataPaths, staging: &Path, aside: &Path) -> TResult<bool> {
    let root = data.root();
    let mut moves = Moves::default();
    let mut moved_any = false;
    let result = (|| -> TResult<()> {
        for name in ["kb.db", "kb.db-wal", "kb.db-shm", "raw", "notes"] {
            let from = root.join(name);
            if from.exists() {
                moves.go(&from, &aside.join(name))?;
                moved_any = true;
            }
        }
        for entry in vector_entries(data) {
            let name = entry.file_name().unwrap_or_default().to_os_string();
            moves.go(&entry, &aside.join("vectors").join(name))?;
            moved_any = true;
        }
        step("after_aside")?;
        moves.go(&staging.join("raw"), &data.raw())?;
        step("after_raw")?;
        moves.go(&staging.join("notes"), &data.notes())?;
        step("after_notes")?;
        moves.go(&staging.join("kb.db"), &data.db())?;
        Ok(())
    })();
    if let Err(e) = result {
        let stuck = moves.undo();
        if stuck.is_empty() {
            return Err(e);
        }
        return Err(TransferError::io(format!(
            "{}; and these could not be moved back (they are in {}): {}",
            e.message,
            aside.display(),
            stuck.join("; ")
        )));
    }
    fsutil::fsync_dir(root);
    Ok(moved_any)
}

// ---------------------------------------------------------------------------
// Catching up
// ---------------------------------------------------------------------------

struct CaughtUp {
    files: u64,
    bytes: u64,
    missing: u64,
    warnings: Vec<String>,
}

/// Index what the backed-up files hold past the snapshot's cursors, and count
/// the files the database knows that are not in the backup.
fn catch_up(
    staged_env: &KbEnv,
    conn: &Connection,
    cancel: &Cancel,
    meter: &mut Meter<'_>,
) -> TResult<CaughtUp> {
    let rows = db::load_files(conn)?;
    meter.phase("catchup", rows.len() as u64, 0);
    let mut out = CaughtUp {
        files: 0,
        bytes: 0,
        missing: 0,
        warnings: Vec::new(),
    };
    for row in &rows {
        cancel.check()?;
        let present = staged_env
            .data
            .mirror_path(&row.mirror_rel)
            .is_ok_and(|p| p.is_file());
        if present {
            match sync::index_mirror_tail(staged_env, conn, row) {
                Ok(0) => {}
                Ok(bytes) => {
                    out.files += 1;
                    out.bytes += bytes;
                }
                Err(message) => out.warnings.push(format!("{}: {message}", row.mirror_rel)),
            }
        } else {
            out.missing += 1;
        }
        meter.file_done();
    }
    Ok(out)
}

/// Where a file of the backup lives on this computer: its place in `raw/`
/// mirrors its place under the source folder (see `discover.rs`).
fn local_source_path(roots: &SourceRoots, mirror_rel: &str) -> Option<PathBuf> {
    let (root, rest) = if let Some(rest) = mirror_rel.strip_prefix("claude/") {
        (roots.claude.as_ref()?, rest)
    } else {
        let rest = mirror_rel.strip_prefix("codex/")?;
        (roots.codex.as_ref()?, rest)
    };
    Some(rest.split('/').fold(root.clone(), |p, part| p.join(part)))
}

/// A snapshot made on another computer, or under another user name, records
/// where each source file was. Point the rows to where this computer keeps
/// them, or the next collection would find the same file under a second name.
fn rehome(conn: &Connection, roots: &SourceRoots) -> TResult<u64> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| TransferError::io(format!("begin: {e}")))?;
    let mut changed = 0u64;
    for row in db::load_files(conn)? {
        let Some(local) = local_source_path(roots, &row.mirror_rel) else {
            continue;
        };
        let local = local.to_string_lossy().into_owned();
        if local == row.source_path {
            continue;
        }
        tx.execute(
            "UPDATE files SET source_path=?2 WHERE id=?1",
            rusqlite::params![row.id, local],
        )
        .map_err(|e| TransferError::io(format!("cannot update a source path: {e}")))?;
        changed += 1;
    }
    tx.commit()
        .map_err(|e| TransferError::io(format!("commit: {e}")))?;
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Restoring
// ---------------------------------------------------------------------------

pub fn restore(
    env: &KbEnv,
    folder: &Path,
    cancel: &Cancel,
    progress: &ProgressSink,
) -> TResult<RestoreReport> {
    let started = Instant::now();
    check_folder(env, folder)?;
    let data = &env.data;
    fs::create_dir_all(data.root()).map_err(|e| TransferError::io(format!("data folder: {e}")))?;

    // No waiting: a collection, a digest or an index build that is running
    // means "not now".
    if let Some(info) = lock::held_by_live_process(&data.digest_lock()) {
        return Err(TransferError::new(
            "digest_running",
            format!("a digest is running (since {})", info.started_at),
        ));
    }
    let _sync = SyncLock::acquire(&data.lock(), Duration::ZERO)?.ok_or_else(|| {
        TransferError::new("busy", "a collection is running; try again in a moment")
    })?;
    let _digest = SyncLock::acquire(&data.digest_lock(), Duration::ZERO)?
        .ok_or_else(|| TransferError::new("digest_running", "a digest is running"))?;
    let _embed = SyncLock::acquire(&super::semantic::build::lock_path(data), Duration::ZERO)?
        .ok_or_else(|| TransferError::new("embed_running", "the semantic index is being built"))?;
    cancel.check()?;

    let log = SyncLog::open(&data.logs());
    let stamp = now_stamp();
    let staging = data.root().join(format!(".restore-{stamp}"));
    let _guard = RemoveOnDrop(staging.clone());
    fs::create_dir_all(&staging).map_err(|e| TransferError::io(format!("work folder: {e}")))?;
    log.line(&format!("restore start from {}", folder.display()));
    let mut meter = Meter::new(progress, OP);

    // 1. The database: copied here, checked here.
    meter.phase("check", 1, 0);
    let staged_db = staging.join("kb.db");
    copy_verified(&folder.join("kb.db"), &staged_db, &mut |_| {})?;
    read_facts(&staged_db)?;
    meter.file_done();

    // 2. The files.
    let raw = backup_files(&folder.join("raw"));
    let note_files = backup_files(&folder.join("notes"));
    let total: u64 = raw.iter().chain(&note_files).map(|(_, n)| n).sum();
    if let Some(free) = fsutil::free_space(data.root()) {
        let needed = total / 100 * SPACE_MARGIN_PERCENT;
        if free < needed {
            return Err(TransferError::new(
                "no_space",
                format!("{needed} bytes are needed, {free} are free"),
            ));
        }
    }
    meter.phase("copy", (raw.len() + note_files.len()) as u64, total);
    for (dir, files) in [("raw", &raw), ("notes", &note_files)] {
        fs::create_dir_all(staging.join(dir))
            .map_err(|e| TransferError::io(format!("work folder: {e}")))?;
        for (rel, _) in files {
            cancel.check()?;
            copy_verified(
                &folder.join(dir).join(rel),
                &staging.join(dir).join(rel),
                &mut |n| meter.add_bytes(n),
            )?;
            meter.file_done();
        }
    }
    step("after_copy")?;

    // 3. Up to date: migrate, take in what the files hold past the snapshot,
    //    rebuild the note index from the note files.
    let staged_data = DataPaths::new(&staging);
    let mut staged_env = env.clone();
    staged_env.data = staged_data.clone();
    let conn = db::open(&staged_db)?;
    let rehomed = rehome(&conn, &env.roots)?;
    let caught = catch_up(&staged_env, &conn, cancel, &mut meter)?;
    let mut warnings = caught.warnings;
    match notes::reconcile(&conn, &staged_data) {
        Ok(rebuilt) => {
            for (path, reason) in rebuilt.skipped {
                warnings.push(format!("note file {path}: {reason}"));
            }
        }
        Err(message) => warnings.push(format!("notes: {message}")),
    }
    // One self-contained file, whatever the snapshot's journal mode was.
    conn.pragma_update(None, "journal_mode", "DELETE")
        .map_err(|e| TransferError::io(format!("cannot finish kb.db: {e}")))?;
    let sessions = count(&conn, "SELECT COUNT(*) FROM sessions");
    let notes_count = count(
        &conn,
        "SELECT COUNT(*) FROM (SELECT DISTINCT source, session_id FROM notes)",
    );
    conn.close()
        .map_err(|(_, e)| TransferError::io(format!("cannot finish kb.db: {e}")))?;
    for side in ["-wal", "-shm"] {
        let _ = fs::remove_file(staging.join(format!("kb.db{side}")));
    }
    if !staging.join("notes").exists() {
        fs::create_dir_all(staging.join("notes"))
            .map_err(|e| TransferError::io(format!("work folder: {e}")))?;
    }
    cancel.check()?;

    // 4. The swap. From here on there is no cancelling.
    meter.phase("switch", 1, 0);
    let aside = data.root().join(format!("before-restore-{stamp}"));
    let moved = swap(data, &staging, &aside)?;
    meter.file_done();

    let report = RestoreReport {
        sessions,
        notes: notes_count,
        raw_files: raw.len() as u64,
        raw_bytes: raw.iter().map(|(_, n)| n).sum(),
        set_aside: moved.then(|| aside.display().to_string()),
        caught_up_files: caught.files,
        caught_up_bytes: caught.bytes,
        missing_files: caught.missing,
        rehomed_files: rehomed,
        warnings,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    log.line(&format!(
        "restore done: {} sessions, {} raw files, {} caught up ({} bytes), {} missing, set aside: {}",
        report.sessions,
        report.raw_files,
        report.caught_up_files,
        report.caught_up_bytes,
        report.missing_files,
        report.set_aside.as_deref().unwrap_or("nothing")
    ));
    Ok(report)
}

fn count(conn: &Connection, sql: &str) -> u64 {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0))
        .map_or(0, |n| u64::try_from(n).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::backup::online_backup;
    use crate::kb::config::SourceRoots;
    use std::path::PathBuf;

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";

    fn line(n: u32) -> String {
        format!(
            r#"{{"type":"user","timestamp":"2026-01-01T00:00:{n:02}Z","cwd":"/Users/me/proj","message":{{"role":"user","content":"message number {n}"}}}}"#
        ) + "\n"
    }

    struct Lib {
        env: KbEnv,
        claude: PathBuf,
    }

    impl Lib {
        fn new(root: &Path, name: &str) -> Self {
            let claude = root.join(name).join(".claude");
            fs::create_dir_all(claude.join("projects")).unwrap();
            let env = KbEnv {
                data: DataPaths::new(root.join(name).join("AgentKB")),
                roots: SourceRoots {
                    claude: Some(claude.clone()),
                    codex: None,
                },
                exclude_prefixes: vec![],
                exclude_cwd_prefixes: vec![],
                mirror: None,
                progress: ProgressSink::default(),
                force_reindex: false,
            };
            Self { env, claude }
        }

        fn source(&self) -> PathBuf {
            self.claude
                .join("projects/-Users-me-proj")
                .join(format!("{SESSION}.jsonl"))
        }

        fn write_source(&self, lines: std::ops::RangeInclusive<u32>) {
            let path = self.source();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let text: String = lines.map(line).collect();
            fs::write(&path, text).unwrap();
            // Different size is enough for the collector, the clock may not move.
        }

        fn sync(&self) -> sync::Summary {
            sync::run_sync(&self.env, "test").unwrap()
        }

        fn conn(&self) -> Connection {
            db::open(&self.env.data.db()).unwrap()
        }

        fn message_count(&self) -> i64 {
            self.conn()
                .query_row("SELECT SUM(message_count) FROM sessions", [], |r| r.get(0))
                .unwrap_or(0)
        }
    }

    /// What `backup.rs` leaves on the disk: a snapshot, `raw/`, `notes/`, plus
    /// the `._` companions exFAT adds.
    fn backup_of(lib: &Lib, folder: &Path) {
        fs::create_dir_all(folder).unwrap();
        online_backup(&lib.conn(), &folder.join("kb.db")).unwrap();
        copy_raw(lib, folder);
    }

    fn copy_raw(lib: &Lib, folder: &Path) {
        for dir in ["raw", "notes"] {
            fs::create_dir_all(folder.join(dir)).unwrap();
            for (rel, _) in fsutil::walk_files(&lib.env.data.root().join(dir)) {
                let to = folder.join(dir).join(&rel);
                fs::create_dir_all(to.parent().unwrap()).unwrap();
                fs::copy(lib.env.data.root().join(dir).join(&rel), &to).unwrap();
                let junk =
                    to.with_file_name(format!("._{}", to.file_name().unwrap().to_string_lossy()));
                fs::write(junk, b"\0\x05\x16\x07").unwrap();
            }
        }
    }

    fn run(env: &KbEnv, folder: &Path) -> TResult<RestoreReport> {
        restore(env, folder, &Cancel::new(), &ProgressSink::default())
    }

    fn digest_of(root: &Path) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = fsutil::walk_files(root)
            .into_iter()
            .filter(|(rel, _)| !rel.starts_with("logs/") && !rel.contains(".lock"))
            .map(|(rel, _)| {
                let hash = fsutil::sha256_file(&root.join(&rel)).unwrap();
                (rel, hash)
            })
            .collect();
        out.sort();
        out
    }

    fn no_work_folders(data: &DataPaths) {
        let leftovers: Vec<String> = fs::read_dir(data.root())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".restore-"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    struct World {
        dir: tempfile::TempDir,
        folder: PathBuf,
        source: Lib,
        target: Lib,
    }

    /// A library with one session, backed up; and a second, other library
    /// that will be restored over.
    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let source = Lib::new(dir.path(), "a");
        source.write_source(1..=3);
        source.sync();
        let folder = dir.path().join("disk/AgentKB-backup");
        backup_of(&source, &folder);
        let target = Lib::new(dir.path(), "b");
        World {
            dir,
            folder,
            source,
            target,
        }
    }

    #[test]
    fn inspect_reports_what_is_in_the_backup_and_skips_exfat_companions() {
        let w = world();
        let info = inspect(&w.target.env, &w.folder).unwrap();
        assert_eq!(info.sessions, 1);
        assert_eq!(info.raw_files, 1, "the ._ file is not counted");
        assert!(info.current_empty);
        assert!(!info.needs_upgrade);
        assert_ne!(info.snapshot_at, "");
        assert!(
            fs::read_dir(w.target.env.data.root())
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with(".restore-")),
            "nothing outlives the call"
        );
    }

    #[test]
    fn a_restore_makes_the_library_searchable_and_openable_and_skips_junk() {
        let w = world();
        let report = run(&w.target.env, &w.folder).unwrap();
        assert_eq!(report.sessions, 1);
        assert_eq!(report.set_aside, None, "nothing was there to set aside");
        assert_eq!(w.target.message_count(), 3);
        let raw = w.target.env.data.raw();
        let files = fsutil::walk_files(&raw);
        assert_eq!(files.len(), 1);
        assert!(files.iter().all(|(rel, _)| !rel.contains("._")));
        assert_eq!(
            digest_of(&raw),
            digest_of(&w.source.env.data.raw()),
            "byte for byte"
        );
        no_work_folders(&w.target.env.data);
        let hits = crate::kb::search::search_in(
            &w.target.env.data,
            &crate::kb::search::SearchParams {
                query: "number".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!hits.results.is_empty(), "the session can be found");
    }

    #[test]
    fn what_was_there_is_set_aside_not_deleted_and_the_vector_index_is_emptied() {
        let w = world();
        let other = &w.target;
        fs::create_dir_all(other.claude.join("projects/-x")).unwrap();
        fs::write(
            other
                .claude
                .join("projects/-x/99999999-0000-0000-0000-000000000000.jsonl"),
            line(1),
        )
        .unwrap();
        other.sync();
        let data = &other.env.data;
        let vectors = super::super::semantic::store::dir(data);
        fs::create_dir_all(&vectors).unwrap();
        fs::write(vectors.join("model.sqlite3"), b"vectors").unwrap();
        fs::write(vectors.join(EMBED_LOCK_NAME), b"").unwrap();
        fs::create_dir_all(data.root().join("models")).unwrap();
        fs::write(data.root().join("models/keep.bin"), b"model").unwrap();
        let before = digest_of(data.root());

        let report = run(&other.env, &w.folder).unwrap();
        let aside = PathBuf::from(report.set_aside.clone().unwrap());
        assert!(aside
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("before-restore-"));
        // Everything that was there is in the set-aside folder, unchanged.
        let moved = digest_of(&aside);
        for (rel, hash) in before
            .iter()
            .filter(|(rel, _)| !rel.starts_with("models/") && !rel.starts_with("logs/"))
        {
            assert!(
                moved.contains(&(rel.clone(), hash.clone())),
                "{rel} was not set aside intact"
            );
        }
        assert_eq!(other.message_count(), 3, "the restored library is live");
        assert!(
            !vectors.join("model.sqlite3").exists(),
            "the index is empty"
        );
        assert!(vectors.join(EMBED_LOCK_NAME).exists());
        assert!(!aside.join("vectors/.embed.lock.info").exists());
        assert!(!vectors.join(".embed.lock.info").exists());
        assert!(data.root().join("models/keep.bin").exists(), "models stay");
    }

    #[test]
    fn a_backup_without_a_database_or_with_a_damaged_one_is_refused_and_nothing_changes() {
        let w = world();
        w.target.write_source(1..=2);
        w.target.sync();
        let before = digest_of(w.target.env.data.root());

        fs::remove_file(w.folder.join("kb.db")).unwrap();
        let err = run(&w.target.env, &w.folder).unwrap_err();
        assert_eq!(err.code, "backup_incomplete");

        fs::write(w.folder.join("kb.db"), vec![7u8; 5000]).unwrap();
        let err = run(&w.target.env, &w.folder).unwrap_err();
        assert!(["corrupt"].contains(&err.code), "{err}");

        let good = Lib::new(
            w.target.env.data.root().parent().unwrap().parent().unwrap(),
            "c",
        );
        good.write_source(1..=1);
        good.sync();
        fs::remove_file(w.folder.join("kb.db")).unwrap();
        online_backup(&good.conn(), &w.folder.join("kb.db")).unwrap();
        fs::remove_dir_all(w.folder.join("raw")).unwrap();
        let err = run(&w.target.env, &w.folder).unwrap_err();
        assert_eq!(err.code, "backup_incomplete", "no raw/");

        assert_eq!(digest_of(w.target.env.data.root()), before);
        no_work_folders(&w.target.env.data);
    }

    #[test]
    fn a_snapshot_from_a_newer_program_is_refused() {
        let w = world();
        let conn = Connection::open(w.folder.join("kb.db")).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        let err = run(&w.target.env, &w.folder).unwrap_err();
        assert_eq!(err.code, "too_new");
        assert_eq!(
            inspect(&w.target.env, &w.folder).unwrap_err().code,
            "too_new"
        );
    }

    #[test]
    fn a_folder_inside_the_data_folder_is_refused() {
        let w = world();
        let inside = w.target.env.data.root().join("copy");
        fs::create_dir_all(&inside).unwrap();
        fs::copy(w.folder.join("kb.db"), inside.join("kb.db")).unwrap();
        fs::create_dir_all(inside.join("raw")).unwrap();
        assert_eq!(run(&w.target.env, &inside).unwrap_err().code, "bad_source");
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_that_fails_halfway_changes_nothing_and_leaves_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let w = world();
        w.target.write_source(1..=2);
        w.target.sync();
        let before = digest_of(w.target.env.data.root());
        let victim = fsutil::walk_files(&w.folder.join("raw"))
            .into_iter()
            .map(|(rel, _)| w.folder.join("raw").join(rel))
            .find(|p| !p.file_name().unwrap().to_string_lossy().starts_with("._"))
            .unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o000)).unwrap();
        let err = run(&w.target.env, &w.folder).unwrap_err();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(err.code, "io", "{err}");
        assert_eq!(digest_of(w.target.env.data.root()), before);
        no_work_folders(&w.target.env.data);
        assert!(
            !fs::read_dir(w.target.env.data.root())
                .unwrap()
                .flatten()
                .any(|e| e
                    .file_name()
                    .to_string_lossy()
                    .starts_with("before-restore-")),
            "nothing was set aside"
        );
    }

    #[test]
    fn running_collection_digest_or_index_build_means_not_now() {
        let w = world();
        w.target.write_source(1..=2);
        w.target.sync();
        let data = &w.target.env.data;
        let before = digest_of(data.root());
        for (path, code) in [
            (data.lock(), "busy"),
            (data.digest_lock(), "digest_running"),
            (
                super::super::semantic::build::lock_path(data),
                "embed_running",
            ),
        ] {
            let held = SyncLock::acquire(&path, Duration::ZERO).unwrap().unwrap();
            let started = Instant::now();
            let err = run(&w.target.env, &w.folder).unwrap_err();
            assert_eq!(err.code, code);
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "it does not wait"
            );
            drop(held);
        }
        assert_eq!(digest_of(data.root()), before);
        assert!(run(&w.target.env, &w.folder).is_ok(), "free again");
    }

    #[test]
    fn a_failure_in_the_middle_of_the_swap_puts_everything_back() {
        for point in ["after_aside", "after_raw", "after_notes"] {
            let w = world();
            w.target.write_source(1..=2);
            w.target.sync();
            let data = &w.target.env.data;
            let vectors = super::super::semantic::store::dir(data);
            fs::create_dir_all(&vectors).unwrap();
            fs::write(vectors.join("model.sqlite3"), b"vectors").unwrap();
            let before = digest_of(data.root());
            DIE_AT.with(|d| d.set(Some(point)));
            let err = run(&w.target.env, &w.folder).unwrap_err();
            DIE_AT.with(|d| d.set(None));
            assert!(err.message.contains("simulated"), "{point}: {err}");
            assert_eq!(digest_of(data.root()), before, "{point}: back as it was");
            assert_eq!(w.target.message_count(), 2);
            no_work_folders(data);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_open_files_block_restore_and_all_prior_moves_are_undone() {
        use std::os::windows::fs::OpenOptionsExt;

        // Deny FILE_SHARE_DELETE using a real Windows handle, not DIE_AT.
        // The vector case fails after the database, raw and notes were moved.
        for locked in ["kb.db", "notes/held.json", "vectors/model.sqlite3"] {
            let w = world();
            w.target.write_source(1..=2);
            w.target.sync();
            let data = &w.target.env.data;
            for (rel, bytes) in [
                ("notes/held.json", b"old notes".as_slice()),
                ("vectors/model.sqlite3", b"old vectors".as_slice()),
                ("models/keep.bin", b"model".as_slice()),
            ] {
                let path = data.root().join(rel);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, bytes).unwrap();
            }
            let before = digest_of(data.root());
            let backup_before = digest_of(&w.folder);
            let sources_before = digest_of(&w.target.claude);
            let held = fs::OpenOptions::new()
                .read(true)
                .share_mode(0x1 | 0x2) // FILE_SHARE_READ | FILE_SHARE_WRITE
                .open(data.root().join(locked))
                .unwrap();
            let err = run(&w.target.env, &w.folder).unwrap_err();
            assert_eq!(err.code, "io", "{locked}: {err}");
            assert!(err.message.contains("cannot move"), "{locked}: {err}");
            let blocked_move = if locked.starts_with("notes/") {
                data.notes()
            } else {
                data.root().join(locked)
            };
            assert!(
                err.message
                    .replace('\\', "/")
                    .contains(&blocked_move.to_string_lossy().replace('\\', "/")),
                "{locked}: failed at a different move: {err}"
            );
            assert!(
                !err.message.contains("could not be moved back"),
                "{locked}: rollback failed: {err}"
            );
            assert_eq!(digest_of(data.root()), before, "{locked}: full rollback");
            assert_eq!(digest_of(&w.folder), backup_before, "backup is read only");
            assert_eq!(digest_of(&w.target.claude), sources_before);
            no_work_folders(data);
            drop(held);
            assert_eq!(w.target.message_count(), 2, "original database still live");
            let report = run(&w.target.env, &w.folder).unwrap();
            assert_eq!(report.sessions, 1);
            assert_eq!(w.target.message_count(), 3, "restore works after release");
            assert!(report.set_aside.is_some());
            assert!(!data.root().join("vectors/model.sqlite3").exists());
            assert_eq!(
                fs::read(data.root().join("models/keep.bin")).unwrap(),
                b"model"
            );
            no_work_folders(data);
            eprintln!("Windows held file {locked}: rollback intact; restore after release passed");
        }
    }

    #[test]
    fn files_that_grew_after_the_snapshot_are_indexed_from_the_backup_and_collect_does_not_duplicate(
    ) {
        let w = world();
        // The source grows; the backup of raw/ follows, the snapshot does not.
        w.source.write_source(1..=6);
        w.source.sync();
        copy_raw(&w.source, &w.folder);

        let report = run(&w.target.env, &w.folder).unwrap();
        assert_eq!(report.caught_up_files, 1);
        assert!(report.caught_up_bytes > 0);
        assert_eq!(w.target.message_count(), 6, "the tail is in the index");
        assert_eq!(report.missing_files, 0);
        assert_eq!(
            report.rehomed_files, 1,
            "the source folder here is another one"
        );

        // A first collection here: the source is the same file, six lines long.
        fs::create_dir_all(w.target.source().parent().unwrap()).unwrap();
        fs::copy(w.source.source(), w.target.source()).unwrap();
        let summary = w.target.sync();
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        assert_eq!(w.target.message_count(), 6, "no duplicates");
        // and it goes on from there
        w.target.write_source(1..=8);
        let summary = w.target.sync();
        assert!(summary.errors.is_empty());
        assert_eq!(w.target.message_count(), 8);
        let mirrored = fs::read_to_string(
            fsutil::walk_files(&w.target.env.data.raw())
                .first()
                .map(|(rel, _)| w.target.env.data.raw().join(rel))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(mirrored, (1..=8).map(line).collect::<String>());
    }

    #[test]
    fn a_collection_with_no_sources_here_after_the_restore_changes_nothing() {
        let w = world();
        w.source.write_source(1..=5);
        w.source.sync();
        copy_raw(&w.source, &w.folder);
        run(&w.target.env, &w.folder).unwrap();
        let before = w.target.message_count();
        let summary = w.target.sync();
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        assert_eq!(w.target.message_count(), before);
        assert_eq!(before, 5);
        let again = w.target.sync();
        assert_eq!(again.changed_files, 0);
    }

    #[test]
    fn a_file_the_database_knows_but_the_backup_lacks_is_counted() {
        let w = world();
        for (rel, _) in fsutil::walk_files(&w.folder.join("raw")) {
            let _ = fs::remove_file(w.folder.join("raw").join(rel));
        }
        let report = run(&w.target.env, &w.folder).unwrap();
        assert_eq!(report.missing_files, 1);
        assert_eq!(report.sessions, 1);
    }

    #[test]
    fn the_backups_of_several_computers_are_listed_and_each_can_be_restored() {
        use crate::kb::backup_dir;
        let w = world();
        let root = w.folder.parent().unwrap().join("shared/AgentKB-backup");
        // The old layout: the backup directly in the configured folder.
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        copy_dir(&w.folder, &root);
        let old = backup_dir::list(&root, None);
        assert_eq!(old.len(), 1);
        assert!(old[0].old_layout);
        // Two computers' folders next to it.
        for (id, name) in [("aaaaaaaa11111111", "Mac"), ("bbbbbbbb22222222", "Win")] {
            let dir = root.join(format!("{name}-{}", &id[..8]));
            copy_dir(&w.folder, &dir);
            let marker = backup_dir::Marker {
                id: id.into(),
                name: name.into(),
                os: if name == "Win" { "windows" } else { "macos" }.into(),
                last_backup_at: "2026-10-19T08:00:00Z".into(),
                sessions: 1,
                ..Default::default()
            };
            fs::write(
                dir.join(backup_dir::MARKER_NAME),
                serde_json::to_string(&marker).unwrap(),
            )
            .unwrap();
        }
        let entries = backup_dir::list(&root, Some("bbbbbbbb22222222"));
        assert_eq!(entries.len(), 3);
        let win = entries.iter().find(|e| e.name == "Win").unwrap();
        assert!(win.this_computer && win.os == "windows" && win.sessions == 1);
        for entry in &entries {
            let target = Lib::new(w.dir.path(), &format!("t-{}", entry.name.len()));
            let info = inspect(&target.env, Path::new(&entry.folder)).unwrap();
            assert_eq!(info.sessions, 1);
        }
        let report = run(&w.target.env, Path::new(&win.folder)).unwrap();
        assert_eq!(report.sessions, 1);
        assert_eq!(w.target.message_count(), 3);
    }

    fn copy_dir(from: &Path, to: &Path) {
        for (rel, _) in fsutil::walk_files(from) {
            let dst = to.join(&rel);
            fs::create_dir_all(dst.parent().unwrap()).unwrap();
            fs::copy(from.join(&rel), dst).unwrap();
        }
    }

    /// By hand, on a real backup folder (read only; everything is written to a
    /// temporary data folder): `P73_BACKUP=/Volumes/X/AgentKB-backup cargo test
    /// restore_a_real_backup -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a real backup folder in P73_BACKUP"]
    fn restore_a_real_backup() {
        let folder = PathBuf::from(std::env::var("P73_BACKUP").expect("P73_BACKUP"));
        let dir = tempfile::tempdir().unwrap();
        let target = Lib::new(dir.path(), "real");
        let started = Instant::now();
        let info = inspect(&target.env, &folder).unwrap();
        eprintln!("inspect {:?}: {info:?}", started.elapsed());
        let started = Instant::now();
        let seen = std::sync::Mutex::new(Vec::new());
        let sink = ProgressSink::new(move |p| {
            if p.done == p.total || p.phase != "copy" {
                seen.lock()
                    .unwrap()
                    .push(format!("{} {}/{}", p.phase, p.done, p.total));
            }
        });
        let report = restore(&target.env, &folder, &Cancel::new(), &sink).unwrap();
        eprintln!("restore {:?}: {report:#?}", started.elapsed());
        let first = target.sync();
        eprintln!(
            "first collection: changed={} errors={:?}",
            first.changed_files, first.errors
        );
        let second = target.sync();
        eprintln!("second collection: changed={}", second.changed_files);
        eprintln!(
            "raw files: {}",
            fsutil::walk_files(&target.env.data.raw()).len()
        );
        let hits = crate::kb::search::search_in(
            &target.env.data,
            &crate::kb::search::SearchParams {
                query: "go-ios".into(),
                ..Default::default()
            },
        )
        .unwrap();
        eprintln!("search go-ios: {} sessions", hits.results.len());
    }
}
