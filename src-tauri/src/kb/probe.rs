//! The "test" buttons of the setup wizard and the settings page. Each check
//! answers one question with a typed result the UI turns into a plain sentence
//! ("writable, 120 GB free"), and none of them changes anything that matters:
//! at most an empty probe file that is removed again.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use super::backup_dir;
use super::config::{KbConfig, SourceRoots};
use super::schedule::{self, ExeStability};
use super::volume::{self, Probe, SystemQuery, Unsupported, VolumeInfo};
use super::{db, discover, fsutil, Source};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceProbe {
    pub source: &'static str,
    pub root: Option<String>,
    pub exists: bool,
    pub sessions: u64,
    pub bytes: u64,
}

/// What is on this machine, regardless of whether a source is switched on.
pub fn probe_sources(config: &KbConfig) -> Vec<SourceProbe> {
    let mut all_on = config.clone();
    all_on.sources.claude.enabled = true;
    all_on.sources.codex.enabled = true;
    let roots = SourceRoots::resolve(&all_on);
    let found = discover::discover(&roots, &config.exclude_prefixes);
    [
        (Source::Claude, &roots.claude),
        (Source::Codex, &roots.codex),
    ]
    .into_iter()
    .map(|(source, root)| {
        let files = found.files.iter().filter(|f| f.source == source);
        SourceProbe {
            source: source.as_str(),
            root: root.as_ref().map(|p| p.display().to_string()),
            exists: root.as_ref().is_some_and(|p| p.is_dir()),
            sessions: files.clone().count() as u64,
            bytes: files.map(|f| f.size).sum(),
        }
    })
    .collect()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExistingData {
    pub sessions: u64,
    pub bytes: u64,
    pub has_db: bool,
    pub has_raw: bool,
}

#[allow(clippy::struct_excessive_bools)] // a flat report the UI reads field by field
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DataDirReport {
    pub path: String,
    pub exists: bool,
    /// Does not exist yet but can be created.
    pub will_create: bool,
    pub writable: bool,
    pub free_bytes: Option<u64>,
    pub existing: Option<ExistingData>,
    /// `kb.db` was written by a newer `AgentKB`.
    pub db_too_new: bool,
    /// The folder overlaps a source folder (rejected).
    pub overlaps_source: Option<String>,
    pub error: Option<String>,
}

/// Can we create and delete a file in `dir` (or, if it does not exist yet, in
/// the nearest existing parent)?
fn writable(dir: &Path) -> Result<(), String> {
    let anchor = fsutil::nearest_existing(dir).ok_or("no existing parent folder")?;
    let anchor = if anchor.is_dir() {
        anchor
    } else {
        anchor
            .parent()
            .map(Path::to_path_buf)
            .ok_or("no parent folder")?
    };
    let probe = anchor.join(format!(".agentkb-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"x").map_err(|e| format!("{}: {e}", anchor.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn existing_data(dir: &Path) -> (Option<ExistingData>, bool) {
    let db_path = dir.join("kb.db");
    let has_db = db_path.is_file();
    let has_raw = dir.join("raw").is_dir();
    if !has_db && !has_raw {
        return (None, false);
    }
    let mut sessions = 0;
    let mut too_new = false;
    if has_db {
        if let Ok(Some(conn)) = db::open_readonly(&db_path) {
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap_or(0);
            too_new = version > db::latest_version();
            if !too_new {
                sessions = count_sessions(&conn);
            }
        }
    }
    (
        Some(ExistingData {
            sessions,
            bytes: fsutil::dir_size(dir),
            has_db,
            has_raw,
        }),
        too_new,
    )
}

fn count_sessions(conn: &Connection) -> u64 {
    conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
        .map_or(0, |n| u64::try_from(n).unwrap_or(0))
}

pub fn test_data_dir(path: &Path, roots: &SourceRoots) -> DataDirReport {
    let mut report = DataDirReport {
        path: path.display().to_string(),
        exists: path.exists(),
        will_create: false,
        writable: false,
        free_bytes: fsutil::free_space(path),
        existing: None,
        db_too_new: false,
        overlaps_source: None,
        error: None,
    };
    if !super::paths::is_absolute(path) {
        report.error = Some("not_absolute".into());
        return report;
    }
    for root in [&roots.claude, &roots.codex].into_iter().flatten() {
        if path.starts_with(root) || root.starts_with(path) {
            report.overlaps_source = Some(root.display().to_string());
            return report;
        }
    }
    if report.exists && !path.is_dir() {
        report.error = Some("not_a_folder".into());
        return report;
    }
    report.will_create = !report.exists;
    match writable(path) {
        Ok(()) => report.writable = true,
        Err(message) => report.error = Some(message),
    }
    let (existing, too_new) = existing_data(path);
    report.existing = existing;
    report.db_too_new = too_new;
    report
}

#[allow(clippy::struct_excessive_bools)] // a flat report the UI reads field by field
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScheduleEnvReport {
    pub supported: bool,
    pub launchctl_available: bool,
    pub launch_agents_writable: bool,
    pub exe: String,
    pub stability: ExeStability,
    /// Everything needed for a lasting task is in place.
    pub ok: bool,
}

pub fn test_schedule_env() -> ScheduleEnvReport {
    let exe = std::env::current_exe().unwrap_or_default();
    let stability = schedule::classify_exe(&exe);
    #[cfg(windows)]
    let supported = true;
    #[cfg(not(windows))]
    let supported = cfg!(target_os = "macos");
    #[cfg(windows)]
    let launchctl_available = Path::new("C:\\Windows\\System32\\schtasks.exe").exists();
    #[cfg(not(windows))]
    let launchctl_available = supported && Path::new("/bin/launchctl").exists();
    #[cfg(windows)]
    let launch_agents_writable = true;
    #[cfg(not(windows))]
    let launch_agents_writable = crate::utils::home_dir()
        .map(|h| h.join("Library").join("LaunchAgents"))
        .is_some_and(|dir| writable(&dir).is_ok());
    ScheduleEnvReport {
        supported,
        launchctl_available,
        launch_agents_writable,
        exe: exe.display().to_string(),
        stability,
        ok: supported
            && launchctl_available
            && launch_agents_writable
            && stability == ExeStability::Stable,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MirrorReport {
    /// Right disk, writable.
    Connected {
        volume: VolumeInfo,
        free_bytes: Option<u64>,
        existing_bytes: u64,
    },
    /// Right disk but we cannot write to it (read-only, full permissions…).
    NotWritable {
        volume: VolumeInfo,
        message: String,
    },
    /// Nothing identifiable at that location, or a different disk than recorded.
    NotConnected {
        reason: String,
    },
    Unsupported {
        reason: Unsupported,
    },
}

/// Check a backup location. With `expected_uuid` (a saved setting) the disk must
/// be that exact one. Nothing is created unless the disk is confirmed external.
pub fn test_mirror(
    target_dir: &Path,
    expected_uuid: Option<&str>,
    query: &dyn SystemQuery,
) -> MirrorReport {
    let volume = match volume::probe(target_dir, query) {
        Probe::External { volume } => volume,
        Probe::Unsupported { reason } => return MirrorReport::Unsupported { reason },
        Probe::Unavailable { reason } => return MirrorReport::NotConnected { reason },
    };
    if let Some(uuid) = expected_uuid {
        if !volume.uuid.eq_ignore_ascii_case(uuid) {
            return MirrorReport::NotConnected {
                reason: format!("{} is on a different disk", target_dir.display()),
            };
        }
    }
    // A disk's root is never the backup folder itself.
    let target_dir = &backup_dir::effective_root(target_dir, &volume.mount_point);
    // Create only below the confirmed mount point, never the `/Volumes/<name>` level.
    let anchor = fsutil::nearest_existing(target_dir).unwrap_or_else(|| volume.mount_point.clone());
    if !volume::same_device(&anchor, &volume.mount_point) {
        return MirrorReport::NotConnected {
            reason: "the disk went away".into(),
        };
    }
    if let Err(message) = std::fs::create_dir_all(target_dir)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            let probe = target_dir.join(format!(".agentkb-write-test-{}", std::process::id()));
            std::fs::write(&probe, b"x").map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&probe);
            Ok(())
        })
    {
        return MirrorReport::NotWritable { volume, message };
    }
    MirrorReport::Connected {
        free_bytes: fsutil::free_space(target_dir),
        existing_bytes: backup_dir::backup_bytes(target_dir),
        volume,
    }
}

/// Convenience for callers that only have a string path.
pub fn path_of(text: &str) -> PathBuf {
    PathBuf::from(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::volume::fake::{volume as fake_volume, FakeQuery};

    #[test]
    fn data_dir_report_for_a_fresh_location() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("new").join("AgentKB");
        let report = test_data_dir(&target, &SourceRoots::default());
        assert!(report.writable && report.will_create && !report.exists);
        assert!(report.existing.is_none());
        assert!(report.free_bytes.is_some());
        assert!(!target.exists(), "testing must not create the folder");
    }

    #[test]
    fn data_dir_report_recognises_existing_data_and_never_touches_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("AgentKB");
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        std::fs::write(dir.join("raw").join("a.jsonl"), b"12345").unwrap();
        let conn = db::open(&dir.join("kb.db")).unwrap();
        drop(conn);
        let before = fsutil::walk_files(&dir);
        let report = test_data_dir(&dir, &SourceRoots::default());
        let existing = report.existing.unwrap();
        assert!(existing.has_db && existing.has_raw);
        assert_eq!(existing.sessions, 0);
        assert!(existing.bytes >= 5);
        // Only SQLite's own side files may differ (read-only open of a WAL db).
        let after: Vec<_> = fsutil::walk_files(&dir)
            .into_iter()
            .filter(|(n, _)| !n.starts_with("kb.db-"))
            .collect();
        let before: Vec<_> = before
            .into_iter()
            .filter(|(n, _)| !n.starts_with("kb.db-"))
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn data_dir_overlapping_a_source_or_relative_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("home/u/.claude");
        let roots = SourceRoots {
            claude: Some(source.clone()),
            codex: None,
        };
        assert!(test_data_dir(&source.join("kb"), &roots)
            .overlaps_source
            .is_some());
        assert_eq!(
            test_data_dir(Path::new("rel"), &roots).error.as_deref(),
            Some("not_absolute")
        );
    }

    #[test]
    fn a_newer_database_is_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("kb");
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("kb.db")).unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
        drop(conn);
        assert!(test_data_dir(&dir, &SourceRoots::default()).db_too_new);
    }

    fn disk_world() -> (tempfile::TempDir, PathBuf, FakeQuery) {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().to_path_buf();
        let q = FakeQuery::with([VolumeInfo {
            mount_point: mount.clone(),
            ..fake_volume("DISK-UUID", "/x", false, "exfat")
        }]);
        (tmp, mount, q)
    }

    #[test]
    fn mirror_test_reports_connected_and_writable_and_creates_only_the_leaf() {
        let (_g, mount, q) = disk_world();
        let target = mount.join("AgentKB-backup");
        let report = test_mirror(&target, None, &q);
        assert!(
            matches!(report, MirrorReport::Connected { .. }),
            "{report:?}"
        );
        assert!(target.is_dir());
        assert!(fsutil::walk_files(&target).is_empty(), "probe file removed");
    }

    #[test]
    fn at_a_disks_root_only_the_backups_are_counted() {
        let (_g, mount, q) = disk_world();
        std::fs::write(mount.join("movie.mkv"), vec![0u8; 50_000]).unwrap();
        let dir = mount.join("AgentKB-backup/Mac-aaaaaaaa");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agentkb-backup.json"), r#"{"id":"a"}"#).unwrap();
        std::fs::write(dir.join("kb.db"), vec![0u8; 300]).unwrap();
        let MirrorReport::Connected { existing_bytes, .. } = test_mirror(&mount, None, &q) else {
            panic!("not connected");
        };
        assert!((300..2000).contains(&existing_bytes), "{existing_bytes}");
    }

    #[test]
    fn mirror_test_refuses_unplugged_internal_and_wrong_disks_without_creating_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("Volumes-Backup1").join("AgentKB-backup");
        // Internal disk answering for the path.
        let internal = FakeQuery::with([VolumeInfo {
            mount_point: tmp.path().to_path_buf(),
            ..fake_volume("SYS", "/", true, "apfs")
        }]);
        assert!(matches!(
            test_mirror(&target, None, &internal),
            MirrorReport::Unsupported {
                reason: Unsupported::Internal { .. }
            }
        ));
        assert!(!target.exists());

        let (_g, mount, q) = disk_world();
        let other = mount.join("AgentKB-backup");
        assert!(matches!(
            test_mirror(&other, Some("SOME-OTHER-UUID"), &q),
            MirrorReport::NotConnected { .. }
        ));
        assert!(!other.exists());
    }

    #[test]
    fn schedule_env_report_is_self_consistent() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("CCHV_TEST_HOME", home.path());
        let report = test_schedule_env();
        // Under `cargo test` the executable is never an installed app.
        assert!(!report.ok);
        assert_eq!(report.stability, ExeStability::NotInApp);
    }
}
