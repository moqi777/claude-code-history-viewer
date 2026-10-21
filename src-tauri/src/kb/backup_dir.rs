//! Where on the backup disk this computer's backup lives.
//!
//! One disk can serve several computers, so the configured folder
//! (`<disk>/AgentKB-backup`) holds one subfolder per computer,
//! `<name>-<short id>/`, each with an `agentkb-backup.json` that says whose it
//! is. The id is made the first time and kept in the settings folder of this
//! computer (`backup-device.json`), so renaming the computer does not change
//! which folder is its own. Nothing is written into a folder whose marker
//! carries another id.
//!
//! The layout of 0.7.2 and 0.7.3 had `kb.db`, `raw/` and `notes/` directly in
//! the configured folder. Such a backup is moved into this computer's
//! subfolder when, and only when, its database says every session came from
//! source folders of this computer; otherwise it is left exactly as it is.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use super::config::SourceRoots;
use super::fsutil;
use super::log::SyncLog;

/// The folder backups live in on a disk.
pub const ROOT_FOLDER: &str = "AgentKB-backup";
pub const MARKER_NAME: &str = "agentkb-backup.json";
const ID_FILE_NAME: &str = "backup-device.json";
const LEGACY_ITEMS: [&str; 4] = ["kb.db", "kb.db.prev", "raw", "notes"];

/// Who this computer is, as far as backups go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub os: String,
}

impl Identity {
    fn short(&self) -> &str {
        &self.id[..self.id.len().min(8)]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Marker {
    pub id: String,
    pub name: String,
    pub os: String,
    pub first_backup_at: String,
    pub last_backup_at: String,
    /// Sessions in the database at the last good backup (for the restore list).
    pub sessions: u64,
    pub app_version: String,
}

#[derive(Serialize, Deserialize)]
struct IdFile {
    id: String,
}

/// Next to `config.json`. Not part of the data folder: copying the data to
/// another computer must not copy who the computer is.
pub fn id_file_path() -> PathBuf {
    super::config::config_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(ID_FILE_NAME)))
        .unwrap_or_else(|| PathBuf::from(ID_FILE_NAME))
}

/// The id of this computer if it has one (nothing is made).
pub fn existing_id(id_file: &Path) -> Option<String> {
    fs::read_to_string(id_file)
        .ok()
        .and_then(|text| serde_json::from_str::<IdFile>(&text).ok())
        .map(|f| f.id)
}

/// The id of this computer: read, or made and kept the first time.
pub fn identity(id_file: &Path) -> Result<Identity, String> {
    let existing = fs::read_to_string(id_file)
        .ok()
        .and_then(|text| serde_json::from_str::<IdFile>(&text).ok())
        .map(|f| f.id)
        .filter(|id| id.len() >= 8 && id.chars().all(|c| c.is_ascii_hexdigit()));
    let id = if let Some(id) = existing {
        id
    } else {
        let id = uuid::Uuid::new_v4().simple().to_string();
        if let Some(parent) = id_file.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let text =
            serde_json::to_string_pretty(&IdFile { id: id.clone() }).map_err(|e| e.to_string())?;
        fsutil::write_atomic(id_file, text.as_bytes())?;
        id
    };
    Ok(Identity {
        id,
        name: super::transfer::device_name(),
        os: std::env::consts::OS.to_string(),
    })
}

/// A folder name that is safe on exFAT, NTFS and APFS.
pub(super) fn folder_name(who: &Identity) -> String {
    let cleaned: String = who
        .name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) || c.is_whitespace() {
                '-'
            } else {
                c
            }
        })
        .take(40)
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '-' || c == '.');
    let cleaned = if cleaned.is_empty() {
        "computer"
    } else {
        cleaned
    };
    format!("{cleaned}-{}", who.short())
}

pub fn read_marker(dir: &Path) -> Option<Marker> {
    let text = fs::read_to_string(dir.join(MARKER_NAME)).ok()?;
    let marker: Marker = serde_json::from_str(&text).ok()?;
    (!marker.id.is_empty()).then_some(marker)
}

fn write_marker(dir: &Path, marker: &Marker) -> Result<(), String> {
    let text = serde_json::to_string_pretty(marker).map_err(|e| e.to_string())?;
    fsutil::write_atomic(&dir.join(MARKER_NAME), text.as_bytes())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// What `resolve` found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The folder to write to.
    pub dir: PathBuf,
    /// An old-layout backup is still directly in the configured folder.
    pub old_backup_left: bool,
    /// An old-layout backup was moved into `dir` this time.
    pub moved_old_backup: bool,
}

/// Whether the old layout is present in `root`.
pub fn has_old_layout(root: &Path) -> bool {
    root.join("kb.db").is_file() || root.join("raw").is_dir()
}

/// Find, or make, the folder of this computer inside `root`, and check that it
/// is its own. `work` is a local folder for looking at an old snapshot.
pub fn resolve(
    root: &Path,
    who: &Identity,
    roots: &SourceRoots,
    work: &Path,
    log: &SyncLog,
) -> Result<Resolved, String> {
    fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let mut ours: Option<PathBuf> = None;
    let mut names: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|e| format!("cannot read {}: {e}", root.display()))?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    names.sort();
    for dir in &names {
        if read_marker(dir).is_some_and(|m| m.id == who.id) {
            ours = Some(dir.clone());
            break;
        }
    }
    let mut moved_old = false;
    let dir = if let Some(dir) = ours {
        dir
    } else {
        let dir = root.join(folder_name(who));
        if dir.exists() {
            match read_marker(&dir) {
                Some(other) if other.id != who.id => {
                    return Err(not_yours(&dir, &other));
                }
                // Ours by name (e.g. an earlier run died before the marker was
                // written), or an empty folder.
                _ => {}
            }
        } else {
            fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        write_marker(
            &dir,
            &Marker {
                id: who.id.clone(),
                name: who.name.clone(),
                os: who.os.clone(),
                first_backup_at: now(),
                ..Marker::default()
            },
        )?;
        if has_old_layout(root) {
            moved_old = move_old_backup(root, &dir, roots, work, log)?;
        }
        dir
    };
    // Whatever was found, it must say it is ours before anything is written.
    match read_marker(&dir) {
        Some(m) if m.id == who.id => {}
        Some(other) => return Err(not_yours(&dir, &other)),
        None => {
            return Err(format!(
                "{} has no {MARKER_NAME}; it is not known to be this computer's backup",
                dir.display()
            ))
        }
    }
    Ok(Resolved {
        old_backup_left: has_old_layout(root),
        moved_old_backup: moved_old,
        dir,
    })
}

fn not_yours(dir: &Path, other: &Marker) -> String {
    format!(
        "{} is the backup of another computer ({} {}); nothing was written there",
        dir.display(),
        other.name,
        other.os
    )
}

/// Record a good backup in the marker.
pub fn touch(dir: &Path, who: &Identity, sessions: u64) -> Result<(), String> {
    let mut marker = read_marker(dir).unwrap_or_default();
    if marker.id != who.id {
        return Err(not_yours(dir, &marker));
    }
    marker.name.clone_from(&who.name);
    marker.os.clone_from(&who.os);
    marker.last_backup_at = now();
    marker.sessions = sessions;
    marker.app_version = super::AGENTKB_VERSION.to_string();
    if marker.first_backup_at.is_empty() {
        marker.first_backup_at = marker.last_backup_at.clone();
    }
    write_marker(dir, &marker)
}

// ---------------------------------------------------------------------------
// The old layout
// ---------------------------------------------------------------------------

fn normal(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = p.trim_end_matches('/').to_string();
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p
    }
}

fn under(path: &str, root: &Path) -> bool {
    let (path, root) = (normal(path), normal(&root.to_string_lossy()));
    !root.is_empty() && (path == root || path.starts_with(&format!("{root}/")))
}

/// Whether every session of the snapshot came from source folders of this
/// computer. `Err` and `Ok(false)` both mean "leave it alone".
fn belongs_here(snapshot: &Path, roots: &SourceRoots, work: &Path) -> Result<bool, String> {
    fs::create_dir_all(work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let copy = work.join(format!("old-snapshot-{}.db", std::process::id()));
    let result = (|| -> Result<Vec<String>, String> {
        fs::copy(snapshot, &copy).map_err(|e| format!("cannot copy the old snapshot: {e}"))?;
        let conn = Connection::open_with_flags(
            &copy,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("cannot open the old snapshot: {e}"))?;
        let mut stmt = conn
            .prepare("SELECT source_path FROM files")
            .map_err(|e| format!("cannot read the old snapshot: {e}"))?;
        let paths: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| format!("cannot read the old snapshot: {e}"))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("cannot read the old snapshot: {e}"))?;
        Ok(paths)
    })();
    let _ = fs::remove_file(&copy);
    let paths: Vec<String> = result?;
    if paths.is_empty() {
        return Ok(false);
    }
    let here = |p: &str| {
        [&roots.claude, &roots.codex]
            .into_iter()
            .flatten()
            .any(|root| under(p, root))
    };
    Ok(paths.iter().all(|p| here(p)))
}

/// Move the old-layout items into `dir`. Returns whether it did.
fn move_old_backup(
    root: &Path,
    dir: &Path,
    roots: &SourceRoots,
    work: &Path,
    log: &SyncLog,
) -> Result<bool, String> {
    let snapshot = root.join("kb.db");
    if !snapshot.is_file() {
        log.line("backup: an old-layout backup without a database stays where it is");
        return Ok(false);
    }
    match belongs_here(&snapshot, roots, work) {
        Ok(true) => {}
        Ok(false) => {
            log.line(&format!(
                "backup: the old backup in {} does not belong to this computer (or cannot be told); left alone",
                root.display()
            ));
            return Ok(false);
        }
        Err(message) => {
            log.line(&format!(
                "backup: cannot tell whose the old backup in {} is ({message}); left alone",
                root.display()
            ));
            return Ok(false);
        }
    }
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for item in LEGACY_ITEMS {
        let (from, to) = (root.join(item), dir.join(item));
        if !from.exists() {
            continue;
        }
        if to.exists() {
            continue;
        }
        if let Err(e) = fs::rename(&from, &to) {
            while let Some((a, b)) = done.pop() {
                let _ = fs::rename(&b, &a);
            }
            return Err(format!(
                "cannot move the old backup {} into {}: {e}",
                from.display(),
                dir.display()
            ));
        }
        done.push((from, to));
    }
    log.line(&format!(
        "backup: moved the old backup of this computer into {}",
        dir.display()
    ));
    Ok(true)
}

// ---------------------------------------------------------------------------
// Listing, for the restore
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BackupEntry {
    /// The folder to restore from.
    pub folder: String,
    pub name: String,
    pub os: String,
    pub last_backup_at: String,
    pub sessions: u64,
    /// The folder of the computer that asks.
    pub this_computer: bool,
    /// Old layout, no owner recorded.
    pub old_layout: bool,
}

/// The backups at `folder`: the folder itself when it is one (it has a
/// database), else the computers' subfolders, plus an old-layout backup next
/// to them. Chosen at a disk's root, `AgentKB-backup` is read as well; chosen
/// as `AgentKB-backup`, the computer folders left at the root beside it are.
pub fn list(folder: &Path, local_id: Option<&str>) -> Vec<BackupEntry> {
    let entry_of = |dir: &Path| {
        let marker = read_marker(dir);
        BackupEntry {
            folder: dir.display().to_string(),
            name: marker.as_ref().map_or_else(String::new, |m| m.name.clone()),
            os: marker.as_ref().map_or_else(String::new, |m| m.os.clone()),
            last_backup_at: marker
                .as_ref()
                .map_or_else(String::new, |m| m.last_backup_at.clone()),
            sessions: marker.as_ref().map_or(0, |m| m.sessions),
            this_computer: marker
                .as_ref()
                .is_some_and(|m| local_id.is_some_and(|id| id == m.id)),
            old_layout: marker.is_none(),
        }
    };
    // (place, whether a database right in it is a backup too)
    let mut places = vec![(folder.to_path_buf(), true)];
    let inner = folder.join(ROOT_FOLDER);
    if inner.is_dir() {
        places.push((inner, true));
    }
    if folder
        .file_name()
        .is_some_and(|n| n.to_string_lossy() == ROOT_FOLDER)
    {
        places.extend(folder.parent().map(|p| (p.to_path_buf(), false)));
    }
    let mut out: Vec<BackupEntry> = Vec::new();
    for (place, own_db) in &places {
        let mut found = Vec::new();
        if let Ok(entries) = fs::read_dir(place) {
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .filter(|p| read_marker(p).is_some() && p.join("kb.db").is_file())
                .collect();
            dirs.sort();
            found.extend(dirs.iter().map(|d| entry_of(d)));
        }
        if *own_db && place.join("kb.db").is_file() {
            found.insert(0, entry_of(place));
        }
        for entry in found {
            if !out.iter().any(|e| e.folder == entry.folder) {
                out.push(entry);
            }
        }
    }
    out
}

/// Whose backup a folder is, for the restore page.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Owner {
    pub name: String,
    pub os: String,
    pub this_computer: bool,
}

/// The owner recorded in `folder`; `None` for an old-layout backup.
pub fn owner(folder: &Path, local_id: Option<&str>) -> Option<Owner> {
    read_marker(folder).map(|m| Owner {
        this_computer: local_id.is_some_and(|id| id == m.id),
        name: m.name,
        os: m.os,
    })
}

/// The folder backups really go to: a disk's root becomes `<root>/AgentKB-backup`.
pub fn effective_root(target: &Path, mount: &Path) -> PathBuf {
    if normal(&target.to_string_lossy()) == normal(&mount.to_string_lossy()) {
        target.join(ROOT_FOLDER)
    } else {
        target.to_path_buf()
    }
}

/// Computer folders of this computer left at the disk's root by 0.7.4 move into
/// `root` (same disk, a rename). Another computer's, or a name already taken
/// in `root`, stay where they are. Returns what was moved.
pub fn adopt_strays(root: &Path, who: &Identity) -> Vec<PathBuf> {
    let Some(parent) = root.parent() else {
        return Vec::new();
    };
    if root
        .file_name()
        .map_or(true, |n| n.to_string_lossy() != ROOT_FOLDER)
    {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut moved = Vec::new();
    for dir in entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
    {
        if dir == root || !read_marker(&dir).is_some_and(|m| m.id == who.id) {
            continue;
        }
        let Some(name) = dir.file_name() else {
            continue;
        };
        let dest = root.join(name);
        // `root` already holding a folder of ours means it is the live one.
        if dest.exists() || fs::create_dir_all(root).is_err() {
            continue;
        }
        if fs::rename(&dir, &dest).is_ok() {
            moved.push(dest);
        }
    }
    moved
}

/// Bytes of `AgentKB`'s own backup content at `root`: the computer folders (those
/// with a marker) and an old-layout backup, or `root` itself when it is one.
/// Whatever else lives on the disk is not counted.
pub fn backup_bytes(root: &Path) -> u64 {
    if read_marker(root).is_some() {
        return fsutil::dir_size(root);
    }
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            if p.is_dir() {
                read_marker(p).is_some() || LEGACY_ITEMS.contains(&name_of(p).as_str())
            } else {
                LEGACY_ITEMS.contains(&name_of(p).as_str())
            }
        })
        .map(|p| {
            if p.is_dir() {
                fsutil::dir_size(&p)
            } else {
                fs::metadata(&p).map_or(0, |m| m.len())
            }
        })
        .sum()
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(id: &str, name: &str) -> Identity {
        Identity {
            id: id.into(),
            name: name.into(),
            os: "macos".into(),
        }
    }

    fn log(dir: &Path) -> SyncLog {
        SyncLog::open(&dir.join("logs"))
    }

    #[test]
    fn the_id_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cfg/backup-device.json");
        let a = identity(&file).unwrap();
        let b = identity(&file).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.id.len(), 32);
        assert_eq!(a.short().len(), 8);
    }

    #[test]
    fn folder_names_are_safe_everywhere() {
        let w = who("0123456789abcdef0123456789abcdef", "Mo's Mac/Book: Pro?");
        let name = folder_name(&w);
        assert!(name.ends_with("-01234567"), "{name}");
        assert!(!name.contains(['/', ':', '?', ' ']), "{name}");
        assert_eq!(
            folder_name(&who("0123456789abcdef", "")),
            "computer-01234567"
        );
    }

    #[test]
    fn a_folder_is_made_with_a_marker_and_found_again_after_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("AgentKB-backup");
        let first = resolve(
            &root,
            &who("aaaaaaaa11111111", "Old-Name"),
            &SourceRoots::default(),
            dir.path(),
            &log(dir.path()),
        )
        .unwrap();
        assert!(first.dir.ends_with("Old-Name-aaaaaaaa"));
        assert_eq!(read_marker(&first.dir).unwrap().id, "aaaaaaaa11111111");
        let again = resolve(
            &root,
            &who("aaaaaaaa11111111", "New-Name"),
            &SourceRoots::default(),
            dir.path(),
            &log(dir.path()),
        )
        .unwrap();
        assert_eq!(again.dir, first.dir, "the computer was renamed");
    }

    #[test]
    fn two_computers_get_two_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("AgentKB-backup");
        let a = resolve(
            &root,
            &who("aaaaaaaa11111111", "Mac"),
            &SourceRoots::default(),
            dir.path(),
            &log(dir.path()),
        )
        .unwrap();
        let b = resolve(
            &root,
            &who("bbbbbbbb22222222", "Mac"),
            &SourceRoots::default(),
            dir.path(),
            &log(dir.path()),
        )
        .unwrap();
        assert_ne!(a.dir, b.dir);
    }

    #[test]
    fn a_folder_that_carries_another_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("AgentKB-backup");
        // The planned folder of "bbbbbbbb…" exists and belongs to somebody else.
        let theirs = root.join("Win-bbbbbbbb");
        fs::create_dir_all(&theirs).unwrap();
        write_marker(
            &theirs,
            &Marker {
                id: "cccccccc33333333".into(),
                name: "Win".into(),
                os: "windows".into(),
                ..Marker::default()
            },
        )
        .unwrap();
        let err = resolve(
            &root,
            &who("bbbbbbbb22222222", "Win"),
            &SourceRoots::default(),
            dir.path(),
            &log(dir.path()),
        )
        .unwrap_err();
        assert!(err.contains("another computer"), "{err}");
        assert!(!theirs.join("kb.db").exists());
        // and `touch` refuses too
        assert!(touch(&theirs, &who("bbbbbbbb22222222", "Win"), 1).is_err());
    }

    fn old_layout(root: &Path, source_paths: &[&str]) {
        fs::create_dir_all(root.join("raw/claude")).unwrap();
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("raw/claude/a.jsonl"), b"x\n").unwrap();
        let conn = Connection::open(root.join("kb.db")).unwrap();
        conn.execute_batch("CREATE TABLE files (id INTEGER PRIMARY KEY, source_path TEXT)")
            .unwrap();
        for p in source_paths {
            conn.execute("INSERT INTO files (source_path) VALUES (?1)", [p])
                .unwrap();
        }
    }

    fn roots() -> SourceRoots {
        SourceRoots {
            claude: Some(PathBuf::from("/Users/me/.claude")),
            codex: Some(PathBuf::from("/Users/me/.codex")),
        }
    }

    #[test]
    fn an_old_backup_of_this_computer_is_moved_in() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("AgentKB-backup");
        old_layout(
            &root,
            &[
                "/Users/me/.claude/projects/p/a.jsonl",
                "/Users/me/.codex/sessions/x.jsonl",
            ],
        );
        let r = resolve(
            &root,
            &who("aaaaaaaa11111111", "Mac"),
            &roots(),
            dir.path(),
            &log(dir.path()),
        )
        .unwrap();
        assert!(r.moved_old_backup);
        assert!(!r.old_backup_left);
        assert!(r.dir.join("kb.db").is_file());
        assert!(r.dir.join("raw/claude/a.jsonl").is_file());
        assert!(r.dir.join("notes").is_dir());
        assert!(!root.join("kb.db").exists() && !root.join("raw").exists());
    }

    #[test]
    fn an_old_backup_of_somebody_else_or_of_unknown_origin_stays_put() {
        for paths in [
            vec!["C:\\Users\\me\\.claude\\projects\\p\\a.jsonl"],
            vec![
                "/Users/me/.claude/projects/p/a.jsonl",
                "/Users/other/.claude/projects/p/b.jsonl",
            ],
            vec![],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("AgentKB-backup");
            old_layout(&root, &paths);
            let before = fs::read(root.join("kb.db")).unwrap();
            let r = resolve(
                &root,
                &who("aaaaaaaa11111111", "Mac"),
                &roots(),
                dir.path(),
                &log(dir.path()),
            )
            .unwrap();
            assert!(!r.moved_old_backup, "{paths:?}");
            assert!(r.old_backup_left, "{paths:?}");
            assert_eq!(fs::read(root.join("kb.db")).unwrap(), before);
            assert!(root.join("raw/claude/a.jsonl").is_file());
            assert!(r.dir.starts_with(&root) && r.dir != root);
            assert!(!r.dir.join("kb.db").exists());
        }
    }

    #[test]
    fn an_unreadable_old_snapshot_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("AgentKB-backup");
        fs::create_dir_all(root.join("raw")).unwrap();
        fs::write(root.join("kb.db"), vec![9u8; 4000]).unwrap();
        let r = resolve(
            &root,
            &who("aaaaaaaa11111111", "Mac"),
            &roots(),
            dir.path(),
            &log(dir.path()),
        )
        .unwrap();
        assert!(!r.moved_old_backup);
        assert!(root.join("kb.db").is_file());
    }

    #[test]
    fn the_old_backup_is_not_touched_once_the_computer_has_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("AgentKB-backup");
        let me = who("aaaaaaaa11111111", "Mac");
        resolve(&root, &me, &roots(), dir.path(), &log(dir.path())).unwrap();
        old_layout(&root, &["/Users/me/.claude/projects/p/a.jsonl"]);
        let r = resolve(&root, &me, &roots(), dir.path(), &log(dir.path())).unwrap();
        assert!(!r.moved_old_backup);
        assert!(r.old_backup_left);
        assert!(root.join("kb.db").is_file());
    }

    #[test]
    fn list_shows_each_computer_the_folder_itself_and_an_old_backup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("AgentKB-backup");
        for (id, name) in [("aaaaaaaa11111111", "Mac"), ("bbbbbbbb22222222", "Win")] {
            let r = resolve(
                &root,
                &who(id, name),
                &SourceRoots::default(),
                dir.path(),
                &log(dir.path()),
            )
            .unwrap();
            fs::write(r.dir.join("kb.db"), b"x").unwrap();
            touch(&r.dir, &who(id, name), 7).unwrap();
        }
        let entries = list(&root, Some("bbbbbbbb22222222"));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries.iter().filter(|e| e.this_computer).count(), 1);
        assert!(entries.iter().all(|e| e.sessions == 7 && !e.old_layout));
        // an old backup next to them
        fs::create_dir_all(root.join("raw")).unwrap();
        fs::write(root.join("kb.db"), b"x").unwrap();
        let entries = list(&root, None);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].old_layout);
        // pointing at one computer's folder lists just that
        let one = list(Path::new(&entries[1].folder), None);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].folder, entries[1].folder);
    }

    fn folder_of(parent: &Path, w: &Identity, with_db: bool) -> PathBuf {
        let dir = parent.join(folder_name(w));
        fs::create_dir_all(&dir).unwrap();
        write_marker(
            &dir,
            &Marker {
                id: w.id.clone(),
                name: w.name.clone(),
                os: w.os.clone(),
                ..Marker::default()
            },
        )
        .unwrap();
        if with_db {
            fs::write(dir.join("kb.db"), b"db").unwrap();
        }
        dir
    }

    #[test]
    fn a_disks_root_becomes_the_backup_folder_and_anything_else_is_kept() {
        let mount = Path::new("/Volumes/Share");
        assert_eq!(effective_root(mount, mount), mount.join("AgentKB-backup"));
        assert_eq!(
            effective_root(Path::new("/Volumes/Share/"), mount),
            mount.join("AgentKB-backup")
        );
        let inner = mount.join("AgentKB-backup");
        assert_eq!(effective_root(&inner, mount), inner);
        let other = mount.join("my/backups");
        assert_eq!(effective_root(&other, mount), other);
    }

    #[test]
    fn own_folders_at_the_root_move_in_and_other_computers_stay() {
        let disk = tempfile::tempdir().unwrap();
        let me = who("aaaaaaaa11111111aaaaaaaa11111111", "PC");
        let other = who("bbbbbbbb22222222bbbbbbbb22222222", "Mac");
        let mine = folder_of(disk.path(), &me, true);
        let theirs = folder_of(disk.path(), &other, true);
        let root = disk.path().join("AgentKB-backup");
        let moved = adopt_strays(&root, &me);
        assert_eq!(moved, vec![root.join(folder_name(&me))]);
        assert!(!mine.exists() && root.join(folder_name(&me)).join("kb.db").is_file());
        assert!(theirs.exists(), "another computer's folder is not touched");
        assert!(!root.join(folder_name(&other)).exists());
        // Nothing left to move the second time; a taken name is never replaced.
        assert!(adopt_strays(&root, &me).is_empty());
        let again = folder_of(disk.path(), &me, false);
        assert!(adopt_strays(&root, &me).is_empty());
        assert!(again.exists());
        // Only `AgentKB-backup` adopts, a folder of any other name does not.
        assert!(adopt_strays(&disk.path().join("elsewhere"), &me).is_empty());
    }

    #[test]
    fn the_list_at_a_root_or_at_the_backup_folder_has_both_places() {
        let disk = tempfile::tempdir().unwrap();
        let win = who("bbbbbbbb22222222bbbbbbbb22222222", "PC");
        let mac = who("aaaaaaaa11111111aaaaaaaa11111111", "Mac");
        folder_of(disk.path(), &win, true); // left at the root
        let root = disk.path().join("AgentKB-backup");
        folder_of(&root, &mac, true);
        for chosen in [disk.path().to_path_buf(), root.clone()] {
            let names: Vec<String> = list(&chosen, Some(&mac.id))
                .into_iter()
                .map(|e| e.name)
                .collect();
            assert_eq!(names.len(), 2, "{chosen:?}: {names:?}");
            assert!(names.contains(&"PC".to_string()) && names.contains(&"Mac".to_string()));
        }
        assert!(list(&root, Some(&mac.id)).iter().any(|e| e.this_computer));
    }

    #[test]
    fn only_backup_content_is_counted() {
        let disk = tempfile::tempdir().unwrap();
        fs::write(disk.path().join("movie.mkv"), vec![0u8; 5000]).unwrap();
        fs::create_dir_all(disk.path().join("Photos")).unwrap();
        fs::write(disk.path().join("Photos/a.jpg"), vec![0u8; 7000]).unwrap();
        let root = disk.path().join("AgentKB-backup");
        let dir = folder_of(&root, &who("aaaaaaaa11111111aaaaaaaa11111111", "Mac"), true);
        fs::write(dir.join("raw.bin"), vec![0u8; 100]).unwrap();
        fs::write(root.join("kb.db"), vec![0u8; 40]).unwrap(); // an old-layout backup
        let counted = backup_bytes(&root);
        assert!((142..1000).contains(&counted), "{counted}");
        // At the disk's root, files that are not AgentKB's stay out.
        assert_eq!(backup_bytes(disk.path()), 0);
        // A computer folder asked for by itself is counted whole.
        assert!(backup_bytes(&dir) >= 102);
    }
}
