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
    pub snapshot_taken: bool,
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

    let _ = db::set_meta(conn, META_STATE, "running");
    let _ = db::set_meta(conn, META_PROGRESS, "0/0");
    log.line(&format!("backup start -> {}", target.target_dir.display()));
    let mut summary = BackupSummary::default();
    if let Err(message) = copy_all(env, conn, log, &target.target_dir, &mount, &mut summary) {
        summary
            .errors
            .push(backup_error(&target.target_dir, message));
    }
    match snapshot_db(env, conn, &target.target_dir, &mount) {
        Ok(taken) => summary.snapshot_taken = taken,
        Err(message) => summary.errors.push(backup_error(
            &target.target_dir.join(SNAPSHOT_NAME),
            message,
        )),
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
    Ok(())
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
    let tmp = target_dir.join(format!("{SNAPSHOT_NAME}.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = (|| -> Result<(), String> {
        online_backup(conn, &tmp)?;
        let check = Connection::open(&tmp).map_err(|e| format!("cannot open the snapshot: {e}"))?;
        let verdict: String = check
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| format!("integrity check failed to run: {e}"))?;
        if verdict != "ok" {
            return Err(format!("snapshot failed its integrity check: {verdict}"));
        }
        drop(check);
        let live = target_dir.join(SNAPSHOT_NAME);
        if live.exists() {
            fs::rename(&live, target_dir.join(SNAPSHOT_PREV_NAME))
                .map_err(|e| format!("cannot rotate the previous snapshot: {e}"))?;
        }
        fs::rename(&tmp, &live).map_err(|e| format!("cannot place the snapshot: {e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    db::set_meta(conn, META_SNAPSHOT_DAY, &today)?;
    Ok(true)
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
        let data = tmp.path().join("AgentKB");
        let disk = tmp.path().join("disk");
        fs::create_dir_all(data.join("raw/claude/projects/p")).unwrap();
        fs::create_dir_all(&disk).unwrap();
        let target = disk.join("AgentKB-backup");
        let env = KbEnv {
            data: DataPaths::new(&data),
            roots: SourceRoots::default(),
            exclude_prefixes: Vec::new(),
            mirror: Some(MirrorTarget {
                target_dir: target.clone(),
                volume_uuid: "DISK-UUID".into(),
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
            vec!["kb.db", "kb.db.prev", "raw"],
            "no stray temp files or dated snapshots"
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
