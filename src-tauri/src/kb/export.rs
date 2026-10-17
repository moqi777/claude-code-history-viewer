//! Export the whole library into one `.agentkb` file.
//!
//! The collection and digest locks are held only while a snapshot is taken:
//! `VACUUM INTO` for the database, a hard link for every `raw/` file (so a
//! later rewrite or rotation by a collection leaves the linked bytes alone),
//! a copy of the small `notes/`. Then the locks are released and the file is
//! written from the snapshot, so a scheduled collection is not held up. When
//! hard links are not possible the locks are kept for the whole export.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::Serialize;

use super::config::KbConfig;
use super::lock::{self, SyncLock};
use super::pack::{
    self, AppInfo, Counts, Manifest, PortableDigest, PortableSettings, SourceDevice, SourcesEnabled,
};
use super::progress::ProgressSink;
use super::sync::KbEnv;
use super::transfer::{
    clean_stale_work_dirs, device_name, is_conflict_path, now_rfc3339, now_stamp, Cancel, Meter,
    RemoveOnDrop, TResult, TransferError,
};
use super::{db, fsutil, AGENTKB_VERSION};

const LOCK_WAIT: Duration = Duration::from_secs(60);
/// What the data compresses to, rounded up (measured: 0.54).
const SPACE_FACTOR_PERCENT: u64 = 60;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExportReport {
    pub path: String,
    pub export_id: String,
    pub file_bytes: u64,
    pub uncompressed_bytes: u64,
    pub files: u64,
    pub sessions: u64,
    pub notes: u64,
    pub sessions_with_notes: u64,
    pub raw_files: u64,
    pub note_files: u64,
    pub duration_ms: u64,
    /// Collection had to wait for the whole export (hard links not possible).
    pub paused_collection: bool,
}

pub fn portable_settings(config: &KbConfig) -> PortableSettings {
    let d = &config.digest;
    PortableSettings {
        interval_secs: config.interval_secs,
        sources_enabled: SourcesEnabled {
            claude: config.sources.claude.enabled,
            codex: config.sources.codex.enabled,
        },
        exclude_prefixes: config.exclude_prefixes.clone(),
        exclude_cwd_prefixes: config.exclude_cwd_prefixes.clone(),
        digest: PortableDigest {
            engine: d.engine.clone(),
            model: d.model.clone(),
            time: d.time.clone(),
            max_sessions_per_round: d.max_sessions_per_round,
            recent_days: d.recent_days,
            quiet_minutes: d.quiet_minutes,
            min_messages: d.min_messages,
            max_chunks_per_session: d.max_chunks_per_session,
            codex_reasoning: d.codex_reasoning.clone(),
        },
    }
}

// Test seam: runs once the snapshot is taken and the locks are released.
#[cfg(test)]
thread_local! {
    pub static AFTER_SNAPSHOT: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

fn is_work_file(name: &str) -> bool {
    name.contains(".tmp-") || name.starts_with(".agentkb-")
}

fn rel_is_skipped(rel: &str) -> bool {
    rel.split('/').any(is_work_file)
}

/// One thing to put in the file: where its bytes are now, and how many.
struct Item {
    entry: String,
    from: PathBuf,
    len: u64,
}

fn count(conn: &Connection, sql: &str) -> u64 {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0))
        .map_or(0, |n| u64::try_from(n).unwrap_or(0))
}

/// Drop what is meaningless on another machine from the snapshot copy: the
/// digest's run state (retries, failures of this machine's runs).
fn clean_snapshot(path: &Path) -> TResult<(u64, u64, u64)> {
    let conn = Connection::open(path)
        .map_err(|e| TransferError::io(format!("cannot open the snapshot: {e}")))?;
    let tidy = || -> rusqlite::Result<()> {
        conn.execute_batch(
            "DELETE FROM digest_items; DELETE FROM digest_rounds; \
             DELETE FROM meta WHERE key = 'digest_first_round_seen'; \
             PRAGMA journal_mode = DELETE;",
        )
    };
    tidy().map_err(|e| TransferError::io(format!("cannot prepare the snapshot: {e}")))?;
    let sessions = count(&conn, "SELECT COUNT(*) FROM sessions");
    let notes = count(&conn, "SELECT COUNT(*) FROM notes");
    let with_notes = count(
        &conn,
        "SELECT COUNT(*) FROM (SELECT DISTINCT source, session_id FROM notes)",
    );
    conn.close()
        .map_err(|(_, e)| TransferError::io(format!("cannot finish the snapshot: {e}")))?;
    Ok((sessions, notes, with_notes))
}

pub fn export(
    env: &KbEnv,
    config: &KbConfig,
    dest: &Path,
    overwrite: bool,
    with_vectors: bool,
    cancel: &Cancel,
    progress: &ProgressSink,
) -> TResult<ExportReport> {
    let started = Instant::now();
    let data = &env.data;
    if !super::paths::is_absolute(dest) {
        return Err(TransferError::new(
            "bad_destination",
            "the destination must be an absolute path",
        ));
    }
    if dest.exists() && !overwrite {
        return Err(TransferError::new(
            "exists",
            format!("{} already exists", dest.display()),
        ));
    }
    let parent = dest.parent().filter(|p| p.is_dir()).ok_or_else(|| {
        TransferError::new("bad_destination", "the destination folder does not exist")
    })?;
    let canonical_parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    let canonical_data = data
        .root()
        .canonicalize()
        .unwrap_or_else(|_| data.root().to_path_buf());
    if canonical_parent.starts_with(&canonical_data) {
        return Err(TransferError::new(
            "bad_destination",
            "choose a place outside the AgentKB data folder",
        ));
    }
    if !data.db().is_file() {
        return Err(TransferError::new(
            "nothing_to_export",
            "there is no knowledge base to export yet",
        ));
    }

    // A digest writes notes for hours; never cut into it.
    let digest_busy = |info: lock::LockInfo| {
        TransferError::new(
            "digest_running",
            format!(
                "a digest is running (since {}); export after it has finished",
                info.started_at
            ),
        )
    };
    if let Some(info) = lock::held_by_live_process(&data.digest_lock()) {
        return Err(digest_busy(info));
    }
    let mut meter = Meter::new(progress, "export");
    meter.phase("snapshot", 0, 0);
    let sync_lock = SyncLock::acquire(&data.lock(), LOCK_WAIT)?.ok_or_else(|| {
        TransferError::new(
            "busy",
            "a collection is still running; try again in a moment",
        )
    })?;
    let digest_lock = SyncLock::acquire(&data.digest_lock(), Duration::ZERO)?
        .ok_or_else(|| TransferError::new("digest_running", "a digest is running"))?;

    clean_stale_work_dirs(data.root());
    let export_id = format!("{}-{:06x}", now_stamp(), std::process::id() & 0xff_ffff);
    let staging = data.root().join(format!(".export-{export_id}"));
    std::fs::create_dir_all(&staging)
        .map_err(|e| TransferError::io(format!("cannot create {}: {e}", staging.display())))?;
    let _staging_guard = RemoveOnDrop(staging.clone());

    // --- the snapshot, under the locks ---------------------------------
    let conn = db::open(&data.db())?;
    let rows = db::load_files(&conn)?;
    let snap_db = staging.join("kb.db");
    conn.execute("VACUUM INTO ?1", [snap_db.to_string_lossy().as_ref()])
        .map_err(|e| TransferError::io(format!("cannot snapshot kb.db: {e}")))?;
    let schema_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| TransferError::io(format!("user_version: {e}")))?;
    drop(conn);
    let (sessions, notes, sessions_with_notes) = clean_snapshot(&snap_db)?;

    let lengths: HashMap<&str, (bool, u64)> = rows
        .iter()
        .map(|r| {
            (
                r.mirror_rel.as_str(),
                (r.is_compressed, u64::try_from(r.mirror_offset).unwrap_or(0)),
            )
        })
        .collect();

    let mut items = vec![Item {
        entry: "kb.db".into(),
        len: std::fs::metadata(&snap_db)
            .map_err(|e| TransferError::io(format!("cannot read the snapshot: {e}")))?
            .len(),
        from: snap_db.clone(),
    }];

    // Notes: small, copied.
    let notes_dir = staging.join("notes");
    let mut note_files = 0u64;
    for (rel, size) in fsutil::walk_files(&data.notes()) {
        if rel_is_skipped(&rel) {
            continue;
        }
        let to = notes_dir.join(&rel);
        fsutil::copy_atomic(&data.notes().join(&rel), &to)?;
        if !is_conflict_path(&rel) {
            note_files += 1;
        }
        items.push(Item {
            entry: format!("notes/{rel}"),
            len: std::fs::metadata(&to).map_or(size, |m| m.len()),
            from: to,
        });
    }

    // The semantic index (a derived file, 25 MB): a consistent copy made with
    // SQLite's backup interface, since a builder may be writing to it.
    if with_vectors {
        let vectors = super::semantic::store::path(data);
        if let Ok(Some(store)) =
            super::semantic::store::Store::open_ro(&vectors, super::semantic::model::model_sha())
        {
            let name = vectors
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let to = staging.join("vectors").join(&name);
            std::fs::create_dir_all(staging.join("vectors"))
                .map_err(|e| TransferError::io(format!("cannot create the vectors folder: {e}")))?;
            store.backup_to(&to).map_err(TransferError::io)?;
            items.push(Item {
                entry: format!("vectors/{name}"),
                len: std::fs::metadata(&to).map_or(0, |m| m.len()),
                from: to,
            });
        }
    }

    // Raw files: hard-linked, to the length the database vouches for.
    let links = staging.join("links");
    let mut hard_links_ok = true;
    let mut raw_files = 0u64;
    for (rel, size) in fsutil::walk_files(&data.raw()) {
        if rel_is_skipped(&rel) {
            continue;
        }
        let len = match lengths.get(rel.as_str()) {
            Some((false, offset)) => size.min(*offset),
            _ => size,
        };
        let original = data.raw().join(&rel);
        let from = if hard_links_ok {
            let link = links.join(&rel);
            let made = link
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::hard_link(&original, &link));
            if made.is_ok() {
                link
            } else {
                hard_links_ok = false;
                original
            }
        } else {
            original
        };
        items.push(Item {
            entry: format!("raw/{rel}"),
            from,
            len,
        });
        raw_files += 1;
    }
    if !hard_links_ok {
        // Some links may exist already; reading from the originals needs the
        // locks held, so repoint every raw item at its original.
        for item in &mut items {
            if let Some(rel) = item.entry.strip_prefix("raw/") {
                item.from = data.raw().join(rel);
            }
        }
    }
    let _held = if hard_links_ok {
        drop(sync_lock);
        drop(digest_lock);
        None
    } else {
        Some((sync_lock, digest_lock))
    };

    #[cfg(test)]
    AFTER_SNAPSHOT.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook();
        }
    });

    // --- space, then the file, without the locks -----------------------
    let total_bytes: u64 = items.iter().map(|i| i.len).sum();
    if let Some(free) = fsutil::free_space(parent) {
        let need = total_bytes / 100 * SPACE_FACTOR_PERCENT;
        if free < need {
            return Err(TransferError::new(
                "no_space",
                format!(
                    "{} needs about {need} bytes and only {free} are free",
                    parent.display()
                ),
            ));
        }
    }
    meter.phase("write", items.len() as u64, total_bytes);

    let partial = {
        let mut name = dest.as_os_str().to_owned();
        name.push(".partial");
        PathBuf::from(name)
    };
    let _ = std::fs::remove_file(&partial);
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let file = open
        .open(&partial)
        .map_err(|e| TransferError::io(format!("cannot create {}: {e}", partial.display())))?;
    let remove_partial = |e: TransferError| {
        let _ = std::fs::remove_file(&partial);
        e
    };

    let mut writer = pack::Writer::new(file).map_err(remove_partial)?;
    for item in &items {
        let mut src = pack::open_prefix(&item.from, item.len).map_err(remove_partial)?;
        writer
            .add(&item.entry, item.len, &mut src, cancel, &mut |n| {
                meter.add_bytes(n);
            })
            .map_err(remove_partial)?;
        meter.file_done();
    }
    let manifest = Manifest {
        export_id: export_id.clone(),
        exported_at: now_rfc3339(),
        app: AppInfo {
            name: "AgentKB".into(),
            version: AGENTKB_VERSION.into(),
        },
        schema_version,
        source_device: SourceDevice {
            name: device_name(),
            os: std::env::consts::OS.into(),
            home: crate::utils::home_dir()
                .map(|h| h.to_string_lossy().into_owned())
                .unwrap_or_default(),
            data_dir: data.root().to_string_lossy().into_owned(),
            claude_root: env
                .roots
                .claude
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            codex_root: env
                .roots
                .codex
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        },
        counts: Counts {
            sessions,
            notes,
            sessions_with_notes,
            raw_files,
            note_files,
        },
        settings: portable_settings(config),
        ..Manifest::default()
    };
    let file: File = writer.finish(manifest).map_err(remove_partial)?;
    drop(file);
    cancel.check().map_err(remove_partial)?;
    if overwrite && dest.exists() {
        #[cfg(windows)]
        let _ = std::fs::remove_file(dest);
    }
    std::fs::rename(&partial, dest).map_err(|e| {
        remove_partial(TransferError::io(format!(
            "cannot place {}: {e}",
            dest.display()
        )))
    })?;
    fsutil::fsync_dir(parent);
    let file_bytes = std::fs::metadata(dest).map_or(0, |m| m.len());
    Ok(ExportReport {
        path: dest.display().to_string(),
        export_id,
        file_bytes,
        uncompressed_bytes: total_bytes,
        files: items.len() as u64,
        sessions,
        notes,
        sessions_with_notes,
        raw_files,
        note_files,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        paused_collection: !hard_links_ok,
    })
}
