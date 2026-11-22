//! Duplicate backups: the same content stored under two names in `raw/`.
//!
//! A backup changes its name when it is stored another way, and the old name
//! can stay behind with nothing referring to it:
//!
//! | left behind (no row) | where the row points | how |
//! |---|---|---|
//! | `X.jsonl` | `X.jsonl.zst` | Codex compressed the source in place |
//! | `X.jsonl` | `X.jsonl.packed.zst` | we compressed the backup, removing the plain one failed or was cut short |
//! | `X.jsonl.packed.zst` | `X.jsonl` | we unpacked it for new messages, same |
//! | `X.jsonl.packed.zst` | `X.jsonl.zst` | Codex compressed the source after we had compressed the backup |
//! | `X.jsonl.zst` | `X.jsonl` or `X.jsonl.packed.zst` | the local source is plain after restoring or unpacking a Codex rollout |
//!
//! The one left behind holds nothing the other does not: this module removes
//! it, but only after proving that every byte of it is the start of (or all
//! of) what the other holds, each read as plain text. If the proof fails or
//! cannot be made (different content, a damaged file, a read error) both stay.
//! A file that cannot be removed (Windows: someone has it open) stays too and
//! is tried again with the next collection.
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
use super::shrink::PACKED_SUFFIX;

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

/// How a backup file is stored, told by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// `X.jsonl`
    Plain,
    /// `X.jsonl.packed.zst`: compressed by us, the source is plain
    Packed,
    /// `X.jsonl.zst`: the source itself is compressed (Codex)
    Codex,
}

impl Storage {
    pub fn is_zst(self) -> bool {
        self != Self::Plain
    }
}

/// The plain name a backup name stands for, and how it is stored. `None` for
/// anything else (`.prev-*` and `.conflict-*` files are kept sets of older
/// content, temp files are not content).
#[allow(clippy::case_sensitive_file_extension_comparisons)] // exact names: the agents write `.jsonl`
pub fn storage_of(rel: &str) -> Option<(&str, Storage)> {
    if let Some(plain) = rel.strip_suffix(PACKED_SUFFIX) {
        return plain
            .ends_with(".jsonl")
            .then_some((plain, Storage::Packed));
    }
    if let Some(plain) = rel.strip_suffix(".zst") {
        return plain.ends_with(".jsonl").then_some((plain, Storage::Codex));
    }
    rel.ends_with(".jsonl").then_some((rel, Storage::Plain))
}

fn name_for(plain: &str, storage: Storage) -> String {
    match storage {
        Storage::Plain => plain.to_string(),
        Storage::Packed => format!("{plain}{PACKED_SUFFIX}"),
        Storage::Codex => format!("{plain}.zst"),
    }
}

/// The names that may hold all of what the left-behind `rel` holds (the
/// table above, read from the left column).
fn twins_of(rel: &str) -> Vec<(String, Storage)> {
    let wanted: &[Storage] = match storage_of(rel) {
        Some((_, Storage::Plain)) => &[Storage::Codex, Storage::Packed],
        Some((_, Storage::Packed)) => &[Storage::Plain, Storage::Codex],
        Some((_, Storage::Codex)) => &[Storage::Plain, Storage::Packed],
        _ => &[],
    };
    let Some((plain, _)) = storage_of(rel) else {
        return Vec::new();
    };
    wanted.iter().map(|s| (name_for(plain, *s), *s)).collect()
}

/// The other names under which the content of the backup `rel` may be stored
/// after the cleanup removed `rel` itself. A database snapshot taken before
/// that still points to `rel`.
pub fn replacements_of(rel: &str) -> Vec<(String, Storage)> {
    let wanted: &[Storage] = match storage_of(rel) {
        Some((_, Storage::Plain)) => &[Storage::Packed, Storage::Codex],
        Some((_, Storage::Packed)) => &[Storage::Plain, Storage::Codex],
        _ => &[],
    };
    let Some((plain, _)) = storage_of(rel) else {
        return Vec::new();
    };
    wanted.iter().map(|s| (name_for(plain, *s), *s)).collect()
}

/// Whether `plain` (as stored) is the beginning of, or all of, what `zst`
/// decompresses to. An empty `plain` proves nothing and is kept.
pub fn is_redundant(plain: &Path, zst: &Path) -> Result<bool, String> {
    content_continues((plain, false, None), (zst, true))
}

/// Whether the left-behind `a` holds nothing that its twin `b` does not.
fn is_contained(a: &Path, a_rel: &str, b: &Path, b_storage: Storage) -> Result<bool, String> {
    let a_zst = storage_of(a_rel).is_some_and(|(_, s)| s.is_zst());
    content_continues((a, a_zst, None), (b, b_storage.is_zst()))
}

/// Remove the backups in `raw/` that no database row points to and whose
/// twin is referenced and proves to hold all of their content.
pub fn clean_raw(conn: &Connection, data: &DataPaths, log: &SyncLog) -> Result<Removed, String> {
    let started = Instant::now();
    let referenced: HashSet<String> = db::load_files(conn)?
        .into_iter()
        .map(|row| row.mirror_rel)
        .collect();
    let raw = data.raw();
    let mut removed = Removed::default();
    'files: for (rel, size) in fsutil::walk_files(&raw) {
        if referenced.contains(&rel) {
            continue;
        }
        for (twin, storage) in twins_of(&rel) {
            if !referenced.contains(&twin) {
                continue;
            }
            if started.elapsed() >= BUDGET {
                log.line("duplicate backups: time is up, the rest next time");
                break 'files;
            }
            match is_contained(&raw.join(&rel), &rel, &raw.join(&twin), storage) {
                Ok(true) => {
                    if let Err(message) = remove(&raw.join(&rel)) {
                        log.line(&format!(
                            "duplicate backup {rel}: cannot remove, next time again: {message}"
                        ));
                        continue 'files;
                    }
                    log.line(&format!(
                        "removed duplicate backup {rel} ({size} bytes): contained in {twin}"
                    ));
                    removed.files += 1;
                    removed.bytes += size;
                    continue 'files;
                }
                Ok(false) => log.line(&format!(
                    "kept {rel}: it is not contained in {twin}, so both stay"
                )),
                Err(message) => log.line(&format!(
                    "kept {rel}: cannot compare with {twin}: {message}"
                )),
            }
        }
    }
    Ok(removed)
}

/// The same on the backup disk. The disk keeps every file the main directory
/// once had, so a copy under the old name remains there after the main one
/// was removed. It goes only when the main directory no longer has it, has
/// the twin, and the disk's own twin is complete (same size) and holds all of it.
pub fn clean_disk(
    data: &DataPaths,
    disk_raw: &Path,
    log: &SyncLog,
    mut ensure_on_disk: impl FnMut(&Path) -> Result<(), String>,
) -> Removed {
    let started = Instant::now();
    let local = data.raw();
    let mut removed = Removed::default();
    'files: for (rel, size) in fsutil::walk_files(disk_raw) {
        if local.join(&rel).exists() {
            continue;
        }
        for (twin, storage) in twins_of(&rel) {
            let (disk_old, disk_twin) = (disk_raw.join(&rel), disk_raw.join(&twin));
            let same_size = match (
                std::fs::metadata(local.join(&twin)),
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
                break 'files;
            }
            if let Err(message) = ensure_on_disk(&disk_old) {
                log.line(&format!("duplicate backup {rel} on the disk: {message}"));
                break 'files;
            }
            match is_contained(&disk_old, &rel, &disk_twin, storage) {
                Ok(true) => {
                    match remove(&disk_old) {
                        Ok(()) => {
                            log.line(&format!(
                                "removed duplicate backup {rel} ({size} bytes) from the disk: contained in {twin}"
                            ));
                            removed.files += 1;
                            removed.bytes += size;
                        }
                        Err(message) => log.line(&format!(
                            "duplicate backup {rel} on the disk: cannot remove: {message}"
                        )),
                    }
                    continue 'files;
                }
                Ok(false) => log.line(&format!(
                    "kept {rel} on the disk: it is not contained in {twin}, so both stay"
                )),
                Err(message) => log.line(&format!(
                    "kept {rel} on the disk: cannot compare with {twin}: {message}"
                )),
            }
        }
    }
    removed
}

fn remove(path: &Path) -> Result<(), String> {
    fsutil::remove_synced(path)
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
    fn the_twins_of_each_left_behind_name() {
        let names = |rel: &str| -> Vec<String> { twins_of(rel).into_iter().map(|t| t.0).collect() };
        assert_eq!(
            names("a/b.jsonl"),
            ["a/b.jsonl.zst", "a/b.jsonl.packed.zst"]
        );
        assert_eq!(
            names("a/b.jsonl.packed.zst"),
            ["a/b.jsonl", "a/b.jsonl.zst"]
        );
        assert_eq!(
            names("a/b.jsonl.zst"),
            ["a/b.jsonl", "a/b.jsonl.packed.zst"]
        );
        assert_eq!(
            names("a/b.jsonl.prev-20260101T000000Z"),
            Vec::<String>::new()
        );
        assert_eq!(
            names("a/b.jsonl.conflict-20260101T000000Z"),
            Vec::<String>::new()
        );
        assert_eq!(names("a/b.jsonl.packed.zst.tmp-123"), Vec::<String>::new());
        assert_eq!(names("a/b.txt.packed.zst"), Vec::<String>::new());
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
