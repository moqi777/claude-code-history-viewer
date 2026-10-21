//! The second copy on an external disk.
//!
//! Runs inside the sync lock right after a collection (or on demand) and only
//! ever *adds* to the backup: nothing is deleted there, nothing is overwritten.
//! The destination file's length is the resume cursor, so an interrupted copy
//! (app quit, disk pulled) simply continues next time.
//!
//! A disk that is not connected is a normal, silent outcome. Because
//! `/Volumes/<name>` can exist as a plain folder on the system disk, presence
//! is decided by volume UUID (see [`super::volume`]) and re-checked by device
//! number before every file.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::Serialize;

use super::backup_dir;
use super::db;
use super::fsutil;
use super::log::SyncLog;
use super::mirror::HEAD_BYTES;
use super::progress::Progress;
#[cfg(test)]
use super::progress::ProgressSink;
use super::sync::{KbEnv, SyncError};
use super::volume::{self, Presence, SystemQuery};

const SNAPSHOT_NAME: &str = "kb.db";
const SNAPSHOT_PREV_NAME: &str = "kb.db.prev";

pub const META_LAST_AT: &str = "backup_last_at";
pub const META_LAST_RESULT: &str = "backup_last_result";
pub const META_LAST_BYTES: &str = "backup_last_bytes";
pub const META_LAST_SKIPPED_AT: &str = "backup_last_skipped_at";
pub const META_STATE: &str = "backup_state";
pub const META_PROGRESS: &str = "backup_progress";
pub const META_FOLDER: &str = "backup_folder";
pub const META_OLD_LAYOUT: &str = "backup_old_layout";
pub const META_SNAPSHOT_DAY: &str = "backup_last_db_snapshot_day";

const PROGRESS_META_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupSummary {
    /// `ok`, `skipped` (disabled / not connected) or `error`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub copied_files: u64,
    pub appended_files: u64,
    pub copied_bytes: u64,
    /// Older backup copies moved to `*.conflict-<ts>` because the main
    /// directory rewrote the file.
    pub set_aside_files: u64,
    /// Plain copies removed from the disk because the `.zst` next to them
    /// holds all of their content, and the bytes that freed.
    pub duplicates_removed: u64,
    pub duplicate_bytes_freed: u64,
    pub snapshot_taken: bool,
    /// This computer's folder on the disk.
    pub folder: Option<String>,
    /// A backup in the old layout is still in the configured folder.
    pub old_backup_left: bool,
    /// An old-layout backup of this computer was moved into its folder.
    pub moved_old_backup: bool,
    pub errors: Vec<SyncError>,
    pub duration_ms: u64,
}

impl BackupSummary {
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: "skipped".into(),
            reason: Some(reason.into()),
            ..Self::default()
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Destination missing: copy the whole file.
    Copy,
    /// Destination is an exact prefix: append the rest, starting at `from`.
    Append { from: u64 },
    /// Destination disagrees with the source (the main directory rewrote the
    /// file): keep the old one as `.conflict-*`, copy the new one whole.
    SetAside,
}

struct Planned {
    rel: String,
    size: u64,
    action: Action,
}

fn same_head(src: &Path, dst: &Path, len: u64) -> std::io::Result<bool> {
    let read = |path: &Path| -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        File::open(path)?.take(len).read_to_end(&mut buf)?;
        Ok(buf)
    };
    Ok(read(src)? == read(dst)?)
}

fn plan_file(src: &Path, dst: &Path, size: u64) -> Result<Option<Action>, String> {
    let meta = match fs::metadata(dst) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(Action::Copy)),
        Err(e) => return Err(format!("cannot stat {}: {e}", dst.display())),
    };
    let have = meta.len();
    let head = have.min(size).min(HEAD_BYTES);
    let agree =
        same_head(src, dst, head).map_err(|e| format!("cannot compare {}: {e}", dst.display()))?;
    Ok(match (agree, have.cmp(&size)) {
        (true, std::cmp::Ordering::Equal) => None,
        (true, std::cmp::Ordering::Less) => Some(Action::Append { from: have }),
        _ => Some(Action::SetAside),
    })
}

fn set_aside(dst: &Path) -> Result<PathBuf, String> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut candidate = dst.with_file_name(format!("{name}.conflict-{stamp}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = dst.with_file_name(format!("{name}.conflict-{stamp}-{n}"));
        n += 1;
    }
    fs::rename(dst, &candidate).map_err(|e| format!("cannot move {} aside: {e}", dst.display()))?;
    Ok(candidate)
}

struct Meter<'a> {
    env: &'a KbEnv,
    conn: &'a Connection,
    total_files: u64,
    total_bytes: u64,
    done_files: u64,
    done_bytes: u64,
    last_meta: Instant,
}

impl Meter<'_> {
    fn tick(&mut self, force: bool) {
        self.env.progress.emit(&Progress {
            op: "backup".into(),
            phase: "copy".into(),
            done: self.done_files,
            total: self.total_files,
            bytes_done: self.done_bytes,
            bytes_total: self.total_bytes,
        });
        if force || self.last_meta.elapsed() >= PROGRESS_META_EVERY {
            self.last_meta = Instant::now();
            let _ = db::set_meta(
                self.conn,
                META_PROGRESS,
                &format!("{}/{}", self.done_bytes, self.total_bytes),
            );
        }
    }
}

/// Run one backup pass if a disk is configured. Never returns `Err`: every
/// problem is reported in the summary so the collection that triggered it keeps
/// its own result.
pub fn run(
    env: &KbEnv,
    conn: &Connection,
    log: &SyncLog,
    query: &dyn SystemQuery,
) -> BackupSummary {
    let started = Instant::now();
    let Some(target) = &env.mirror else {
        return BackupSummary::skipped("disabled");
    };
    let mount = match volume::presence(&target.target_dir, &target.volume_uuid, query) {
        Presence::Connected { volume } => volume.mount_point,
        Presence::NotConnected { reason } => {
            log.line(&format!("backup skipped: volume not connected ({reason})"));
            let _ = db::set_meta(conn, META_LAST_SKIPPED_AT, &now());
            let _ = db::set_meta(conn, META_LAST_RESULT, "skipped");
            return BackupSummary::skipped("not_connected");
        }
        Presence::Unsupported { reason } => {
            let message = format!("the configured backup location is not supported: {reason:?}");
            log.line(&format!("backup error: {message}"));
            let _ = db::set_meta(conn, META_LAST_RESULT, &format!("error:{message}"));
            return BackupSummary {
                status: "error".into(),
                errors: vec![backup_error(&target.target_dir, message)],
                ..BackupSummary::default()
            };
        }
    };

    // This computer's folder inside the configured one; never anybody else's.
    let (who, resolved) = match own_folder(env, target, &mount, log) {
        Ok(found) => found,
        Err(message) => {
            log.line(&format!("backup error: {message}"));
            let _ = db::set_meta(conn, META_LAST_RESULT, &format!("error:{message}"));
            return BackupSummary {
                status: "error".into(),
                errors: vec![backup_error(&target.target_dir, message)],
                ..BackupSummary::default()
            };
        }
    };
    let dir = resolved.dir.clone();
    let _ = db::set_meta(conn, META_FOLDER, &dir.display().to_string());
    let _ = db::set_meta(
        conn,
        META_OLD_LAYOUT,
        if resolved.old_backup_left { "1" } else { "0" },
    );

    let _ = db::set_meta(conn, META_STATE, "running");
    let _ = db::set_meta(conn, META_PROGRESS, "0/0");
    log.line(&format!("backup start -> {}", dir.display()));
    let mut summary = BackupSummary {
        folder: Some(dir.display().to_string()),
        old_backup_left: resolved.old_backup_left,
        moved_old_backup: resolved.moved_old_backup,
        ..BackupSummary::default()
    };
    if let Err(message) = copy_all(env, conn, log, &dir, &mount, &mut summary) {
        summary.errors.push(backup_error(&dir, message));
    }
    match snapshot_db(env, conn, &dir, &mount) {
        Ok(taken) => summary.snapshot_taken = taken,
        Err(message) => summary
            .errors
            .push(backup_error(&dir.join(SNAPSHOT_NAME), message)),
    }
    if summary.errors.is_empty() {
        let sessions = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .map_or(0, |n| u64::try_from(n).unwrap_or(0));
        if let Err(message) = backup_dir::touch(&dir, &who, sessions) {
            summary.errors.push(backup_error(&dir, message));
        }
    }

    summary.status = if summary.errors.is_empty() {
        "ok"
    } else {
        "error"
    }
    .into();
    summary.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let result = if summary.errors.is_empty() {
        let _ = db::set_meta(conn, META_LAST_AT, &now());
        "ok".to_string()
    } else {
        format!("error:{}", summary.errors[0].message)
    };
    let _ = db::set_meta(conn, META_LAST_RESULT, &result);
    let _ = db::set_meta(conn, META_LAST_BYTES, &summary.copied_bytes.to_string());
    let _ = db::set_meta(conn, META_STATE, "idle");
    log.line(&format!(
        "backup end status={} files={} appended={} bytes={} set_aside={} snapshot={} errors={}",
        summary.status,
        summary.copied_files,
        summary.appended_files,
        summary.copied_bytes,
        summary.set_aside_files,
        summary.snapshot_taken,
        summary.errors.len()
    ));
    summary
}

/// Who this computer is and which folder of the disk is its own.
fn own_folder(
    env: &KbEnv,
    target: &super::sync::MirrorTarget,
    mount: &Path,
    log: &SyncLog,
) -> Result<(backup_dir::Identity, backup_dir::Resolved), String> {
    // A disk's root is never the backup folder itself (a setting saved before 0.7.6).
    let root = backup_dir::effective_root(&target.target_dir, mount);
    ensure_on_disk(&root, mount)?;
    let who = backup_dir::identity(&target.id_file)?;
    for moved in backup_dir::adopt_strays(&root, &who) {
        log.line(&format!("moved own backup folder to {}", moved.display()));
    }
    let resolved = backup_dir::resolve(&root, &who, &env.roots, env.data.root(), log)?;
    Ok((who, resolved))
}

fn backup_error(path: &Path, message: String) -> SyncError {
    SyncError {
        path: path.display().to_string(),
        stage: "backup".into(),
        message,
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn ensure_on_disk(path: &Path, mount: &Path) -> Result<(), String> {
    let anchor =
        fsutil::nearest_existing(path).ok_or_else(|| format!("{} vanished", path.display()))?;
    if volume::same_device(&anchor, mount) {
        Ok(())
    } else {
        Err(format!(
            "the backup disk went away: {} is no longer on {}",
            path.display(),
            mount.display()
        ))
    }
}

fn copy_all(
    env: &KbEnv,
    conn: &Connection,
    log: &SyncLog,
    target_dir: &Path,
    mount: &Path,
    summary: &mut BackupSummary,
) -> Result<(), String> {
    let src_root = env.data.raw();
    let dst_root = target_dir.join("raw");

    // Plan first, so progress has a denominator and a pulled disk is noticed
    // before anything is half-done.
    let mut plan: Vec<Planned> = Vec::new();
    for (rel, size) in fsutil::walk_files(&src_root) {
        let dst = dst_root.join(&rel);
        match plan_file(&src_root.join(&rel), &dst, size) {
            Ok(Some(action)) => plan.push(Planned { rel, size, action }),
            Ok(None) => {}
            Err(message) => summary.errors.push(backup_error(&dst, message)),
        }
    }
    let bytes_of = |p: &Planned| match p.action {
        Action::Append { from } => p.size - from,
        Action::Copy | Action::SetAside => p.size,
    };
    let mut meter = Meter {
        env,
        conn,
        total_files: plan.len() as u64,
        total_bytes: plan.iter().map(bytes_of).sum(),
        done_files: 0,
        done_bytes: 0,
        last_meta: Instant::now(),
    };
    meter.tick(true);

    for item in &plan {
        let src = src_root.join(&item.rel);
        let dst = dst_root.join(&item.rel);
        ensure_on_disk(&dst, mount)?;
        // (bytes copied, whole file copied?) — counted only after success.
        let result = match item.action {
            Action::Copy => fsutil::copy_atomic(&src, &dst).map(|n| (n, true)),
            Action::Append { from } => fsutil::append_tail(&src, &dst, from).map(|n| (n, false)),
            Action::SetAside => set_aside(&dst).and_then(|aside| {
                log.line(&format!(
                    "backup: {} was rewritten in the main directory; the older backup copy is kept as {}",
                    item.rel,
                    aside.display()
                ));
                summary.set_aside_files += 1;
                fsutil::copy_atomic(&src, &dst).map(|n| (n, true))
            }),
        };
        match result {
            Ok((n, whole)) => {
                if whole {
                    summary.copied_files += 1;
                } else {
                    summary.appended_files += 1;
                }
                summary.copied_bytes += n;
                meter.done_bytes += bytes_of(item);
            }
            Err(message) => {
                log.line(&format!("ERROR backup {}: {message}", item.rel));
                summary.errors.push(backup_error(&dst, message));
                // A pulled disk fails every later file the same way; stop.
                if ensure_on_disk(&dst, mount).is_err() {
                    break;
                }
            }
        }
        meter.done_files += 1;
        meter.tick(false);
    }
    meter.tick(true);
    copy_notes(env, log, target_dir, mount, summary);
    // Only when every file went over: a twin that failed to copy proves nothing.
    if summary.errors.is_empty() {
        let removed = super::dedupe::clean_disk(&env.data, &dst_root, log, |path| {
            ensure_on_disk(path, mount)
        });
        summary.duplicates_removed += removed.files;
        summary.duplicate_bytes_freed += removed.bytes;
        if let Err(message) = super::dedupe::record(conn, removed) {
            log.line(&format!(
                "duplicate backups: cannot record the count: {message}"
            ));
        }
    }
    Ok(())
}

/// The note files, which exist nowhere else: an AI wrote them and writing
/// them again costs quota. Unlike session logs they are rewritten whole, so a
/// changed file simply replaces its copy (the copy is made through a temp
/// file, so the old one stays intact until the new one is complete). Nothing
/// is ever removed from the backup.
fn copy_notes(
    env: &KbEnv,
    log: &SyncLog,
    target_dir: &Path,
    mount: &Path,
    summary: &mut BackupSummary,
) {
    let src_root = env.data.notes();
    let dst_root = target_dir.join("notes");
    for (rel, _) in fsutil::walk_files(&src_root) {
        let (src, dst) = (src_root.join(&rel), dst_root.join(&rel));
        if matches!((fsutil::sha256_file(&src), fsutil::sha256_file(&dst)), (Ok(a), Ok(b)) if a == b)
        {
            continue;
        }
        if let Err(message) = ensure_on_disk(&dst, mount) {
            summary.errors.push(backup_error(&dst, message));
            return;
        }
        match fsutil::copy_atomic(&src, &dst) {
            Ok(n) => {
                summary.copied_files += 1;
                summary.copied_bytes += n;
            }
            Err(message) => {
                log.line(&format!("ERROR backup notes/{rel}: {message}"));
                summary.errors.push(backup_error(&dst, message));
            }
        }
    }
}

/// Consistent copy of `kb.db` via the `SQLite` online backup, at most once a day,
/// keeping exactly one previous generation (`kb.db.prev`). Returns whether a
/// snapshot was taken.
fn snapshot_db(
    env: &KbEnv,
    conn: &Connection,
    target_dir: &Path,
    mount: &Path,
) -> Result<bool, String> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if db::get_meta(conn, META_SNAPSHOT_DAY)?.as_deref() == Some(today.as_str())
        && target_dir.join(SNAPSHOT_NAME).is_file()
    {
        return Ok(false);
    }
    if !env.data.db().is_file() {
        return Ok(false);
    }
    ensure_on_disk(target_dir, mount)?;
    fs::create_dir_all(target_dir)
        .map_err(|e| format!("cannot create {}: {e}", target_dir.display()))?;
    // SQLite never opens a file on the disk: the snapshot is made and checked in
    // the data directory, then copied whole (some exFAT mounts refuse the
    // shared-memory file WAL needs).
    let local = env
        .data
        .root()
        .join(format!("{SNAPSHOT_NAME}.snapshot-{}", std::process::id()));
    let staged = target_dir.join(format!("{SNAPSHOT_NAME}.new-{}", std::process::id()));
    let result = place_snapshot(conn, &local, &staged, target_dir, |from, to| {
        fsutil::copy_atomic(from, to).map(|_| ())
    });
    let _ = fs::remove_file(&local);
    let _ = fs::remove_file(&staged);
    result?;
    db::set_meta(conn, META_SNAPSHOT_DAY, &today)?;
    Ok(true)
}

/// Make the snapshot at `local`, check it, copy it to `staged` on the disk,
/// compare the two by SHA-256 and only then rotate `kb.db` to `kb.db.prev` and
/// move the new one in. Any failure leaves the disk's snapshots as they were.
fn place_snapshot(
    conn: &Connection,
    local: &Path,
    staged: &Path,
    target_dir: &Path,
    copy: impl FnOnce(&Path, &Path) -> Result<(), String>,
) -> Result<(), String> {
    let _ = fs::remove_file(local);
    online_backup(conn, local)?;
    let check = Connection::open(local).map_err(|e| format!("cannot open the snapshot: {e}"))?;
    let verdict: String = check
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| format!("integrity check failed to run: {e}"))?;
    if verdict != "ok" {
        return Err(format!("snapshot failed its integrity check: {verdict}"));
    }
    drop(check);
    copy(local, staged)?;
    let (want, got) = (fsutil::sha256_file(local), fsutil::sha256_file(staged));
    match (want, got) {
        (Ok(a), Ok(b)) if a == b => {}
        (Ok(_), Ok(_)) => return Err("the snapshot on the disk differs from the original".into()),
        (Err(e), _) | (_, Err(e)) => return Err(format!("cannot verify the snapshot copy: {e}")),
    }
    let live = target_dir.join(SNAPSHOT_NAME);
    if live.exists() {
        fs::rename(&live, target_dir.join(SNAPSHOT_PREV_NAME))
            .map_err(|e| format!("cannot rotate the previous snapshot: {e}"))?;
    }
    fs::rename(staged, &live).map_err(|e| format!("cannot place the snapshot: {e}"))
}

/// Write a consistent copy of the live database to `dest` (WAL included).
pub fn online_backup(conn: &Connection, dest: &Path) -> Result<(), String> {
    let mut out =
        Connection::open(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let backup = rusqlite::backup::Backup::new(conn, &mut out)
        .map_err(|e| format!("cannot start the database backup: {e}"))?;
    backup
        .run_to_completion(256, Duration::from_millis(5), None)
        .map_err(|e| format!("database backup failed: {e}"))?;
    drop(backup);
    // The copy inherits WAL mode; a single self-contained file is what we want
    // on a removable disk (no -wal/-shm companions to lose).
    out.pragma_update(None, "journal_mode", "DELETE")
        .map_err(|e| format!("cannot finalise {}: {e}", dest.display()))?;
    out.close()
        .map_err(|(_, e)| format!("cannot finish {}: {e}", dest.display()))?;
    Ok(())
}

/// "Back up now": take the sync lock (so it never races a collection), open the
/// database and run one pass.
pub fn run_standalone(env: &KbEnv, query: &dyn SystemQuery) -> Result<BackupSummary, String> {
    use super::lock::SyncLock;
    let Some(_lock) = SyncLock::acquire(&env.data.lock(), super::sync::LOCK_WAIT)? else {
        return Ok(BackupSummary::skipped("locked"));
    };
    let conn = db::open(&env.data.db())?;
    let log = SyncLog::open(&env.data.logs());
    Ok(run(env, &conn, &log, query))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::config::SourceRoots;
    use crate::kb::paths::DataPaths;
    use crate::kb::sync::MirrorTarget;
    use crate::kb::volume::fake::{volume, FakeQuery};
    use crate::kb::volume::VolumeInfo;

    struct Rig {
        _tmp: tempfile::TempDir,
        data: PathBuf,
        disk: PathBuf,
        target: PathBuf,
        env: KbEnv,
        query: FakeQuery,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let disk = tmp.path().join("disk");
        fs::create_dir_all(&disk).unwrap();
        rig_in(tmp, disk, "aaaaaaaa11111111aaaaaaaa11111111")
    }

    /// Another computer (its own data folder and id) on the disk of `disk`.
    fn rig_on(disk: &Path, id: &str) -> Rig {
        rig_in(tempfile::tempdir().unwrap(), disk.to_path_buf(), id)
    }

    fn rig_in(tmp: tempfile::TempDir, disk: PathBuf, id: &str) -> Rig {
        let data = tmp.path().join("AgentKB");
        fs::create_dir_all(data.join("raw/claude/projects/p")).unwrap();
        let root = disk.join("AgentKB-backup");
        let id_file = tmp.path().join("cfg/backup-device.json");
        fs::create_dir_all(id_file.parent().unwrap()).unwrap();
        fs::write(&id_file, format!(r#"{{"id":"{id}"}}"#)).unwrap();
        // This computer's folder inside the configured one.
        let target = root.join(backup_dir::folder_name(
            &backup_dir::identity(&id_file).unwrap(),
        ));
        let env = KbEnv {
            data: DataPaths::new(&data),
            roots: SourceRoots::default(),
            exclude_prefixes: Vec::new(),
            exclude_cwd_prefixes: Vec::new(),
            mirror: Some(MirrorTarget {
                target_dir: root,
                volume_uuid: "DISK-UUID".into(),
                id_file,
            }),
            progress: ProgressSink::default(),
            force_reindex: false,
        };
        let query = FakeQuery::with([VolumeInfo {
            mount_point: disk.clone(),
            ..volume("DISK-UUID", "/x", false, "exfat")
        }]);
        Rig {
            _tmp: tmp,
            data,
            disk,
            target,
            env,
            query,
        }
    }

    fn put(rig: &Rig, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = rig.data.join("raw").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn pass(rig: &Rig) -> BackupSummary {
        let conn = db::open(&rig.env.data.db()).unwrap();
        let log = SyncLog::open(&rig.env.data.logs());
        run(&rig.env, &conn, &log, &rig.query)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    const REL: &str = "claude/projects/p/s.jsonl";

    #[test]
    fn first_pass_copies_everything_byte_for_byte_and_second_pass_does_nothing() {
        let rig = rig();
        put(&rig, REL, b"line1\nline2\n");
        put(
            &rig,
            "claude/projects/p/s.jsonl.prev-20260101T000000Z",
            b"old\n",
        );
        let first = pass(&rig);
        assert_eq!(first.status, "ok", "{first:?}");
        assert_eq!(first.copied_files, 2);
        assert!(first.snapshot_taken);
        assert_eq!(
            fs::read(rig.target.join("raw").join(REL)).unwrap(),
            b"line1\nline2\n"
        );
        assert!(rig.target.join("kb.db").is_file());

        let second = pass(&rig);
        assert_eq!(second.status, "ok");
        assert_eq!(
            (
                second.copied_files,
                second.appended_files,
                second.copied_bytes
            ),
            (0, 0, 0)
        );
        assert!(!second.snapshot_taken, "snapshot is at most once a day");
    }

    #[test]
    fn notes_are_backed_up_updated_in_place_and_never_removed() {
        let rig = rig();
        let note = rig.data.join("notes/claude/s-1.json");
        fs::create_dir_all(note.parent().unwrap()).unwrap();
        fs::write(&note, b"{\"v\":1}\n").unwrap();
        let copy = rig.target.join("notes/claude/s-1.json");

        pass(&rig);
        assert_eq!(fs::read(&copy).unwrap(), b"{\"v\":1}\n");
        assert_eq!(
            pass(&rig).copied_files,
            0,
            "an unchanged note is not copied again"
        );

        fs::write(&note, b"{\"v\":2}\n").unwrap();
        assert_eq!(pass(&rig).copied_files, 1);
        assert_eq!(fs::read(&copy).unwrap(), b"{\"v\":2}\n");

        fs::remove_file(&note).unwrap();
        pass(&rig);
        assert!(copy.is_file(), "nothing is ever deleted from the backup");
    }

    #[test]
    fn grown_files_are_appended_not_recopied() {
        let rig = rig();
        let src = put(&rig, REL, b"aaa\n");
        pass(&rig);
        fs::write(&src, b"aaa\nbbb\n").unwrap();
        let again = pass(&rig);
        assert_eq!(
            (again.appended_files, again.copied_files, again.copied_bytes),
            (1, 0, 4)
        );
        assert_eq!(
            fs::read(rig.target.join("raw").join(REL)).unwrap(),
            b"aaa\nbbb\n"
        );
    }

    const PLAIN: &str = "codex/sessions/2026/05/29/r.jsonl";
    const PACKED: &str = "codex/sessions/2026/05/29/r.jsonl.zst";
    const BODY: &[u8] = b"line one\nline two\nline three\n";

    /// The state Codex's compression leaves: a plain copy on the disk from
    /// before, and now the `.zst` in the main directory.
    fn compressed_in_main(rig: &Rig) {
        let plain = put(rig, PLAIN, BODY);
        pass(rig);
        fs::remove_file(plain).unwrap();
        put(rig, PACKED, &zstd::encode_all(BODY, 3).unwrap());
    }

    #[test]
    fn the_disk_loses_a_plain_copy_only_when_the_compressed_one_next_to_it_holds_it_all() {
        let rig = rig();
        compressed_in_main(&rig);
        let disk_plain = rig.target.join("raw").join(PLAIN);
        let disk_packed = rig.target.join("raw").join(PACKED);
        let after = pass(&rig);
        assert_eq!(after.status, "ok", "{after:?}");
        assert!(disk_packed.is_file());
        assert!(!disk_plain.exists(), "identical content: removed");
        assert_eq!(
            (after.duplicates_removed, after.duplicate_bytes_freed),
            (1, BODY.len() as u64)
        );
        assert_eq!(pass(&rig).duplicates_removed, 0);
    }

    #[test]
    fn a_plain_copy_on_the_disk_that_differs_or_is_still_in_the_main_directory_stays() {
        let rig = rig();
        compressed_in_main(&rig);
        let disk_plain = rig.target.join("raw").join(PLAIN);
        fs::write(&disk_plain, b"line one\nline TWO\n").unwrap();
        assert_eq!(pass(&rig).duplicates_removed, 0);
        assert!(disk_plain.is_file(), "different content: both stay");

        // The main directory has the plain file again: the disk's copy is not a
        // duplicate of anything that is gone.
        fs::write(&disk_plain, BODY).unwrap();
        put(&rig, PLAIN, BODY);
        assert_eq!(pass(&rig).duplicates_removed, 0);
        assert!(disk_plain.is_file());
    }

    #[test]
    fn nothing_is_ever_deleted_from_the_backup() {
        let rig = rig();
        let src = put(&rig, REL, b"keep me\n");
        pass(&rig);
        fs::remove_file(&src).unwrap();
        let again = pass(&rig);
        assert_eq!(again.status, "ok");
        assert_eq!(
            fs::read(rig.target.join("raw").join(REL)).unwrap(),
            b"keep me\n"
        );
    }

    #[test]
    fn a_rewrite_in_the_main_directory_keeps_the_old_backup_and_never_conflicts_again() {
        let rig = rig();
        let src = put(&rig, REL, b"original content that is long\n");
        pass(&rig);

        // The collector's rewrite: old mirror becomes `.prev-*`, the new
        // (shorter, different) content takes over the original name.
        let prev = src.with_file_name("s.jsonl.prev-20260202T000000Z");
        fs::rename(&src, &prev).unwrap();
        fs::write(&src, b"rewritten\n").unwrap();

        let after = pass(&rig);
        assert_eq!(after.status, "ok", "a rewrite is not an error: {after:?}");
        assert!(after.errors.is_empty());
        assert_eq!(after.set_aside_files, 1);

        let dir = rig.target.join("raw/claude/projects/p");
        let listing = names(&dir);
        assert!(
            listing.iter().any(|n| n.starts_with("s.jsonl.conflict-")),
            "{listing:?}"
        );
        assert!(listing.contains(&"s.jsonl.prev-20260202T000000Z".to_string()));
        for n in &listing {
            if n.contains(".conflict-") {
                assert!(
                    !Path::new(n)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("jsonl")),
                    "{n} would be mistaken for a session"
                );
            }
        }
        // Old version preserved, new version complete.
        let aside = listing
            .iter()
            .find(|n| n.starts_with("s.jsonl.conflict-"))
            .unwrap();
        assert_eq!(
            fs::read(dir.join(aside)).unwrap(),
            b"original content that is long\n"
        );
        assert_eq!(fs::read(dir.join("s.jsonl")).unwrap(), b"rewritten\n");

        let third = pass(&rig);
        assert_eq!(
            (
                third.set_aside_files,
                third.copied_files,
                third.errors.len()
            ),
            (0, 0, 0)
        );
        // And the new content keeps flowing.
        fs::write(&src, b"rewritten\nmore\n").unwrap();
        assert_eq!(pass(&rig).appended_files, 1);
        assert_eq!(fs::read(dir.join("s.jsonl")).unwrap(), b"rewritten\nmore\n");
    }

    #[test]
    fn a_missing_disk_is_a_silent_skip_and_writes_nothing_even_if_the_folder_exists() {
        let mut rig = rig();
        put(&rig, REL, b"x\n");
        // Disk unplugged: the same path now sits on the system disk.
        rig.query = FakeQuery::with([VolumeInfo {
            mount_point: PathBuf::from("/"),
            ..volume("SYS-UUID", "/", true, "apfs")
        }]);
        fs::create_dir_all(&rig.target).unwrap(); // the leftover plain folder
        let result = pass(&rig);
        assert_eq!(result.status, "skipped");
        assert_eq!(result.reason.as_deref(), Some("not_connected"));
        assert!(result.errors.is_empty());
        assert!(
            names(&rig.target).is_empty(),
            "nothing may be written there"
        );
        assert!(!rig.target.join("raw").exists());
    }

    #[test]
    fn a_different_disk_with_the_same_folder_is_skipped_too() {
        let mut rig = rig();
        put(&rig, REL, b"x\n");
        rig.query = FakeQuery::with([VolumeInfo {
            mount_point: rig.disk.clone(),
            ..volume("OTHER-UUID", "/x", false, "exfat")
        }]);
        assert_eq!(pass(&rig).status, "skipped");
        assert!(!rig.target.exists());
    }

    #[test]
    fn disabled_backup_does_nothing() {
        let mut rig = rig();
        rig.env.mirror = None;
        assert_eq!(pass(&rig).status, "skipped");
    }

    #[test]
    fn an_interrupted_copy_resumes_from_the_destination_length() {
        let rig = rig();
        let src = put(&rig, REL, b"0123456789abcdef\n");
        // Simulate a copy that died after 10 bytes (destination is a prefix,
        // maybe with an orphaned tail written past the cursor).
        let dst = rig.target.join("raw").join(REL);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, b"0123456789").unwrap();
        let resumed = pass(&rig);
        assert_eq!(resumed.status, "ok");
        assert_eq!((resumed.appended_files, resumed.copied_bytes), (1, 7));
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
    }

    #[test]
    fn db_snapshot_is_consistent_rotates_once_a_day_and_keeps_one_previous() {
        let rig = rig();
        put(&rig, REL, b"x\n");
        let conn = db::open(&rig.env.data.db()).unwrap();
        db::set_meta(&conn, "marker", "v1").unwrap();
        let log = SyncLog::open(&rig.env.data.logs());
        assert!(run(&rig.env, &conn, &log, &rig.query).snapshot_taken);
        let snap = Connection::open(rig.target.join("kb.db")).unwrap();
        assert_eq!(
            db::get_meta(&snap, "marker").unwrap().as_deref(),
            Some("v1")
        );
        drop(snap);

        // Pretend yesterday's snapshot: a new one is taken and the old kept.
        db::set_meta(&conn, "marker", "v2").unwrap();
        db::set_meta(&conn, META_SNAPSHOT_DAY, "2000-01-01").unwrap();
        assert!(run(&rig.env, &conn, &log, &rig.query).snapshot_taken);
        let new = Connection::open(rig.target.join("kb.db")).unwrap();
        let prev = Connection::open(rig.target.join("kb.db.prev")).unwrap();
        assert_eq!(db::get_meta(&new, "marker").unwrap().as_deref(), Some("v2"));
        assert_eq!(
            db::get_meta(&prev, "marker").unwrap().as_deref(),
            Some("v1")
        );
        assert_eq!(
            names(&rig.target),
            vec!["agentkb-backup.json", "kb.db", "kb.db.prev", "raw"],
            "no stray temp files or dated snapshots"
        );
    }

    fn snapshot_rig() -> (Rig, Connection) {
        let rig = rig();
        let conn = db::open(&rig.env.data.db()).unwrap();
        db::set_meta(&conn, "marker", "old").unwrap();
        fs::create_dir_all(&rig.target).unwrap();
        let (live, prev) = (rig.target.join("kb.db"), rig.target.join("kb.db.prev"));
        online_backup(&conn, &live).unwrap();
        online_backup(&conn, &prev).unwrap();
        db::set_meta(&conn, "marker", "new").unwrap();
        (rig, conn)
    }

    fn marker(path: &Path) -> String {
        let c = Connection::open(path).unwrap();
        db::get_meta(&c, "marker").unwrap().unwrap()
    }

    #[test]
    fn a_snapshot_is_built_locally_and_copied_whole() {
        let (rig, conn) = snapshot_rig();
        let local = rig.data.join("kb.db.snapshot-test");
        let staged = rig.target.join("kb.db.new-test");
        place_snapshot(&conn, &local, &staged, &rig.target, |a, b| {
            fsutil::copy_atomic(a, b).map(|_| ())
        })
        .unwrap();
        assert_eq!(marker(&rig.target.join("kb.db")), "new");
        assert_eq!(marker(&rig.target.join("kb.db.prev")), "old");
        assert!(!staged.exists());
    }

    #[test]
    fn a_copy_that_fails_halfway_leaves_the_old_snapshots_and_no_litter() {
        let (rig, conn) = snapshot_rig();
        let local = rig.data.join("kb.db.snapshot-test");
        let staged = rig.target.join("kb.db.new-test");
        let err = place_snapshot(&conn, &local, &staged, &rig.target, |a, b| {
            let bytes = fs::read(a).unwrap();
            fs::write(b, &bytes[..bytes.len() / 2]).unwrap();
            Err("disk went away".into())
        })
        .unwrap_err();
        assert!(err.contains("disk went away"));
        assert_eq!(marker(&rig.target.join("kb.db")), "old");
        assert_eq!(marker(&rig.target.join("kb.db.prev")), "old");
        let _ = fs::remove_file(&staged); // snapshot_db removes both on any error
    }

    #[test]
    fn a_copy_that_does_not_match_is_never_placed() {
        let (rig, conn) = snapshot_rig();
        let local = rig.data.join("kb.db.snapshot-test");
        let staged = rig.target.join("kb.db.new-test");
        let err = place_snapshot(&conn, &local, &staged, &rig.target, |a, b| {
            let mut bytes = fs::read(a).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 0xff;
            fs::write(b, bytes).map_err(|e| e.to_string())
        })
        .unwrap_err();
        assert!(err.contains("differs"), "{err}");
        assert_eq!(marker(&rig.target.join("kb.db")), "old");
        assert_eq!(marker(&rig.target.join("kb.db.prev")), "old");
    }

    #[test]
    fn a_failed_snapshot_removes_the_local_and_staged_temp_files() {
        let (rig, conn) = snapshot_rig();
        // the target has no room for the staged file: its parent is a plain file
        let blocker = rig.disk.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let local = rig.data.join("kb.db.snapshot-test");
        let staged = blocker.join("kb.db.new-test");
        assert!(place_snapshot(&conn, &local, &staged, &rig.target, |a, b| {
            fsutil::copy_atomic(a, b).map(|_| ())
        })
        .is_err());
        assert_eq!(marker(&rig.target.join("kb.db")), "old");
    }

    /// Real exFAT: needs macOS and `hdiutil`; run by hand with
    /// `cargo test exfat_disk_image -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "creates and mounts an exFAT disk image"]
    fn exfat_disk_image_takes_a_snapshot() {
        use std::process::Command;
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("disk.dmg");
        let ok = |c: &mut Command| c.output().unwrap();
        let out = ok(Command::new("hdiutil")
            .args([
                "create", "-size", "64m", "-fs", "ExFAT", "-volname", "P72", "-attach",
            ])
            .arg(&image));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mount = PathBuf::from("/Volumes/P72");
        let result = std::panic::catch_unwind(|| {
            let rig = rig();
            let conn = db::open(&rig.env.data.db()).unwrap();
            db::set_meta(&conn, "marker", "exfat").unwrap();
            let target = mount.join("AgentKB-backup");
            fs::create_dir_all(&target).unwrap();
            let local = rig.data.join("kb.db.snapshot-test");
            let staged = target.join("kb.db.new-test");
            place_snapshot(&conn, &local, &staged, &target, |a, b| {
                fsutil::copy_atomic(a, b).map(|_| ())
            })
            .unwrap();
            assert_eq!(marker(&target.join("kb.db")), "exfat");
            // the old way, for the record: SQLite writing on the disk itself
            let direct = online_backup(&conn, &target.join("direct.db"));
            eprintln!("direct online_backup on exFAT: {direct:?}");
        });
        let _ = Command::new("hdiutil")
            .args(["detach", "-force"])
            .arg(&mount)
            .output();
        result.unwrap();
    }

    fn other_computer(rig: &Rig, id: &str) -> Rig {
        // A second computer: its own data folder and id, the same disk.
        rig_on(&rig.disk, id)
    }

    #[test]
    fn two_computers_back_up_to_one_disk_without_touching_each_other() {
        let a = rig();
        put(&a, REL, b"from A\n");
        let b = other_computer(&a, "bbbbbbbb22222222bbbbbbbb22222222");
        put(&b, REL, b"from B, a different file with the same name\n");
        let (ra, rb) = (pass(&a), pass(&b));
        assert_eq!(ra.status, "ok", "{:?}", ra.errors);
        assert_eq!(rb.status, "ok", "{:?}", rb.errors);
        assert_ne!(a.target, b.target);
        assert_eq!(
            fs::read(a.target.join("raw").join(REL)).unwrap(),
            b"from A\n"
        );
        assert_eq!(
            fs::read(b.target.join("raw").join(REL)).unwrap(),
            b"from B, a different file with the same name\n"
        );
        // Each snapshot is that computer's own, and nothing was set aside.
        for rig in [&a, &b] {
            assert!(rig.target.join("kb.db").is_file());
            assert_eq!(
                names(&rig.target.join("raw/claude/projects/p")),
                vec!["s.jsonl"],
                "no .prev-* or .conflict-*"
            );
            let marker = backup_dir::read_marker(&rig.target).unwrap();
            assert!(!marker.last_backup_at.is_empty() && !marker.first_backup_at.is_empty());
        }
        assert_eq!(names(&a.disk.join("AgentKB-backup")).len(), 2);
        // A second pass changes nothing for anybody.
        assert_eq!(pass(&a).copied_files, 0);
        assert_eq!(pass(&b).copied_files, 0);
    }

    #[test]
    fn a_folder_of_another_computer_is_never_written_to() {
        let a = rig();
        put(&a, REL, b"x\n");
        // The folder this computer would use already belongs to somebody else.
        fs::create_dir_all(&a.target).unwrap();
        fs::write(
            a.target.join(backup_dir::MARKER_NAME),
            r#"{"id":"cccccccc33333333","name":"Windows-PC","os":"windows"}"#,
        )
        .unwrap();
        // (The scan finds no folder of ours, then meets the planned name.)
        let summary = pass(&a);
        assert_eq!(summary.status, "error");
        assert!(
            summary.errors[0].message.contains("another computer"),
            "{:?}",
            summary.errors
        );
        assert_eq!(
            names(&a.target),
            vec!["agentkb-backup.json"],
            "nothing was written"
        );
    }

    #[test]
    fn renaming_the_computer_keeps_the_same_folder() {
        let a = rig();
        put(&a, REL, b"x\n");
        assert_eq!(pass(&a).status, "ok");
        // The marker still carries the id; the folder name is whatever it was.
        let mut marker = backup_dir::read_marker(&a.target).unwrap();
        marker.name = "renamed-long-ago".into();
        fs::write(
            a.target.join(backup_dir::MARKER_NAME),
            serde_json::to_string(&marker).unwrap(),
        )
        .unwrap();
        let old = a.target.clone();
        let renamed = a.disk.join("AgentKB-backup/Old-Name-aaaaaaaa");
        fs::rename(&old, &renamed).unwrap();
        put(&a, "claude/projects/p/t.jsonl", b"y\n");
        assert_eq!(pass(&a).status, "ok");
        assert_eq!(
            names(&a.disk.join("AgentKB-backup")),
            vec!["Old-Name-aaaaaaaa"]
        );
        assert!(renamed.join("raw/claude/projects/p/t.jsonl").is_file());
    }

    #[test]
    fn a_disks_root_as_target_backs_up_into_the_backup_folder_and_adopts_own_strays() {
        let mut a = rig();
        put(&a, REL, b"x\n");
        assert_eq!(pass(&a).status, "ok");
        // 0.7.4 on a disk-root target left the folder at the root.
        let stray = a.disk.join(a.target.file_name().unwrap());
        fs::rename(&a.target, &stray).unwrap();
        let other = a.disk.join("Mac-bbbbbbbb");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join(backup_dir::MARKER_NAME),
            r#"{"id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","name":"Mac","os":"macos"}"#,
        )
        .unwrap();
        a.env.mirror.as_mut().unwrap().target_dir = a.disk.clone();
        put(&a, "claude/projects/p/t.jsonl", b"y\n");
        let summary = pass(&a);
        assert_eq!(summary.status, "ok", "{:?}", summary.errors);
        assert!(!stray.exists(), "own folder moved into AgentKB-backup");
        assert!(a.target.join("raw/claude/projects/p/t.jsonl").is_file());
        assert!(other.exists(), "another computer's folder is not touched");
        assert_eq!(
            summary.folder.as_deref(),
            Some(a.target.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn the_folder_and_the_old_layout_are_reported_for_the_settings_page() {
        let a = rig();
        put(&a, REL, b"x\n");
        let root = a.disk.join("AgentKB-backup");
        fs::create_dir_all(root.join("raw")).unwrap();
        fs::write(root.join("kb.db"), b"not ours, not readable").unwrap();
        let summary = pass(&a);
        assert_eq!(summary.status, "ok", "{:?}", summary.errors);
        assert!(summary.old_backup_left && !summary.moved_old_backup);
        assert_eq!(
            summary.folder.as_deref(),
            Some(a.target.to_string_lossy().as_ref())
        );
        let conn = db::open(&a.env.data.db()).unwrap();
        assert_eq!(
            db::get_meta(&conn, META_FOLDER).unwrap().as_deref(),
            Some(a.target.to_string_lossy().as_ref())
        );
        assert_eq!(
            db::get_meta(&conn, META_OLD_LAYOUT).unwrap().as_deref(),
            Some("1")
        );
    }

    #[test]
    fn state_and_result_are_recorded_for_the_status_page() {
        let rig = rig();
        put(&rig, REL, b"x\n");
        pass(&rig);
        let conn = db::open(&rig.env.data.db()).unwrap();
        assert_eq!(
            db::get_meta(&conn, META_LAST_RESULT).unwrap().as_deref(),
            Some("ok")
        );
        assert_eq!(
            db::get_meta(&conn, META_STATE).unwrap().as_deref(),
            Some("idle")
        );
        assert!(db::get_meta(&conn, META_LAST_AT).unwrap().is_some());
    }
}
