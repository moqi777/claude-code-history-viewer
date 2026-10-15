//! Moving the data directory.
//!
//! The old directory is never modified or deleted. Everything is copied into
//! the new one, verified byte for byte (SHA-256), and only then does
//! `config.json` switch over, atomically. A failure at any earlier point leaves
//! the configuration pointing at the untouched old directory; the half-filled
//! new directory carries a marker so a retry resumes instead of refusing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::Serialize;

use super::backup::online_backup;
use super::config::{KbConfig, SourceRoots};
use super::lock::SyncLock;
use super::paths::DataPaths;
use super::progress::{Progress, ProgressSink};
use super::{db, fsutil};

pub const MARKER: &str = ".agentkb-migrating";
pub const REPORT: &str = "migration-report.json";
/// A migration may legitimately wait for a running collection to finish.
const LOCK_WAIT: Duration = Duration::from_secs(60);
const SPACE_MARGIN_PERCENT: u64 = 110;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    /// Missing or empty: a normal migration.
    Empty,
    /// Holds our marker: an earlier attempt was interrupted; continue it.
    Resume,
    /// Already an `AgentKB` data directory (has `kb.db`): can be adopted as is.
    ExistingData,
    /// Something else lives there; refuse rather than mix.
    NotEmpty,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MigrateCheck {
    pub target_state: TargetState,
    pub needed_bytes: u64,
    pub free_bytes: Option<u64>,
    /// Machine-readable reason the move cannot go ahead, if any.
    pub problem: Option<String>,
}

impl MigrateCheck {
    pub fn ok(&self) -> bool {
        self.problem.is_none()
    }
}

fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |mut entries| entries.next().is_none())
}

pub fn check(old: &Path, new: &Path, roots: &SourceRoots) -> MigrateCheck {
    let needed = fsutil::dir_size(old);
    let free = fsutil::free_space(new);
    let target_state = if !new.exists() || dir_is_empty(new) {
        TargetState::Empty
    } else if new.join(MARKER).exists() {
        TargetState::Resume
    } else if new.join("kb.db").is_file() {
        TargetState::ExistingData
    } else {
        TargetState::NotEmpty
    };
    let problem = if !new.is_absolute() {
        Some("not_absolute")
    } else if new == old || new.starts_with(old) || old.starts_with(new) {
        Some("nested")
    } else if [&roots.claude, &roots.codex]
        .into_iter()
        .flatten()
        .any(|root| new.starts_with(root) || root.starts_with(new))
    {
        Some("overlaps_source")
    } else if new.exists() && !new.is_dir() {
        Some("not_a_folder")
    } else if target_state == TargetState::NotEmpty {
        Some("not_empty")
    } else if free.is_some_and(|f| f < needed / 100 * SPACE_MARGIN_PERCENT) {
        Some("not_enough_space")
    } else {
        None
    };
    MigrateCheck {
        target_state,
        needed_bytes: needed,
        free_bytes: free,
        problem: problem.map(str::to_owned),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MigrationReport {
    pub old_dir: String,
    pub new_dir: String,
    pub files: u64,
    pub bytes: u64,
    pub verified_files: u64,
    pub duration_ms: u64,
}

struct Meter<'a> {
    sink: &'a ProgressSink,
    phase: &'static str,
    total_files: u64,
    total_bytes: u64,
    done_files: u64,
    done_bytes: u64,
}

impl Meter<'_> {
    fn tick(&self) {
        self.sink.emit(&Progress {
            op: "migrate".into(),
            phase: self.phase.into(),
            done: self.done_files,
            total: self.total_files,
            bytes_done: self.done_bytes,
            bytes_total: self.total_bytes,
        });
    }
}

/// A stable fingerprint of the logical content of `kb.db` (not its bytes: the
/// copy is made with `SQLite`'s backup API and legitimately differs on disk).
fn db_fingerprint(path: &Path) -> Result<String, String> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| format!("integrity check failed to run: {e}"))?;
    if verdict != "ok" {
        return Err(format!(
            "{} failed its integrity check: {verdict}",
            path.display()
        ));
    }
    let mut parts = Vec::new();
    for table in ["meta", "files", "sessions", "sync_runs"] {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .map_err(|e| format!("cannot count {table}: {e}"))?;
        parts.push(format!("{table}={n}"));
    }
    let mut stmt = conn
        .prepare("SELECT id, source_path, mirror_rel, mirror_offset FROM files ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}",
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        parts.push(row.map_err(|e| e.to_string())?);
    }
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(parts.join("\n").as_bytes());
    Ok(digest.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

fn copy_tree(
    from_root: &Path,
    to_root: &Path,
    meter: &mut Meter<'_>,
) -> Result<(u64, u64), String> {
    let mut files = 0;
    let mut bytes = 0;
    for (rel, size) in fsutil::walk_files(from_root) {
        let dst = to_root.join(&rel);
        let have = std::fs::metadata(&dst).map(|m| m.len()).ok();
        if have != Some(size) {
            fsutil::copy_atomic(&from_root.join(&rel), &dst)?;
        }
        files += 1;
        bytes += size;
        meter.done_files += 1;
        meter.done_bytes += size;
        meter.tick();
    }
    Ok((files, bytes))
}

fn verify_tree(from_root: &Path, to_root: &Path, meter: &mut Meter<'_>) -> Result<u64, String> {
    let mut checked = 0;
    for (rel, size) in fsutil::walk_files(from_root) {
        let dst = to_root.join(&rel);
        let theirs = std::fs::metadata(&dst)
            .map_err(|e| format!("{rel} is missing in the new location: {e}"))?;
        if theirs.len() != size {
            return Err(format!(
                "{rel} differs in size ({size} vs {})",
                theirs.len()
            ));
        }
        let a = fsutil::sha256_file(&from_root.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
        let b = fsutil::sha256_file(&dst).map_err(|e| format!("{rel}: {e}"))?;
        if a != b {
            return Err(format!("{rel} differs in content after copying"));
        }
        checked += 1;
        meter.done_files += 1;
        meter.done_bytes += size;
        meter.tick();
    }
    Ok(checked)
}

/// Copy and verify. Does not touch `config.json`.
pub fn migrate_data(
    old: &Path,
    new: &Path,
    progress: &ProgressSink,
) -> Result<MigrationReport, String> {
    let started = Instant::now();
    let old_paths = DataPaths::new(old);
    // Held for the whole move: a collection writing into `old` meanwhile would
    // not be in the copy.
    let Some(_lock) = SyncLock::acquire(&old_paths.lock(), LOCK_WAIT)? else {
        return Err("a collection is still running; try again in a moment".into());
    };
    // So is a digest writing notes: they must be in the copy too.
    let Some(_digest_lock) = SyncLock::acquire(&old_paths.digest_lock(), LOCK_WAIT)? else {
        return Err("a digest is still running; try again in a moment".into());
    };

    std::fs::create_dir_all(new).map_err(|e| format!("cannot create {}: {e}", new.display()))?;
    let marker = new.join(MARKER);
    if !marker.exists() {
        let note = serde_json::json!({
            "from": old.display().to_string(),
            "started_at": chrono::Utc::now().to_rfc3339(),
        });
        fsutil::write_atomic(&marker, note.to_string().as_bytes())?;
    }

    let old_raw = old_paths.raw();
    let new_raw = DataPaths::new(new).raw();
    let old_notes = old_paths.notes();
    let new_notes = DataPaths::new(new).notes();
    let total_files = (fsutil::walk_files(&old_raw).len()
        + fsutil::walk_files(&old_notes).len()
        + fsutil::walk_files(&old_paths.logs()).len()) as u64;
    let total_bytes = fsutil::dir_size(&old_raw)
        + fsutil::dir_size(&old_notes)
        + fsutil::dir_size(&old_paths.logs());
    let mut meter = Meter {
        sink: progress,
        phase: "copy",
        total_files,
        total_bytes,
        done_files: 0,
        done_bytes: 0,
    };
    meter.tick();
    let (mut files, mut bytes) = copy_tree(&old_raw, &new_raw, &mut meter)?;
    // Notes cannot be regenerated for free, so they travel with the raw logs.
    let (note_files, note_bytes) = copy_tree(&old_notes, &new_notes, &mut meter)?;
    files += note_files;
    bytes += note_bytes;
    let (log_files, log_bytes) =
        copy_tree(&old_paths.logs(), &DataPaths::new(new).logs(), &mut meter)?;
    files += log_files;
    bytes += log_bytes;

    // The database: a consistent snapshot, never a raw file copy.
    let new_db = DataPaths::new(new).db();
    let has_db = old_paths.db().is_file();
    if has_db {
        let conn = db::open(&old_paths.db())?;
        let tmp = new.join(format!("kb.db.tmp-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        online_backup(&conn, &tmp)?;
        std::fs::rename(&tmp, &new_db).map_err(|e| format!("cannot place kb.db: {e}"))?;
        for side in ["kb.db-wal", "kb.db-shm"] {
            let _ = std::fs::remove_file(new.join(side));
        }
    }

    // Prove it.
    meter.phase = "verify";
    meter.done_files = 0;
    meter.done_bytes = 0;
    let mut verified = verify_tree(&old_raw, &new_raw, &mut meter)?;
    verified += verify_tree(&old_notes, &new_notes, &mut meter)?;
    verified += verify_tree(&old_paths.logs(), &DataPaths::new(new).logs(), &mut meter)?;
    if has_db {
        let a = db_fingerprint(&old_paths.db())?;
        let b = db_fingerprint(&new_db)?;
        if a != b {
            return Err("kb.db differs after copying".into());
        }
        verified += 1;
    }

    let report = MigrationReport {
        old_dir: old.display().to_string(),
        new_dir: new.display().to_string(),
        files: files + u64::from(has_db),
        bytes,
        verified_files: verified,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    fsutil::write_atomic(&new.join(REPORT), json.as_bytes())?;
    let _ = std::fs::remove_file(&marker);
    Ok(report)
}

/// Move the data and, only if everything verified, switch `config_file` to the
/// new directory. The caller reinstalls the schedule afterwards.
pub fn apply(
    config_file: &Path,
    config: &KbConfig,
    new: &Path,
    progress: &ProgressSink,
) -> Result<MigrationReport, String> {
    let roots = SourceRoots::resolve(config);
    let verdict = check(&config.data_dir, new, &roots);
    if let Some(problem) = verdict.problem {
        return Err(format!("cannot move the data directory: {problem}"));
    }
    if verdict.target_state == TargetState::ExistingData {
        return Err("the target already holds AgentKB data; use it as is instead".into());
    }
    let report = migrate_data(&config.data_dir, new, progress)?;
    let mut next = config.clone();
    next.data_dir = new.to_path_buf();
    next.save_to(config_file)?;
    Ok(report)
}

/// Adopt a directory that already is an `AgentKB` data directory, without copying.
pub fn adopt_existing(config_file: &Path, config: &KbConfig, dir: &Path) -> Result<(), String> {
    let found = super::probe::test_data_dir(dir, &SourceRoots::resolve(config));
    if found.db_too_new {
        return Err("that data directory was written by a newer AgentKB".into());
    }
    if found.existing.is_none() {
        return Err("that folder holds no AgentKB data".into());
    }
    let mut next = config.clone();
    next.data_dir = dir.to_path_buf();
    next.save_to(config_file)
}

/// Where a `PathBuf` argument from the UI is expanded (`~/…`).
pub fn expand(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    match path.strip_prefix("~/") {
        Some(rest) => crate::utils::home_dir().map_or(p, |h| h.join(rest)),
        None => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Rig {
        _tmp: tempfile::TempDir,
        old: PathBuf,
        new: PathBuf,
        config_file: PathBuf,
        config: KbConfig,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CCHV_TEST_HOME", tmp.path());
        let old = tmp.path().join("old/AgentKB");
        let new = tmp.path().join("new/AgentKB");
        fs::create_dir_all(old.join("raw/claude/p")).unwrap();
        fs::write(old.join("raw/claude/p/a.jsonl"), b"alpha\nbeta\n").unwrap();
        fs::write(old.join("raw/claude/p/a.jsonl.prev-1"), b"older\n").unwrap();
        fs::create_dir_all(old.join("notes/claude")).unwrap();
        fs::write(old.join("notes/claude/s-1.json"), b"{\"note\":true}\n").unwrap();
        fs::create_dir_all(old.join("logs")).unwrap();
        fs::write(old.join("logs/sync-1.log"), b"log\n").unwrap();
        let conn = db::open(&old.join("kb.db")).unwrap();
        db::set_meta(&conn, "marker", "v1").unwrap();
        drop(conn);
        let config = KbConfig {
            data_dir: old.clone(),
            ..KbConfig::default()
        };
        let config_file = tmp.path().join("cfg/config.json");
        config.save_to(&config_file).unwrap();
        Rig {
            _tmp: tmp,
            old,
            new,
            config_file,
            config,
        }
    }

    fn snapshot(dir: &Path) -> Vec<(String, String)> {
        fsutil::walk_files(dir)
            .into_iter()
            .filter(|(n, _)| !n.starts_with("kb.db-") && n != "kb.lock" && n != "digest.lock")
            .map(|(n, _)| {
                let h = fsutil::sha256_file(&dir.join(&n)).unwrap();
                (n, h)
            })
            .collect()
    }

    #[test]
    fn a_good_move_copies_verifies_switches_the_config_and_leaves_the_old_dir_alone() {
        let rig = rig();
        let old_before = snapshot(&rig.old);
        let report = apply(
            &rig.config_file,
            &rig.config,
            &rig.new,
            &ProgressSink::default(),
        )
        .unwrap();
        assert!(report.verified_files >= 5, "{report:?}");

        // Raw files are byte-identical.
        for rel in [
            "raw/claude/p/a.jsonl",
            "raw/claude/p/a.jsonl.prev-1",
            "notes/claude/s-1.json",
            "logs/sync-1.log",
        ] {
            assert_eq!(
                fs::read(rig.old.join(rel)).unwrap(),
                fs::read(rig.new.join(rel)).unwrap()
            );
        }
        let moved = Connection::open(rig.new.join("kb.db")).unwrap();
        assert_eq!(
            db::get_meta(&moved, "marker").unwrap().as_deref(),
            Some("v1")
        );
        assert!(!rig.new.join(MARKER).exists());
        assert!(rig.new.join(REPORT).is_file());

        // The configuration now points at the new directory...
        assert_eq!(
            KbConfig::load_from(&rig.config_file).unwrap().data_dir,
            rig.new
        );
        // ...and the old one is exactly as it was.
        assert_eq!(snapshot(&rig.old), old_before);
    }

    #[test]
    fn a_failed_copy_leaves_the_config_on_the_old_directory() {
        let rig = rig();
        // Make the new `raw` a file so copying into it fails.
        fs::create_dir_all(&rig.new).unwrap();
        fs::write(rig.new.join("raw"), b"in the way").unwrap();
        // `check` calls this directory non-empty, so go through migrate_data's
        // own path via a marker that makes it a resumable target.
        fs::write(rig.new.join(MARKER), b"{}").unwrap();
        let err = apply(
            &rig.config_file,
            &rig.config,
            &rig.new,
            &ProgressSink::default(),
        )
        .unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(
            KbConfig::load_from(&rig.config_file).unwrap().data_dir,
            rig.old
        );
        assert!(rig.old.join("raw/claude/p/a.jsonl").is_file());
    }

    #[test]
    fn verification_catches_a_single_flipped_byte() {
        let rig = rig();
        migrate_data(&rig.old, &rig.new, &ProgressSink::default()).unwrap();
        let victim = rig.new.join("raw/claude/p/a.jsonl");
        let mut bytes = fs::read(&victim).unwrap();
        bytes[0] ^= 0xff;
        fs::write(&victim, bytes).unwrap();
        let sink = ProgressSink::default();
        let mut meter = Meter {
            sink: &sink,
            phase: "verify",
            total_files: 0,
            total_bytes: 0,
            done_files: 0,
            done_bytes: 0,
        };
        let err = verify_tree(&rig.old.join("raw"), &rig.new.join("raw"), &mut meter).unwrap_err();
        assert!(err.contains("differs"), "{err}");
    }

    #[test]
    fn an_interrupted_move_resumes_and_completes() {
        let rig = rig();
        // First attempt died after one file.
        fs::create_dir_all(rig.new.join("raw/claude/p")).unwrap();
        fs::copy(
            rig.old.join("raw/claude/p/a.jsonl"),
            rig.new.join("raw/claude/p/a.jsonl"),
        )
        .unwrap();
        fs::write(rig.new.join(MARKER), b"{}").unwrap();
        assert_eq!(
            check(&rig.old, &rig.new, &SourceRoots::default()).target_state,
            TargetState::Resume
        );
        apply(
            &rig.config_file,
            &rig.config,
            &rig.new,
            &ProgressSink::default(),
        )
        .unwrap();
        assert_eq!(
            fs::read(rig.new.join("raw/claude/p/a.jsonl.prev-1")).unwrap(),
            b"older\n"
        );
        assert!(!rig.new.join(MARKER).exists());
    }

    #[test]
    fn unsuitable_targets_are_refused_before_anything_is_copied() {
        let rig = rig();
        let roots = SourceRoots::default();
        // Inside the old directory.
        assert_eq!(
            check(&rig.old, &rig.old.join("sub"), &roots)
                .problem
                .as_deref(),
            Some("nested")
        );
        // A non-empty, unrelated folder.
        fs::create_dir_all(&rig.new).unwrap();
        fs::write(rig.new.join("holiday.jpg"), b"x").unwrap();
        assert_eq!(
            check(&rig.old, &rig.new, &roots).problem.as_deref(),
            Some("not_empty")
        );
        assert!(apply(
            &rig.config_file,
            &rig.config,
            &rig.new,
            &ProgressSink::default()
        )
        .is_err());
        assert!(
            rig.new.join("holiday.jpg").is_file(),
            "their files are untouched"
        );
        // Overlapping a source folder.
        let roots = SourceRoots {
            claude: Some(PathBuf::from("/src/claude")),
            codex: None,
        };
        assert_eq!(
            check(&rig.old, Path::new("/src/claude/kb"), &roots)
                .problem
                .as_deref(),
            Some("overlaps_source")
        );
        // An existing AgentKB directory is adoptable, not copy-over-able.
        let adopt = rig.old.parent().unwrap().join("other");
        fs::create_dir_all(&adopt).unwrap();
        db::open(&adopt.join("kb.db")).unwrap();
        assert_eq!(
            check(&rig.old, &adopt, &SourceRoots::default()).target_state,
            TargetState::ExistingData
        );
        adopt_existing(&rig.config_file, &rig.config, &adopt).unwrap();
        assert_eq!(
            KbConfig::load_from(&rig.config_file).unwrap().data_dir,
            adopt
        );
    }

    #[test]
    fn progress_is_reported() {
        let rig = rig();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Progress>::new()));
        let sink = {
            let seen = seen.clone();
            ProgressSink::new(move |p| seen.lock().unwrap().push(p.clone()))
        };
        migrate_data(&rig.old, &rig.new, &sink).unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|p| p.phase == "copy"));
        assert!(seen.iter().any(|p| p.phase == "verify" && p.done > 0));
    }
}
