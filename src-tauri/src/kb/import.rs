//! Import an `.agentkb` file.
//!
//! Two calls, so a person can look before anything changes:
//!
//! * [`prepare`] unpacks into `<data dir>/.import-<id>/`, checking every file
//!   against the manifest, then fixes the database copy up for this machine
//!   (migrate, convert paths, mark sessions that came from elsewhere, rebuild
//!   the note index). The data directory itself is not touched. It returns
//!   what [`commit`] would do.
//! * [`commit`] takes the collection and digest locks and applies it: an
//!   empty library is switched over with renames (database last); a library
//!   with data is merged file by file. Nothing the user already has is
//!   deleted or overwritten.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::Serialize;

use super::backup::online_backup;
use super::config::KbConfig;
use super::db::{self, Cursor, FileRow, ImportedFile};
use super::fsutil;
use super::lock::{self, SyncLock};
use super::merge::{self, Action, LocalCounts, Plan};
use super::notes;
use super::pack::{self, Manifest, PortableSettings, SourceDevice};
use super::paths::DataPaths;
use super::progress::ProgressSink;
use super::sync::{self, KbEnv};
use super::transfer::{
    clean_stale_work_dirs, is_conflict_path, now_stamp, Cancel, Meter, RemoveOnDrop, TResult,
    TransferError,
};

const LOCK_WAIT: Duration = Duration::from_secs(60);
const SPACE_MARGIN_PERCENT: u64 = 110;
const STAGED_MARKER: &str = ".agentkb-staged";
const EXCLUDE_CWD_KEY: &str = "exclude_cwd_applied";
const INDEXER_VERSION_KEY: &str = "indexer_version";
/// Not a version: any value but the current one makes the next collection
/// rebuild the index from the mirror.
const INDEXING: &str = "importing";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Where things are on this machine, for [`convert_path`].
#[derive(Debug, Clone)]
pub struct LocalPaths {
    pub os: String,
    pub home: String,
    pub claude: Option<String>,
    pub codex: Option<String>,
    pub data_dir: String,
}

impl LocalPaths {
    pub fn from_env(env: &KbEnv) -> Self {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        Self {
            os: std::env::consts::OS.into(),
            home: crate::utils::home_dir()
                .map(|h| text(&h))
                .unwrap_or_default(),
            claude: env.roots.claude.as_deref().map(text),
            codex: env.roots.codex.as_deref().map(text),
            data_dir: text(env.data.root()),
        }
    }
}

fn under<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return None;
    }
    let rest = path.strip_prefix(root)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// A path from the exporting machine, as it reads on this one: the Claude and
/// Codex source folders, the data folder, then the home folder. Anything else
/// (a mounted disk, another operating system) stays as it was.
pub fn convert_path(path: &str, from: &SourceDevice, to: &LocalPaths) -> String {
    if from.os != to.os {
        return path.to_string();
    }
    let rules: [(Option<&str>, Option<&str>); 4] = [
        (from.claude_root.as_deref(), to.claude.as_deref()),
        (from.codex_root.as_deref(), to.codex.as_deref()),
        (Some(from.data_dir.as_str()), Some(to.data_dir.as_str())),
        (Some(from.home.as_str()), Some(to.home.as_str())),
    ];
    for (old, new) in rules {
        let (Some(old), Some(new)) = (old, new) else {
            continue;
        };
        if let Some(rest) = under(path, old) {
            return format!("{}{rest}", new.trim_end_matches('/'));
        }
    }
    path.to_string()
}

fn staging_dir(data: &DataPaths, export_id: &str) -> PathBuf {
    let safe: String = export_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    data.root().join(format!(".import-{safe}"))
}

// ---------------------------------------------------------------------------
// Looking at the file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalState {
    /// No sessions, notes or backed-up files here: the import takes over.
    pub empty: bool,
    pub sessions: u64,
    pub notes: u64,
    pub raw_files: u64,
    pub note_files: u64,
}

pub fn local_state(data: &DataPaths) -> LocalState {
    let raw_files = fsutil::walk_files(&data.raw()).len() as u64;
    let note_files = fsutil::walk_files(&data.notes())
        .iter()
        .filter(|(rel, _)| !is_conflict_path(rel))
        .count() as u64;
    let (sessions, notes) = db::open_readonly(&data.db())
        .ok()
        .flatten()
        .map_or((0, 0), |conn| {
            let n = |sql: &str| {
                conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                    .map_or(0, |n| u64::try_from(n).unwrap_or(0))
            };
            (
                n("SELECT COUNT(*) FROM sessions"),
                n("SELECT COUNT(*) FROM notes"),
            )
        });
    LocalState {
        empty: raw_files == 0 && note_files == 0 && sessions == 0,
        sessions,
        notes,
        raw_files,
        note_files,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectReport {
    pub export_id: String,
    pub exported_at: String,
    pub app_version: String,
    pub schema_version: i64,
    pub device_name: String,
    pub device_os: String,
    pub counts: pack::Counts,
    pub uncompressed_bytes: u64,
    pub file_bytes: u64,
    pub settings: PortableSettings,
    pub local: LocalState,
    /// Space the import needs in the data folder's volume.
    pub needed_bytes: u64,
    pub free_bytes: Option<u64>,
    /// `Some(code)` when this program cannot import the file.
    pub problem: Option<String>,
}

fn check_compatible(manifest: &Manifest) -> TResult<()> {
    if manifest.schema_version > db::latest_version() {
        return Err(TransferError::new(
            "too_new",
            format!(
                "the file comes from a newer AgentKB (database v{}); this one supports v{}; \
                 update the app first",
                manifest.schema_version,
                db::latest_version()
            ),
        ));
    }
    Ok(())
}

/// Read the manifest only (the end of the file) and say what importing it
/// would involve here.
pub fn inspect(env: &KbEnv, package: &Path) -> TResult<InspectReport> {
    let manifest = pack::read_manifest(package)?;
    let file_bytes = std::fs::metadata(package).map_or(0, |m| m.len());
    let needed = manifest.totals.uncompressed_bytes / 100 * SPACE_MARGIN_PERCENT;
    Ok(InspectReport {
        export_id: manifest.export_id.clone(),
        exported_at: manifest.exported_at.clone(),
        app_version: manifest.app.version.clone(),
        schema_version: manifest.schema_version,
        device_name: manifest.source_device.name.clone(),
        device_os: manifest.source_device.os.clone(),
        counts: manifest.counts.clone(),
        uncompressed_bytes: manifest.totals.uncompressed_bytes,
        file_bytes,
        settings: manifest.settings.clone(),
        local: local_state(&env.data),
        needed_bytes: needed,
        free_bytes: fsutil::free_space(env.data.root()),
        problem: check_compatible(&manifest)
            .err()
            .map(|e| e.code.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// This machine's settings with the file's applied. Exclusion lists are
/// merged (a union); the rest only when asked for, or when there are none yet.
/// Never taken from the file: data folder, backup disk, the digest's CLI path
/// and on/off switch, the schedule.
pub fn merge_settings(
    config: &KbConfig,
    adopt_all: bool,
    settings: &PortableSettings,
    from: &SourceDevice,
    to: &LocalPaths,
) -> (KbConfig, Vec<String>) {
    let mut merged = config.clone();
    let mut added = Vec::new();
    let mut union = |own: &mut Vec<String>, incoming: &[String], track: bool| {
        for item in incoming {
            let item = convert_path(item, from, to);
            if !own.contains(&item) {
                if track {
                    added.push(item.clone());
                }
                own.push(item);
            }
        }
    };
    union(
        &mut merged.exclude_prefixes,
        &settings.exclude_prefixes,
        true,
    );
    union(
        &mut merged.exclude_cwd_prefixes,
        &settings.exclude_cwd_prefixes,
        true,
    );
    if adopt_all {
        if settings.interval_secs != 0 {
            merged.interval_secs = settings.interval_secs;
        }
        merged.sources.claude.enabled = settings.sources_enabled.claude;
        merged.sources.codex.enabled = settings.sources_enabled.codex;
        let d = &settings.digest;
        if !d.engine.is_empty() {
            merged.digest.engine.clone_from(&d.engine);
            merged.digest.model.clone_from(&d.model);
            merged.digest.time.clone_from(&d.time);
            merged.digest.max_sessions_per_round = d.max_sessions_per_round;
            merged.digest.recent_days = d.recent_days;
            merged.digest.quiet_minutes = d.quiet_minutes;
            merged.digest.min_messages = d.min_messages;
            merged.digest.max_chunks_per_session = d.max_chunks_per_session;
            merged.digest.codex_reasoning.clone_from(&d.codex_reasoning);
        }
    }
    (merged, added)
}

// ---------------------------------------------------------------------------
// Preparing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Nothing here yet: the import takes over the folder.
    Empty,
    /// Data here: merge.
    Merge,
}

#[derive(Debug, Clone, Serialize)]
pub struct Prepared {
    pub export_id: String,
    pub mode: Mode,
    pub device_name: String,
    pub counts: pack::Counts,
    pub summary: Option<merge::Summary>,
    pub added_exclusions: Vec<String>,
    pub local: LocalState,
}

fn db_err(what: &str, e: impl std::fmt::Display) -> TransferError {
    TransferError::io(format!("{what}: {e}"))
}

/// Make the unpacked database right for this machine.
fn prepare_db(staging: &Path, manifest: &Manifest, local: &LocalPaths) -> TResult<()> {
    let db_path = staging.join("kb.db");
    let conn = db::open(&db_path).map_err(|e| {
        if e.contains("newer than this program") {
            TransferError::new("too_new", e)
        } else {
            TransferError::corrupt(format!("the database in the file cannot be opened: {e}"))
        }
    })?;
    let device = &manifest.source_device;
    let from_text = |what: &str, e: rusqlite::Error| db_err(what, e);

    // Where each session's file lives here, and whether it is here at all.
    let rows: Vec<(i64, String, Option<String>, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, source_path, origin, source_status FROM files")
            .map_err(|e| from_text("read files", e))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| from_text("read files", e))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| from_text("read files", e))?
    };
    {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| from_text("begin", e))?;
        for (id, path, origin, status) in rows {
            let converted = convert_path(&path, device, local);
            let origin = if status == "present" && !Path::new(&converted).exists() {
                origin.or_else(|| Some(device.name.clone()))
            } else if status == "present" {
                None
            } else {
                origin
            };
            tx.execute(
                "UPDATE files SET source_path = ?2, origin = ?3 WHERE id = ?1",
                rusqlite::params![id, converted, origin],
            )
            .map_err(|e| {
                TransferError::corrupt(format!("two sessions end up with the same path: {e}"))
            })?;
        }
        let cwds: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT cwd FROM sessions WHERE cwd IS NOT NULL")
                .map_err(|e| from_text("read sessions", e))?;
            let rows = stmt
                .query_map([], |r| r.get(0))
                .map_err(|e| from_text("read sessions", e))?;
            rows.collect::<Result<_, _>>()
                .map_err(|e| from_text("read sessions", e))?
        };
        for cwd in cwds {
            let converted = convert_path(&cwd, device, local);
            if converted != cwd {
                tx.execute(
                    "UPDATE sessions SET cwd = ?2 WHERE cwd = ?1",
                    rusqlite::params![cwd, converted],
                )
                .map_err(|e| from_text("update sessions", e))?;
            }
        }
        tx.commit().map_err(|e| from_text("commit", e))?;
    }

    // The index was built under the exporter's exclusion list; say so in
    // this machine's terms, so a matching list needs no rebuild.
    if let Some(applied) = db::get_meta(&conn, EXCLUDE_CWD_KEY)? {
        let converted: Vec<String> = applied
            .split('\n')
            .map(|p| convert_path(p, device, local))
            .collect();
        db::set_meta(&conn, EXCLUDE_CWD_KEY, &converted.join("\n"))?;
    }
    db::delete_meta(&conn, "digest_first_round_seen")?;

    // Notes are read from their files, which are the source of truth.
    notes::rebuild_all(&conn, &DataPaths::new(staging))?;

    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| from_text("integrity_check", e))?;
    if check != "ok" {
        return Err(TransferError::corrupt(format!(
            "the database in the file is damaged: {check}"
        )));
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;")
        .map_err(|e| from_text("finish the database", e))?;
    conn.close()
        .map_err(|(_, e)| from_text("close the database", e))?;
    // The migration's safety copy and WAL leftovers are not part of the result.
    if let Ok(entries) = std::fs::read_dir(staging) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("kb.db.") || name == "kb.db-wal" || name == "kb.db-shm" {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

fn staged_file_list(manifest: &Manifest) -> Vec<(String, u64)> {
    manifest
        .files
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect()
}

fn plan_for(
    env: &KbEnv,
    staging: &Path,
    manifest: &Manifest,
    local: &LocalState,
) -> TResult<(Plan, Vec<FileRow>, Vec<ImportedFile>)> {
    let incoming = match db::open_readonly(&staging.join("kb.db"))? {
        Some(conn) => db::load_imported_files(&conn)?,
        None => Vec::new(),
    };
    let local_rows = match db::open_readonly(&env.data.db())? {
        Some(conn) => db::load_files(&conn)?,
        None => Vec::new(),
    };
    let plan = merge::plan(
        &env.data,
        staging,
        &staged_file_list(manifest),
        &incoming,
        &local_rows,
        &LocalCounts {
            sessions: local.sessions,
            notes: local.notes,
            raw_files: local.raw_files,
            note_files: local.note_files,
        },
        &manifest.export_id,
    )?;
    Ok((plan, local_rows, incoming))
}

pub struct Options {
    /// Take the file's interval, source switches and digest parameters too
    /// (always true when there is no configuration yet).
    pub adopt_settings: bool,
    /// Write `config.json`. The wizard leaves this off and prefills its form.
    pub write_config: bool,
    /// There is a `config.json` already.
    pub config_exists: bool,
}

/// Unpack and verify. Changes nothing outside the work folder.
pub fn prepare(
    env: &KbEnv,
    config: &KbConfig,
    opts: &Options,
    package: &Path,
    cancel: &Cancel,
    progress: &ProgressSink,
) -> TResult<Prepared> {
    let manifest = pack::read_manifest(package)?;
    check_compatible(&manifest)?;
    let data = &env.data;
    std::fs::create_dir_all(data.root())
        .map_err(|e| db_err(&format!("cannot create {}", data.root().display()), e))?;
    clean_stale_work_dirs(data.root());
    let needed = manifest.totals.uncompressed_bytes / 100 * SPACE_MARGIN_PERCENT;
    if let Some(free) = fsutil::free_space(data.root()) {
        if free < needed {
            return Err(TransferError::new(
                "no_space",
                format!("the import needs about {needed} bytes in the data folder's disk and only {free} are free"),
            ));
        }
    }
    let staging = staging_dir(data, &manifest.export_id);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|e| db_err(&format!("cannot clear {}", staging.display()), e))?;
    }
    std::fs::create_dir_all(&staging)
        .map_err(|e| db_err(&format!("cannot create {}", staging.display()), e))?;
    let guard = RemoveOnDrop(staging.clone());

    let mut meter = Meter::new(progress, "import");
    meter.phase(
        "extract",
        manifest.files.len() as u64,
        manifest.totals.uncompressed_bytes,
    );
    pack::extract(package, &manifest, &staging, cancel, &mut |_, n| {
        meter.add_bytes(n);
    })?;
    meter.phase("database", 0, 0);

    let local_paths = LocalPaths::from_env(env);
    prepare_db(&staging, &manifest, &local_paths)?;

    let local = local_state(data);
    let mode = if local.empty {
        Mode::Empty
    } else {
        Mode::Merge
    };
    let summary = if mode == Mode::Merge {
        Some(plan_for(env, &staging, &manifest, &local)?.0.summary)
    } else {
        None
    };
    let (_, added) = merge_settings(
        config,
        opts.adopt_settings || !opts.config_exists,
        &manifest.settings,
        &manifest.source_device,
        &local_paths,
    );
    pack_marker(&staging, &manifest)?;
    guard.keep();
    Ok(Prepared {
        export_id: manifest.export_id.clone(),
        mode,
        device_name: manifest.source_device.name.clone(),
        counts: manifest.counts.clone(),
        summary,
        added_exclusions: added,
        local,
    })
}

fn pack_marker(staging: &Path, manifest: &Manifest) -> TResult<()> {
    fsutil::write_atomic(&staging.join(STAGED_MARKER), manifest.export_id.as_bytes())
        .map_err(TransferError::io)
}

/// Throw away what [`prepare`] left (the person said no).
pub fn discard(env: &KbEnv, package: &Path) -> TResult<()> {
    let manifest = pack::read_manifest(package)?;
    let dir = staging_dir(&env.data, &manifest.export_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| db_err(&format!("cannot remove {}", dir.display()), e))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Committing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub expected: u64,
    pub actual: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub mode: Mode,
    pub export_id: String,
    pub device_name: String,
    pub exported_at: String,
    pub summary: Option<merge::Summary>,
    /// The result against the file's own numbers (or, merging, against the
    /// numbers before plus what the plan announced). A mismatch is reported,
    /// not an error: the import is done and verified file by file.
    pub checks: Vec<Check>,
    pub all_match: bool,
    pub backup_db: Option<String>,
    pub backup_config: Option<String>,
    pub added_exclusions: Vec<String>,
    pub config: KbConfig,
    pub config_written: bool,
    pub warnings: Vec<String>,
    pub duration_ms: u64,
}

// Test seam: pretend the process died at a named step.
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

fn remove_empty_tree(path: &Path) {
    if path.is_dir() && fsutil::walk_files(path).is_empty() {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn rename_err(what: &str, e: std::io::Error) -> TransferError {
    TransferError::io(format!("cannot move {what} into place: {e}"))
}

/// Switch an empty library over to the unpacked one. Every step is one
/// `rename`; the database goes last, so a crash in between leaves files and
/// no database, which the next import completes.
fn apply_empty(env: &KbEnv, staging: &Path, stamp: &str, report: &mut ImportReport) -> TResult<()> {
    let data = &env.data;
    remove_empty_tree(&data.raw());
    remove_empty_tree(&data.notes());
    if data.raw().exists() || data.notes().exists() {
        return Err(TransferError::new(
            "not_empty",
            "the data folder is not empty any more",
        ));
    }
    if data.db().is_file() {
        let backup = data.root().join(format!("kb.db.before-import-{stamp}"));
        std::fs::rename(data.db(), &backup).map_err(|e| rename_err("the old kb.db", e))?;
        for side in ["-wal", "-shm"] {
            let from = data.root().join(format!("kb.db{side}"));
            if from.exists() {
                let _ = std::fs::rename(
                    &from,
                    data.root()
                        .join(format!("kb.db.before-import-{stamp}{side}")),
                );
            }
        }
        report.backup_db = Some(backup.display().to_string());
    }
    let staged_notes = staging.join("notes");
    if !staged_notes.exists() {
        std::fs::create_dir_all(&staged_notes).map_err(|e| db_err("cannot create notes", e))?;
    }
    let staged_raw = staging.join("raw");
    if !staged_raw.exists() {
        std::fs::create_dir_all(&staged_raw).map_err(|e| db_err("cannot create raw", e))?;
    }
    std::fs::rename(&staged_raw, data.raw()).map_err(|e| rename_err("raw/", e))?;
    step("after_raw")?;
    std::fs::rename(&staged_notes, data.notes()).map_err(|e| rename_err("notes/", e))?;
    step("after_notes")?;
    std::fs::rename(staging.join("kb.db"), data.db()).map_err(|e| rename_err("kb.db", e))?;
    fsutil::fsync_dir(data.root());
    Ok(())
}

fn count_checks_empty(manifest: &Manifest, data: &DataPaths) -> Vec<Check> {
    let actual = local_state(data);
    let with_notes = db::open_readonly(&data.db())
        .ok()
        .flatten()
        .map_or(0, |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM (SELECT DISTINCT source, session_id FROM notes)",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_or(0, |n| u64::try_from(n).unwrap_or(0))
        });
    vec![
        Check {
            name: "sessions",
            expected: manifest.counts.sessions,
            actual: actual.sessions,
        },
        Check {
            name: "notes",
            expected: manifest.counts.notes,
            actual: actual.notes,
        },
        Check {
            name: "sessions_with_notes",
            expected: manifest.counts.sessions_with_notes,
            actual: with_notes,
        },
        Check {
            name: "raw_files",
            expected: manifest.counts.raw_files,
            actual: actual.raw_files,
        },
        Check {
            name: "note_files",
            expected: manifest.counts.note_files,
            actual: actual.note_files,
        },
    ]
}

fn count_checks_merge(summary: &merge::Summary, data: &DataPaths) -> Vec<Check> {
    let actual = local_state(data);
    vec![
        Check {
            name: "sessions",
            expected: summary.expected_sessions,
            actual: actual.sessions,
        },
        Check {
            name: "notes",
            expected: summary.expected_notes,
            actual: actual.notes,
        },
        Check {
            name: "raw_files",
            expected: summary.expected_raw_files,
            actual: actual.raw_files,
        },
        Check {
            name: "note_files",
            expected: summary.expected_note_files,
            actual: actual.note_files,
        },
    ]
}

fn imported_cursor(row: &ImportedFile, len: u64) -> Cursor {
    Cursor {
        src_size: row.src_size,
        src_mtime_ns: row.src_mtime_ns,
        head_hash: row.head_hash.clone(),
        head_len: row.head_len,
        mirror_offset: i64::try_from(len).unwrap_or(i64::MAX),
    }
}

/// The database side of a merge, in one transaction. Returns the ids of the
/// files whose messages must be indexed.
fn apply_merge_db(
    conn: &Connection,
    plan: &Plan,
    incoming: &[ImportedFile],
    local_rows: &[FileRow],
    device: &str,
    tag: &str,
) -> TResult<MergedRows> {
    let by_rel: HashMap<&str, &FileRow> = local_rows
        .iter()
        .map(|r| (r.mirror_rel.as_str(), r))
        .collect();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| db_err("begin", e))?;
    // Until the new files are indexed the index is declared out of date, in
    // the same transaction as the rows, so a crash before the indexing is
    // finished leaves a database the next collection rebuilds.
    let previous = db::get_meta(&tx, INDEXER_VERSION_KEY)?;
    let marked = previous.as_deref() != Some(INDEXING);
    if marked {
        db::set_meta(&tx, INDEXER_VERSION_KEY, INDEXING)?;
    }
    let mut touched = Vec::new();
    // In the order the exporter's database had them, so equal scores sort the
    // same way afterwards.
    let mut ordered: Vec<&merge::FilePlan> =
        plan.files.iter().filter(|f| f.row.is_some()).collect();
    ordered.sort_by_key(|f| f.row);
    for item in ordered {
        let Some(index) = item.row else { continue };
        let row = &incoming[index];
        match item.action {
            Action::Extend { .. } => {
                if let Some(local) = by_rel.get(item.rel.as_str()) {
                    db::set_merged_cursor(&tx, local.id, &imported_cursor(row, item.len))?;
                    touched.push(local.id);
                }
            }
            _ if item.insert_row => {
                let mut row = row.clone();
                row.mirror_rel.clone_from(&item.target_rel);
                row.mirror_offset = i64::try_from(item.len).unwrap_or(i64::MAX);
                if matches!(item.action, Action::KeepBoth { .. }) {
                    row.source_path = merge::imported_name(&row.source_path, tag);
                    row.origin = Some(device.to_string());
                }
                touched.push(db::insert_imported_file(&tx, &row)?);
            }
            _ => {}
        }
    }
    tx.commit().map_err(|e| db_err("commit", e))?;
    Ok(MergedRows {
        touched,
        restore_version: marked.then_some(previous).flatten(),
    })
}

struct MergedRows {
    touched: Vec<i64>,
    /// The indexer version to put back once the new files are indexed.
    restore_version: Option<String>,
}

/// Apply a prepared import. `config_file` is where `config.json` lives.
#[allow(clippy::too_many_lines)]
pub fn commit(
    env: &KbEnv,
    config: &KbConfig,
    opts: &Options,
    config_file: &Path,
    package: &Path,
    cancel: &Cancel,
    progress: &ProgressSink,
) -> TResult<ImportReport> {
    let started = Instant::now();
    let manifest = pack::read_manifest(package)?;
    check_compatible(&manifest)?;
    let data = &env.data;
    let staging = staging_dir(data, &manifest.export_id);
    let marker = std::fs::read_to_string(staging.join(STAGED_MARKER)).unwrap_or_default();
    if marker != manifest.export_id {
        return Err(TransferError::new(
            "not_prepared",
            "the file was not unpacked yet (or the unpacked copy is gone); start the import again",
        ));
    }
    let _staging_guard = RemoveOnDrop(staging.clone());

    if let Some(info) = lock::held_by_live_process(&data.digest_lock()) {
        return Err(TransferError::new(
            "digest_running",
            format!(
                "a digest is running (since {}); import after it has finished",
                info.started_at
            ),
        ));
    }
    let _sync = SyncLock::acquire(&data.lock(), LOCK_WAIT)?.ok_or_else(|| {
        TransferError::new(
            "busy",
            "a collection is still running; try again in a moment",
        )
    })?;
    let _digest = SyncLock::acquire(&data.digest_lock(), Duration::ZERO)?
        .ok_or_else(|| TransferError::new("digest_running", "a digest is running"))?;
    cancel.check()?;

    let local_paths = LocalPaths::from_env(env);
    let (merged, added_exclusions) = merge_settings(
        config,
        opts.adopt_settings || !opts.config_exists,
        &manifest.settings,
        &manifest.source_device,
        &local_paths,
    );
    // Everything that can be refused is refused before anything changes.
    merged
        .validate()
        .map_err(|e| TransferError::new("settings", e))?;

    let stamp = now_stamp();
    let local = local_state(data);
    let mode = if local.empty {
        Mode::Empty
    } else {
        Mode::Merge
    };
    let mut report = ImportReport {
        mode,
        export_id: manifest.export_id.clone(),
        device_name: manifest.source_device.name.clone(),
        exported_at: manifest.exported_at.clone(),
        summary: None,
        checks: Vec::new(),
        all_match: false,
        backup_db: None,
        backup_config: None,
        added_exclusions,
        config: merged.clone(),
        config_written: false,
        warnings: Vec::new(),
        duration_ms: 0,
    };
    let mut meter = Meter::new(progress, "import");

    match mode {
        Mode::Empty => {
            meter.phase("switch", 3, 0);
            apply_empty(env, &staging, &stamp, &mut report)?;
            report.checks = count_checks_empty(&manifest, data);
        }
        Mode::Merge => {
            let (plan, local_rows, incoming) = plan_for(env, &staging, &manifest, &local)?;
            if data.db().is_file() {
                let backup = data.root().join(format!("kb.db.before-import-{stamp}"));
                let conn = db::open(&data.db())?;
                online_backup(&conn, &backup)?;
                report.backup_db = Some(backup.display().to_string());
            }
            let total: u64 = plan.files.iter().map(|f| f.len).sum();
            meter.phase("merge", plan.files.len() as u64, total);
            merge::apply_files(&plan, data, &staging, cancel, &mut |n| {
                meter.add_bytes(n);
                meter.file_done();
            })?;
            step("after_files")?;
            merge::apply_notes(&plan, data, &staging, &manifest.export_id)?;
            let conn = db::open(&data.db())?;
            let tag = merge::tag_of(&manifest.export_id);
            let merged_rows = apply_merge_db(
                &conn,
                &plan,
                &incoming,
                &local_rows,
                &manifest.source_device.name,
                &tag,
            )?;
            step("after_db")?;
            let touched = &merged_rows.touched;
            meter.phase("index", touched.len() as u64, 0);
            let mut index_env = env.clone();
            index_env.exclude_cwd_prefixes = KbEnv::from_config(&merged).exclude_cwd_prefixes;
            let rows = db::load_files(&conn)?;
            let mut index_failed = false;
            for row in rows.iter().filter(|r| touched.contains(&r.id)) {
                cancel.check()?;
                if let Err(message) = sync::reindex_one(&index_env, &conn, row) {
                    index_failed = true;
                    report
                        .warnings
                        .push(format!("could not index {}: {message}", row.mirror_rel));
                }
                meter.file_done();
            }
            if let (false, Some(version)) = (index_failed, &merged_rows.restore_version) {
                db::set_meta(&conn, INDEXER_VERSION_KEY, version)?;
            }
            match notes::reconcile(&conn, data) {
                Ok(rebuilt) => {
                    for (path, reason) in rebuilt.skipped {
                        report.warnings.push(format!("note file {path}: {reason}"));
                    }
                }
                Err(message) => report.warnings.push(format!("notes: {message}")),
            }
            drop(conn);
            report.checks = count_checks_merge(&plan.summary, data);
            report.summary = Some(plan.summary);
        }
    }
    report.all_match = report.checks.iter().all(|c| c.expected == c.actual);

    // Settings last: a failure above leaves the configuration as it was.
    if opts.write_config {
        if config_file.is_file() {
            let backup = config_file.with_file_name(format!("config.json.before-import-{stamp}"));
            std::fs::copy(config_file, &backup)
                .map_err(|e| db_err("cannot back up config.json", e))?;
            report.backup_config = Some(backup.display().to_string());
        }
        merged
            .save_to(config_file)
            .map_err(|e| TransferError::new("settings", e))?;
        report.config_written = true;
    }
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(report)
}

/// Both calls in a row (the command line).
pub fn import(
    env: &KbEnv,
    config: &KbConfig,
    opts: &Options,
    config_file: &Path,
    package: &Path,
    cancel: &Cancel,
    progress: &ProgressSink,
) -> TResult<ImportReport> {
    prepare(env, config, opts, package, cancel, progress)?;
    commit(env, config, opts, config_file, package, cancel, progress)
}
