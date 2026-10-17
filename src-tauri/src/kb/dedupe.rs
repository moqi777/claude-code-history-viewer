//! Duplicate backups left behind when Codex compresses a rollout in place.
//!
//! Codex turns `X.jsonl` into `X.jsonl.zst`. The collector follows the session
//! to the new file and mirrors it as `X.jsonl.zst`, and the database row now
//! points there; the earlier plain mirror `X.jsonl` stays in `raw/` with
//! nothing referring to it. It holds nothing the `.zst` does not: this module
//! removes it, but only after proving that every byte of it is the start of
//! (or all of) what the `.zst` decompresses to. If the proof fails or cannot
//! be made (different content, a damaged `.zst`, a read error) both files stay.
//!
//! Removal is the last step and nothing is written before it, so being killed
//! at any point leaves every piece of content on disk.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::db;
use super::fsutil;
use super::log::SyncLog;
use super::mirror::content_continues;
use super::paths::DataPaths;

/// One pass stops starting new proofs after this long; the next pass goes on.
pub const BUDGET: Duration = Duration::from_secs(60);

const META_TOTAL_FILES: &str = "dedupe_total_files";
const META_TOTAL_BYTES: &str = "dedupe_total_bytes";
const META_UNSEEN_FILES: &str = "dedupe_unseen_files";
const META_UNSEEN_BYTES: &str = "dedupe_unseen_bytes";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Removed {
    pub files: u64,
    pub bytes: u64,
}

/// Whether `plain` (as stored) is the beginning of, or all of, what `zst`
/// decompresses to. An empty `plain` proves nothing and is kept.
pub fn is_redundant(plain: &Path, zst: &Path) -> Result<bool, String> {
    content_continues((plain, false, None), (zst, true))
}

/// The twin of `X.jsonl` is `X.jsonl.zst`; anything else is left alone
/// (`.prev-*` and `.conflict-*` files are kept sets of older content).
#[allow(clippy::case_sensitive_file_extension_comparisons)] // exact names: Codex writes `.jsonl`
fn twin_of(rel: &str) -> Option<String> {
    rel.ends_with(".jsonl").then(|| format!("{rel}.zst"))
}

/// Remove the plain mirrors in `raw/` that no database row points to and whose
/// `.zst` twin is referenced and proves to hold all of their content.
pub fn clean_raw(conn: &Connection, data: &DataPaths, log: &SyncLog) -> Result<Removed, String> {
    let started = Instant::now();
    let referenced: HashSet<String> = db::load_files(conn)?
        .into_iter()
        .map(|row| row.mirror_rel)
        .collect();
    let raw = data.raw();
    let mut removed = Removed::default();
    for (rel, size) in fsutil::walk_files(&raw) {
        if referenced.contains(&rel) {
            continue;
        }
        let Some(twin) = twin_of(&rel) else { continue };
        if !referenced.contains(&twin) {
            continue;
        }
        if started.elapsed() >= BUDGET {
            log.line("duplicate backups: time is up, the rest next time");
            break;
        }
        match is_redundant(&raw.join(&rel), &raw.join(&twin)) {
            Ok(true) => {
                if let Err(message) = remove(&raw.join(&rel)) {
                    log.line(&format!("duplicate backup {rel}: cannot remove: {message}"));
                    continue;
                }
                log.line(&format!(
                    "removed duplicate backup {rel} ({size} bytes): identical to the start of {twin}"
                ));
                removed.files += 1;
                removed.bytes += size;
            }
            Ok(false) => log.line(&format!(
                "kept {rel}: it is not contained in {twin}, so both stay"
            )),
            Err(message) => log.line(&format!(
                "kept {rel}: cannot compare with {twin}: {message}"
            )),
        }
    }
    Ok(removed)
}

/// The same on the backup disk. The disk keeps every file the main directory
/// once had, so a plain copy remains there after the main one was removed.
/// It goes only when the main directory no longer has it, has the `.zst`
/// twin, and the disk's own twin is complete (same size) and holds all of it.
pub fn clean_disk(
    data: &DataPaths,
    disk_raw: &Path,
    log: &SyncLog,
    mut ensure_on_disk: impl FnMut(&Path) -> Result<(), String>,
) -> Removed {
    let started = Instant::now();
    let local = data.raw();
    let mut removed = Removed::default();
    for (rel, size) in fsutil::walk_files(disk_raw) {
        let Some(twin) = twin_of(&rel) else { continue };
        let (disk_plain, disk_twin) = (disk_raw.join(&rel), disk_raw.join(&twin));
        let (local_plain, local_twin) = (local.join(&rel), local.join(&twin));
        if local_plain.exists() {
            continue;
        }
        let same_size = match (
            std::fs::metadata(&local_twin),
            std::fs::metadata(&disk_twin),
        ) {
            (Ok(a), Ok(b)) => a.is_file() && b.is_file() && a.len() == b.len(),
            _ => false,
        };
        if !same_size {
            continue;
        }
        if started.elapsed() >= BUDGET {
            log.line("duplicate backups on the disk: time is up, the rest next time");
            break;
        }
        if let Err(message) = ensure_on_disk(&disk_plain) {
            log.line(&format!("duplicate backup {rel} on the disk: {message}"));
            break;
        }
        match is_redundant(&disk_plain, &disk_twin) {
            Ok(true) => match remove(&disk_plain) {
                Ok(()) => {
                    log.line(&format!(
                        "removed duplicate backup {rel} ({size} bytes) from the disk: contained in {twin}"
                    ));
                    removed.files += 1;
                    removed.bytes += size;
                }
                Err(message) => {
                    log.line(&format!(
                        "duplicate backup {rel} on the disk: cannot remove: {message}"
                    ));
                }
            },
            Ok(false) => log.line(&format!(
                "kept {rel} on the disk: it is not contained in {twin}, so both stay"
            )),
            Err(message) => log.line(&format!(
                "kept {rel} on the disk: cannot compare with {twin}: {message}"
            )),
        }
    }
    removed
}

fn remove(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        fsutil::fsync_dir(parent);
    }
    Ok(())
}

/// Add to the running totals and to what the user has not been told about yet.
pub fn record(conn: &Connection, removed: Removed) -> Result<(), String> {
    if removed.files == 0 {
        return Ok(());
    }
    for (total, unseen, value) in [
        (META_TOTAL_FILES, META_UNSEEN_FILES, removed.files),
        (META_TOTAL_BYTES, META_UNSEEN_BYTES, removed.bytes),
    ] {
        for key in [total, unseen] {
            let before: u64 = db::get_meta(conn, key)?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            db::set_meta(conn, key, &before.saturating_add(value).to_string())?;
        }
    }
    Ok(())
}

/// What was cleaned since the user last dismissed the notice.
pub fn unseen(conn: &Connection) -> Option<Removed> {
    let get = |key: &str| -> u64 {
        db::get_meta(conn, key)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let files = get(META_UNSEEN_FILES);
    (files > 0).then(|| Removed {
        files,
        bytes: get(META_UNSEEN_BYTES),
    })
}

pub fn dismiss(conn: &Connection) -> Result<(), String> {
    db::set_meta(conn, META_UNSEEN_FILES, "0")?;
    db::set_meta(conn, META_UNSEEN_BYTES, "0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn zst(path: &Path, text: &str) {
        fs::write(path, zstd::encode_all(text.as_bytes(), 3).unwrap()).unwrap();
    }

    #[test]
    fn equal_and_shorter_plain_copies_are_redundant() {
        let dir = tempfile::tempdir().unwrap();
        let (plain, packed) = (dir.path().join("x.jsonl"), dir.path().join("x.jsonl.zst"));
        zst(&packed, "one\ntwo\nthree\n");
        fs::write(&plain, "one\ntwo\nthree\n").unwrap();
        assert!(is_redundant(&plain, &packed).unwrap());
        fs::write(&plain, "one\ntwo\n").unwrap();
        assert!(is_redundant(&plain, &packed).unwrap(), "a prefix");
    }

    #[test]
    fn different_longer_empty_or_unreadable_is_not_redundant() {
        let dir = tempfile::tempdir().unwrap();
        let (plain, packed) = (dir.path().join("x.jsonl"), dir.path().join("x.jsonl.zst"));
        zst(&packed, "one\ntwo\n");
        fs::write(&plain, "one\nTWO\n").unwrap();
        assert!(!is_redundant(&plain, &packed).unwrap(), "different");
        fs::write(&plain, "one\ntwo\nthree\n").unwrap();
        assert!(
            !is_redundant(&plain, &packed).unwrap(),
            "longer than the twin"
        );
        fs::write(&plain, "").unwrap();
        assert!(
            !is_redundant(&plain, &packed).unwrap(),
            "empty proves nothing"
        );
        fs::write(&plain, "one\ntwo\n").unwrap();
        fs::write(&packed, b"not zstd data at all").unwrap();
        assert!(
            !is_redundant(&plain, &packed).unwrap_or(false),
            "damaged twin"
        );
        let whole = zstd::encode_all(&b"one\ntwo\nthree\n"[..], 3).unwrap();
        fs::write(&packed, &whole[..whole.len() - 5]).unwrap();
        fs::write(&plain, "one\ntwo\nthree\n").unwrap();
        assert!(
            !is_redundant(&plain, &packed).unwrap_or(false),
            "a cut-off twin does not vouch for the whole file"
        );
    }

    #[test]
    fn only_the_jsonl_with_a_zst_twin_is_a_candidate() {
        assert_eq!(twin_of("a/b.jsonl").as_deref(), Some("a/b.jsonl.zst"));
        assert_eq!(twin_of("a/b.jsonl.zst"), None);
        assert_eq!(twin_of("a/b.jsonl.prev-20260101T000000Z"), None);
        assert_eq!(twin_of("a/b.jsonl.conflict-20260101T000000Z"), None);
    }

    #[test]
    fn totals_and_the_unseen_notice() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("kb.db")).unwrap();
        assert_eq!(unseen(&conn), None);
        record(&conn, Removed { files: 0, bytes: 0 }).unwrap();
        assert_eq!(unseen(&conn), None);
        record(
            &conn,
            Removed {
                files: 2,
                bytes: 100,
            },
        )
        .unwrap();
        record(
            &conn,
            Removed {
                files: 1,
                bytes: 50,
            },
        )
        .unwrap();
        assert_eq!(
            unseen(&conn),
            Some(Removed {
                files: 3,
                bytes: 150
            })
        );
        dismiss(&conn).unwrap();
        assert_eq!(unseen(&conn), None);
        record(&conn, Removed { files: 1, bytes: 7 }).unwrap();
        assert_eq!(unseen(&conn), Some(Removed { files: 1, bytes: 7 }));
        assert_eq!(
            db::get_meta(&conn, META_TOTAL_FILES).unwrap().as_deref(),
            Some("4")
        );
        assert_eq!(
            db::get_meta(&conn, META_TOTAL_BYTES).unwrap().as_deref(),
            Some("157")
        );
    }
}
