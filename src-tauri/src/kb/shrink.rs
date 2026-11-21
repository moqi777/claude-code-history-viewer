//! Old backups stored compressed (phase 8b; `docs/agent-kb/P8b-design.md`).
//!
//! The backup of a session that has had no new message for `after_days` days
//! is replaced by one zstd stream, `X.jsonl.packed.zst` (`files.packed = 1`).
//! The source stays plain text, and every position in the database
//! (`mirror_offset`, `head_*`, `messages.byte_off`) keeps meaning the plain
//! text: readers decompress and skip, as for the `.zst` files Codex writes.
//! When the source grows again, the backup is unpacked first and then
//! appended to as before.
//!
//! Every replacement, compressing and unpacking alike:
//! 1. the new file is written beside the target as `<target>.tmp-<pid>` and fsynced;
//! 2. it is checked against what it replaces (SHA-256 of the plain text, or
//!    the zstd checksum and the length);
//! 3. it is renamed into place (a leftover at that name from an interrupted
//!    run is removed when proven redundant, set aside otherwise);
//! 4. the row is pointed at it in one transaction;
//! 5. the old file is removed. If that fails (Windows: someone has it open) it
//!    stays, and the duplicate cleanup (`dedupe.rs`) removes it later.
//!
//! Killed at any step, the file the row points to is complete.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::config::PackConfig;
use super::db::{self, FileRow};
use super::fsutil;
use super::log::SyncLog;
use super::mirror;
use super::paths::DataPaths;
use super::progress::{Progress, ProgressSink};

pub const PACKED_SUFFIX: &str = ".packed.zst";
/// Measured in `P8-design.md` 4.1: level 12 saves almost nothing more and
/// takes twice as long.
pub const LEVEL: i32 = 9;
/// Smaller backups save too little to be worth another storage form.
pub const MIN_BYTES: u64 = 4 * 1024;
/// Reading the last line of a compressed backup decompresses all before it;
/// above this it could take more than a second.
pub const MAX_BYTES: u64 = 1 << 30;
/// How long the pass inside a scheduled collection may run.
pub const BUDGET: Duration = Duration::from_secs(20);

const META_LAST_AT: &str = "shrink_last_at";
const META_LAST_MS: &str = "shrink_last_ms";
const META_LAST_FILES: &str = "shrink_last_files";

pub fn packed_rel(plain: &str) -> String {
    format!("{plain}{PACKED_SUFFIX}")
}

/// `X.jsonl` for `X.jsonl.packed.zst`; `None` for any other name.
pub fn plain_rel(rel: &str) -> Option<&str> {
    rel.strip_suffix(PACKED_SUFFIX)
}

fn is_zst(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "zst")
}

// ---------------------------------------------------------------------------
// Test hooks
// ---------------------------------------------------------------------------

/// A step at which a test wants the work to stop as if the process died.
#[cfg(test)]
pub static DIE_AT: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// Runs at every step (tests only), e.g. to change the source meanwhile.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub static ON_STEP: std::sync::Mutex<Option<Box<dyn Fn(&str) + Send>>> =
    std::sync::Mutex::new(None);

#[allow(clippy::unnecessary_wraps)]
fn step(name: &'static str) -> Result<(), String> {
    #[cfg(test)]
    if let Some(hook) = ON_STEP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        hook(name);
    }
    #[cfg(test)]
    if *DIE_AT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        == Some(name)
    {
        return Err(format!("simulated crash at {name}"));
    }
    let _ = name;
    Ok(())
}

// ---------------------------------------------------------------------------
// Which backups
// ---------------------------------------------------------------------------

/// Backups to compress now, oldest first: stored plain by us, between
/// [`MIN_BYTES`] and [`MAX_BYTES`], and both the last message and the source
/// file's modification older than `after_days` (a session without message
/// times goes by the file time alone).
pub fn candidates(
    conn: &Connection,
    now: DateTime<Utc>,
    after_days: u32,
) -> Result<Vec<FileRow>, String> {
    let cutoff = now - chrono::Duration::days(i64::from(after_days));
    let cutoff_ns = cutoff.timestamp_nanos_opt().unwrap_or(i64::MAX);
    let mut last: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT file_id, last_activity_at FROM sessions WHERE last_activity_at IS NOT NULL",
            )
            .map_err(|e| format!("prepare sessions: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("query sessions: {e}"))?;
        for row in rows {
            let (id, at) = row.map_err(|e| format!("read sessions: {e}"))?;
            last.insert(id, at);
        }
    }
    let mut out: Vec<FileRow> = db::load_files(conn)?
        .into_iter()
        .filter(|row| {
            let len = u64::try_from(row.mirror_offset).unwrap_or(0);
            !row.mirror_is_zst()
                && (MIN_BYTES..=MAX_BYTES).contains(&len)
                && row.src_mtime_ns < cutoff_ns
                && last
                    .get(&row.id)
                    .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                    .map_or(true, |at| at.with_timezone(&Utc) < cutoff)
        })
        .collect();
    out.sort_by_key(|r| r.src_mtime_ns);
    Ok(out)
}

// ---------------------------------------------------------------------------
// One file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    /// Size of the file the row now points to.
    pub bytes: u64,
    /// The file it replaced, when removing it failed (removed later).
    pub left_behind: Option<String>,
}

/// Put a checked temp file in place of `target`. Something already there is
/// not what the row points to (the caller's row points elsewhere): a leftover
/// of an interrupted run. It goes when the new file holds all of it, and is
/// set aside as `.prev-*` otherwise. Nothing is lost if this fails: the temp
/// file is removed and the row still points to the old file.
fn place(tmp: &Path, target: &Path) -> Result<(), String> {
    let result = (|| {
        if target.exists() {
            // Both are in the target's form (the temp file's name ends in `.tmp-<pid>`).
            let zst = is_zst(target);
            let redundant = mirror::content_continues((target, zst, None), (tmp, zst));
            if redundant == Ok(true) {
                fsutil::remove_synced(target)?;
            } else {
                mirror::rotate_aside(target)
                    .map_err(|e| format!("cannot move {} aside: {e}", target.display()))?;
            }
        }
        fsutil::install_tmp(tmp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

/// SHA-256 of the first `len` bytes `reader` yields, and how many it yielded.
fn hash_of(mut reader: impl Read, len: u64) -> std::io::Result<(Vec<u8>, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut left = len;
    let mut seen = 0u64;
    while left > 0 {
        let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        left -= n as u64;
        seen += n as u64;
    }
    // Anything past `len` means the stream is longer than expected.
    if left == 0 && reader.read(&mut [0u8; 1])? != 0 {
        seen += 1;
    }
    Ok((hasher.finalize().to_vec(), seen))
}

/// Replace the plain backup of `row` by `X.jsonl.packed.zst` holding exactly
/// its first `mirror_offset` bytes (anything past the cursor is the orphan
/// tail of an interrupted append and is not part of the backup).
pub fn pack_one(conn: &Connection, data: &DataPaths, row: &FileRow) -> Result<Replaced, String> {
    if row.mirror_is_zst() {
        return Err(format!("{} is compressed already", row.mirror_rel));
    }
    let plain = data.mirror_path(&row.mirror_rel)?;
    let target_rel = packed_rel(&row.mirror_rel);
    let target = data.mirror_path(&target_rel)?;
    let len = u64::try_from(row.mirror_offset).unwrap_or(0);

    let mut plain_hash = Vec::new();
    let tmp = fsutil::write_tmp(&target, |out| {
        let input = File::open(&plain)?;
        if input.metadata()?.len() < len {
            return Err(std::io::Error::other(
                "the backup is shorter than its cursor",
            ));
        }
        let mut encoder = zstd::stream::Encoder::new(out, LEVEL)?;
        encoder.include_checksum(true)?;
        encoder.set_pledged_src_size(Some(len))?;
        let mut hasher = Sha256::new();
        let mut input = input.take(len);
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            encoder.write_all(&buf[..n])?;
        }
        encoder.finish()?;
        plain_hash = hasher.finalize().to_vec();
        Ok(())
    })?;
    let checked = File::open(&tmp)
        .and_then(zstd::stream::Decoder::new)
        .and_then(|d| hash_of(d, len));
    match checked {
        Ok((hash, seen)) if hash == plain_hash && seen == len => {}
        other => {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "the compressed copy of {} does not read back the same: {other:?}",
                row.mirror_rel
            ));
        }
    }
    let bytes = std::fs::metadata(&tmp)
        .map_err(|e| format!("cannot stat {}: {e}", tmp.display()))?
        .len();
    step("pack:after_tmp")?;
    place(&tmp, &target)?;
    step("pack:after_rename")?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin: {e}"))?;
    db::set_packed(&tx, row.id, &target_rel, true)?;
    tx.commit().map_err(|e| format!("commit: {e}"))?;
    step("pack:after_db")?;
    let left_behind = fsutil::remove_synced(&plain)
        .err()
        .map(|_| row.mirror_rel.clone());
    Ok(Replaced { bytes, left_behind })
}

/// Turn the compressed backup of `row` back into the plain `X.jsonl`, so new
/// lines can be appended. Returns the row as it now is.
pub fn unpack(
    conn: &Connection,
    data: &DataPaths,
    row: &FileRow,
) -> Result<(FileRow, Replaced), String> {
    let plain_rel = plain_rel(&row.mirror_rel)
        .filter(|_| row.packed)
        .ok_or_else(|| format!("{} is not a backup we compressed", row.mirror_rel))?
        .to_string();
    let packed = data.mirror_path(&row.mirror_rel)?;
    let target = data.mirror_path(&plain_rel)?;
    let len = u64::try_from(row.mirror_offset).unwrap_or(0);

    let tmp = fsutil::write_tmp(&target, |out| {
        let mut decoder = zstd::stream::Decoder::new(File::open(&packed)?)?;
        // The stream carries a checksum: a damaged file fails here.
        let n = std::io::copy(&mut decoder, out)?;
        if n == len {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "it holds {n} bytes, the row vouches for {len}"
            )))
        }
    })
    .map_err(|e| format!("cannot unpack {}: {e}", row.mirror_rel))?;
    let bytes = len;
    step("unpack:after_tmp")?;
    place(&tmp, &target)?;
    step("unpack:after_rename")?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin: {e}"))?;
    db::set_packed(&tx, row.id, &plain_rel, false)?;
    tx.commit().map_err(|e| format!("commit: {e}"))?;
    step("unpack:after_db")?;
    let left_behind = fsutil::remove_synced(&packed)
        .err()
        .map(|_| row.mirror_rel.clone());
    let mut now = row.clone();
    now.mirror_rel = plain_rel;
    now.packed = false;
    Ok((now, Replaced { bytes, left_behind }))
}

// ---------------------------------------------------------------------------
// A pass
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PassSummary {
    pub packed_files: u64,
    /// Plain bytes of the files compressed in this pass, and what they take now.
    pub plain_bytes: u64,
    pub packed_bytes: u64,
    pub failed_files: u64,
    /// Old files that could not be removed yet (the duplicate cleanup retries).
    pub left_behind: u64,
    /// Candidates not reached because the time was up.
    pub remaining: u64,
    pub temp_files_removed: u64,
    pub duration_ms: u64,
}

/// Temp files in `raw/` are leftovers of a process that died: everything
/// that writes there holds the collection lock, which the caller holds now.
pub fn remove_temp_files(data: &DataPaths, log: &SyncLog) -> u64 {
    let raw = data.raw();
    let mut removed = 0;
    for (rel, _) in fsutil::walk_files(&raw) {
        let name = rel.rsplit('/').next().unwrap_or(&rel);
        if !fsutil::is_tmp_name(name) {
            continue;
        }
        match fsutil::remove_synced(&raw.join(&rel)) {
            Ok(()) => {
                log.line(&format!("removed leftover temp file {rel}"));
                removed += 1;
            }
            Err(message) => log.line(&format!("leftover temp file {rel}: {message}")),
        }
    }
    removed
}

/// Compress what is due, oldest first, until `budget` is used up (`None`: all
/// of it). The caller holds the collection lock.
pub fn run_pass(
    conn: &Connection,
    data: &DataPaths,
    settings: &PackConfig,
    log: &SyncLog,
    budget: Option<Duration>,
    progress: &ProgressSink,
) -> Result<PassSummary, String> {
    let started = Instant::now();
    let mut summary = PassSummary {
        temp_files_removed: remove_temp_files(data, log),
        ..PassSummary::default()
    };
    let due = candidates(conn, Utc::now(), settings.after_days)?;
    let total = due.len() as u64;
    let total_bytes: u64 = due
        .iter()
        .map(|r| u64::try_from(r.mirror_offset).unwrap_or(0))
        .sum();
    for (index, row) in due.iter().enumerate() {
        if budget.is_some_and(|b| started.elapsed() >= b) {
            summary.remaining = total - index as u64;
            log.line(&format!(
                "compressing old backups: time is up, {} left for next time",
                summary.remaining
            ));
            break;
        }
        progress.emit(&Progress {
            op: "shrink".into(),
            phase: "compress".into(),
            done: index as u64,
            total,
            bytes_done: summary.plain_bytes,
            bytes_total: total_bytes,
        });
        match pack_one(conn, data, row) {
            Ok(done) => {
                let plain = u64::try_from(row.mirror_offset).unwrap_or(0);
                summary.packed_files += 1;
                summary.plain_bytes += plain;
                summary.packed_bytes += done.bytes;
                log.line(&format!(
                    "compressed backup {} ({plain} -> {} bytes)",
                    row.mirror_rel, done.bytes
                ));
                if let Some(old) = done.left_behind {
                    summary.left_behind += 1;
                    log.line(&format!("{old}: cannot remove it yet, next time again"));
                }
            }
            Err(message) => {
                summary.failed_files += 1;
                log.line(&format!("ERROR compressing {}: {message}", row.mirror_rel));
            }
        }
    }
    summary.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    progress.emit(&Progress {
        op: "shrink".into(),
        phase: "compress".into(),
        done: total - summary.remaining,
        total,
        bytes_done: summary.plain_bytes,
        bytes_total: total_bytes,
    });
    if summary.packed_files > 0 || summary.failed_files > 0 || budget.is_none() {
        let _ = db::set_meta(conn, META_LAST_AT, &Utc::now().to_rfc3339());
        let _ = db::set_meta(conn, META_LAST_MS, &summary.duration_ms.to_string());
        let _ = db::set_meta(conn, META_LAST_FILES, &summary.packed_files.to_string());
    }
    Ok(summary)
}

/// [`run_pass`] on a thread of its own at background priority, with its own
/// connection, so the collection that started it is not slowed down by it.
pub fn run_pass_politely(
    data: &DataPaths,
    settings: &PackConfig,
    log: &SyncLog,
    budget: Option<Duration>,
    progress: &ProgressSink,
) -> Result<PassSummary, String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                lower_thread_priority();
                let conn = db::open(&data.db())?;
                run_pass(&conn, data, settings, log, budget, progress)
            })
            .join()
            .unwrap_or_else(|_| Err("the compression thread stopped unexpectedly".into()))
    })
}

#[allow(unsafe_code)] // plain OS calls with constant arguments
fn lower_thread_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: affects only the calling thread.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
    }
    #[cfg(target_os = "linux")]
    // SAFETY: affects only the calling thread (Linux nice values are per thread).
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
    #[cfg(windows)]
    super::platform::lower_thread_priority();
}

/// What "compress now" did, or that a collection was running.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct NowSummary {
    /// `ok`, `skipped` (a collection holds the lock) or `partial`.
    pub status: String,
    #[serde(flatten)]
    pub pass: PassSummary,
    pub duplicates_removed: u64,
}

/// "Compress now" (settings page, `--kb-shrink --all`): everything due, with
/// no time limit and whether or not the automatic pass is switched on, then
/// the duplicate cleanup for whatever could not be removed right away.
pub fn run_now(env: &super::sync::KbEnv) -> Result<NowSummary, String> {
    let Some(_lock) = super::lock::SyncLock::acquire(&env.data.lock(), super::sync::LOCK_WAIT)?
    else {
        return Ok(NowSummary {
            status: "skipped".into(),
            ..NowSummary::default()
        });
    };
    let log = SyncLog::open(&env.data.logs());
    log.line("compress now");
    let conn = db::open(&env.data.db())?;
    let pass = run_pass(&conn, &env.data, &env.pack, &log, None, &env.progress)?;
    let removed = super::dedupe::clean_raw(&conn, &env.data, &log)?;
    super::dedupe::record(&conn, removed)?;
    Ok(NowSummary {
        status: if pass.failed_files == 0 {
            "ok"
        } else {
            "partial"
        }
        .into(),
        pass,
        duplicates_removed: removed.files,
    })
}

// ---------------------------------------------------------------------------
// What the settings page shows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Stats {
    pub files: u64,
    /// What they held as plain text, and what they take now.
    pub plain_bytes: u64,
    pub packed_bytes: u64,
    pub last_at: Option<String>,
    pub last_ms: Option<u64>,
    pub last_files: Option<u64>,
}

pub fn stats(conn: &Connection, data: &DataPaths) -> Result<Stats, String> {
    let mut out = Stats::default();
    for row in db::load_files(conn)?.into_iter().filter(|r| r.packed) {
        out.files += 1;
        out.plain_bytes += u64::try_from(row.mirror_offset).unwrap_or(0);
        out.packed_bytes += data
            .mirror_path(&row.mirror_rel)
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
    }
    out.last_at = db::get_meta(conn, META_LAST_AT)?;
    out.last_ms = db::get_meta(conn, META_LAST_MS)?.and_then(|v| v.parse().ok());
    out.last_files = db::get_meta(conn, META_LAST_FILES)?.and_then(|v| v.parse().ok());
    Ok(out)
}

/// The backup `path` names, as plain text: decompressed when it is a `.zst`.
pub fn open_plain(path: &Path) -> std::io::Result<Box<dyn Read>> {
    let file = File::open(path)?;
    if is_zst(path) {
        Ok(Box::new(zstd::stream::Decoder::new(file)?))
    } else {
        Ok(Box::new(file))
    }
}
