//! One collection pass: discover → reconcile moves/deletions → mirror + index
//! each changed file → refresh Codex titles → record the run.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::Serialize;

use super::config::{KbConfig, SourceRoots};
use super::db::{self, Cursor, FileRow, NewFile};
use super::discover::{self, Discovery, ScanError, SourceFile};
use super::index::codex_titles;
use super::index::{FileMeta, Indexer};
use super::lock::SyncLock;
use super::log::{self, SyncLog};
use super::mirror::{self, Decision, Observed};
use super::paths::DataPaths;
use super::progress::{Progress, ProgressSink};
use super::Source;

/// A sync that meets a held lock keeps retrying this long before it gives up,
/// so a status probe or a just-finishing run never costs a scheduled round.
pub const LOCK_WAIT: Duration = Duration::from_secs(3);

/// Bump when the indexers' output changes (title rules, counting, known types…).
/// The next sync then rebuilds every session row from the mirror, which is the
/// source of truth, so improvements reach sessions that never change again.
pub const INDEXER_VERSION: &str = "6";
const INDEXER_VERSION_KEY: &str = "indexer_version";
/// The excluded working directories the index was last built under.
const EXCLUDE_CWD_KEY: &str = "exclude_cwd_applied";

/// Warn (in the log) about unexpectedly huge single lines.
const HUGE_LINE_BYTES: u64 = 100 * 1024 * 1024;

/// The external backup disk, as recorded when it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorTarget {
    pub target_dir: PathBuf,
    pub volume_uuid: String,
}

#[derive(Debug, Clone)]
pub struct KbEnv {
    pub data: DataPaths,
    pub roots: SourceRoots,
    pub exclude_prefixes: Vec<String>,
    /// Working directories left out of search and the session list.
    pub exclude_cwd_prefixes: Vec<String>,
    /// `Some` only when the disk backup is enabled and fully configured.
    pub mirror: Option<MirrorTarget>,
    pub progress: ProgressSink,
    /// Rebuild every derived row from the mirror even if the indexer version
    /// matches (`--kb-reindex`).
    pub force_reindex: bool,
}

impl KbEnv {
    pub fn from_config(config: &KbConfig) -> Self {
        let mirror = config.mirror.enabled.then(|| {
            Some(MirrorTarget {
                target_dir: config.mirror.target_dir.clone()?,
                volume_uuid: config.mirror.volume_uuid.clone()?,
            })
        });
        let data = DataPaths::new(config.data_dir.clone());
        // The digest's own working directory is always excluded from search
        // and the session list (its sessions stay in the backup), whatever the
        // user's list says and whether or not the digest is switched on.
        let mut exclude_cwd_prefixes = config.exclude_cwd_prefixes.clone();
        exclude_cwd_prefixes.push(data.digest_work().to_string_lossy().into_owned());
        Self {
            data,
            roots: SourceRoots::resolve(config),
            exclude_prefixes: config.exclude_prefixes.clone(),
            exclude_cwd_prefixes,
            mirror: mirror.flatten(),
            progress: ProgressSink::default(),
            force_reindex: false,
        }
    }

    pub fn with_progress(mut self, progress: ProgressSink) -> Self {
        self.progress = progress;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncError {
    pub path: String,
    pub stage: String,
    pub message: String,
}

impl From<ScanError> for SyncError {
    fn from(e: ScanError) -> Self {
        Self {
            path: e.path,
            stage: e.stage.to_string(),
            message: e.message,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    /// `ok`, `partial` (some files failed), `skipped` (lock held) or `error`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub trigger: String,
    pub scanned_files: u64,
    pub changed_files: u64,
    pub copied_bytes: u64,
    pub new_sessions: u64,
    pub deleted_sources: u64,
    pub moved_sources: u64,
    /// Sessions that older versions stored twice (plain and compressed).
    pub joined_twins: u64,
    pub rewritten_files: u64,
    pub errors: Vec<SyncError>,
    pub unknown_types: BTreeMap<String, u64>,
    pub bad_lines: u64,
    pub longest_line_bytes: u64,
    pub duration_ms: u64,
    /// Present when a backup disk is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<super::backup::BackupSummary>,
}

impl Summary {
    pub fn skipped(trigger: &str, reason: &str) -> Self {
        Self {
            status: "skipped".into(),
            reason: Some(reason.into()),
            trigger: trigger.into(),
            ..Self::default()
        }
    }
}

/// Run one collection. Takes the sync lock (waiting up to [`LOCK_WAIT`]); a
/// held lock yields a `skipped` summary, not an error.
pub fn run_sync(env: &KbEnv, trigger: &str) -> Result<Summary, String> {
    run_sync_with(env, trigger, &super::volume::MacQuery)
}

/// [`run_sync`] with the disk-identification backend chosen by the caller.
pub fn run_sync_with(
    env: &KbEnv,
    trigger: &str,
    query: &dyn super::volume::SystemQuery,
) -> Result<Summary, String> {
    let started = Instant::now();
    let Some(_lock) = SyncLock::acquire(&env.data.lock(), LOCK_WAIT)? else {
        return Ok(Summary::skipped(trigger, "locked"));
    };

    std::fs::create_dir_all(env.data.raw())
        .map_err(|e| format!("cannot create {}: {e}", env.data.raw().display()))?;
    let log = SyncLog::open(&env.data.logs());
    log.line(&format!("sync start trigger={trigger}"));
    log::prune(&env.data.logs());

    let conn = db::open(&env.data.db())?;
    let run_id = db::begin_run(&conn, trigger, &now())?;

    let mut summary = Summary {
        trigger: trigger.to_string(),
        ..Summary::default()
    };
    let result = collect(env, &conn, &log, &mut summary);
    // Collection first, so the backup copies what was just collected. A backup
    // problem is reported but never turns a good collection into a failed one.
    if result.is_ok() && env.mirror.is_some() {
        let backup = super::backup::run(env, &conn, &log, query);
        summary.errors.extend(backup.errors.iter().cloned());
        summary.backup = Some(backup);
    }
    summary.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(()) => {
            summary.status = if summary.errors.is_empty() {
                "ok"
            } else {
                "partial"
            }
            .into();
        }
        Err(message) => {
            summary.status = "error".into();
            summary.errors.push(SyncError {
                path: String::new(),
                stage: "sync".into(),
                message,
            });
        }
    }

    let json = serde_json::to_string(&summary).unwrap_or_else(|_| "{}".into());
    log.line(&format!("sync end {json}"));
    for (kind, count) in &summary.unknown_types {
        log.line(&format!("unknown record type {kind:?}: {count}"));
    }
    db::finish_run(&conn, run_id, &now(), &json, summary.errors.is_empty())?;
    Ok(summary)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn collect(
    env: &KbEnv,
    conn: &Connection,
    log: &SyncLog,
    summary: &mut Summary,
) -> Result<(), String> {
    let discovery = discover::discover(&env.roots, &env.exclude_prefixes);
    summary.scanned_files = discovery.files.len() as u64;
    summary
        .errors
        .extend(discovery.errors.iter().cloned().map(SyncError::from));

    join_twin_rows(env, conn, log, summary)?;
    reindex_if_outdated(env, conn, log, summary)?;
    // Notes are files first: bring the database copy in line with them.
    if let Err(message) = super::notes::reconcile(conn, &env.data) {
        log.line(&format!("ERROR notes: {message}"));
        summary.errors.push(SyncError {
            path: env.data.notes().display().to_string(),
            stage: "notes".into(),
            message,
        });
    }
    reconcile_sources(env, conn, &discovery, log, summary)?;

    // Reload: moves changed `source_path`s.
    let rows = db::load_files(conn)?;
    let by_path: HashMap<&str, &FileRow> =
        rows.iter().map(|r| (r.source_path.as_str(), r)).collect();

    let total = discovery.files.len() as u64;
    for (index, file) in discovery.files.iter().enumerate() {
        if index % 25 == 0 {
            env.progress.emit(&Progress {
                op: "sync".into(),
                phase: "collect".into(),
                done: index as u64,
                total,
                bytes_done: summary.copied_bytes,
                bytes_total: 0,
            });
        }
        let prior = by_path.get(file.path_str.as_str()).copied();
        if let Err(message) = process_file(env, conn, file, prior, log, summary) {
            log.line(&format!("ERROR {}: {message}", file.path_str));
            summary.errors.push(SyncError {
                path: file.path_str.clone(),
                stage: "mirror".into(),
                message,
            });
        }
    }

    if summary.longest_line_bytes >= HUGE_LINE_BYTES {
        log.line(&format!(
            "WARNING longest line is {} bytes",
            summary.longest_line_bytes
        ));
    }

    if let Some(codex) = &env.roots.codex {
        if let Err(message) = refresh_codex_titles(conn, codex, summary) {
            summary.errors.push(SyncError {
                path: codex.display().to_string(),
                stage: "titles".into(),
                message,
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Moves and deletions
// ---------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reconciliation {
    /// (file id, new source path)
    pub moves: Vec<(i64, String)>,
    /// The file is still there but stored the other way (plain <-> `.zst`).
    /// Only a candidate until its content is compared.
    pub restored: Vec<Restored>,
    pub deleted: Vec<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Restored {
    pub id: i64,
    pub new_path: String,
    pub new_mirror_rel: String,
    pub compressed: bool,
}

/// Codex moves a rollout between `sessions/` and `archived_sessions/` keeping
/// its filename, and compresses old ones in place by adding `.zst`. The
/// filename without that suffix (it carries the session id) is its identity.
fn move_key(source: Source, path: &Path) -> Option<(Source, String)> {
    if source != Source::Codex {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".zst").unwrap_or(name);
    Some((source, stem.to_string()))
}

/// Decide, from one complete listing, which vanished sources were moved and
/// which are really gone. Moves are resolved BEFORE anything is called
/// deleted, so a file that merely changed folders is never marked deleted.
pub fn reconcile(
    rows: &[FileRow],
    discovery: &Discovery,
    exclude_prefixes: &[String],
) -> Reconciliation {
    let found: HashSet<&str> = discovery
        .files
        .iter()
        .map(|f| f.path_str.as_str())
        .collect();
    let known: HashSet<&str> = rows.iter().map(|r| r.source_path.as_str()).collect();

    // Unclaimed new paths by identity.
    let mut fresh: HashMap<(Source, String), Vec<&SourceFile>> = HashMap::new();
    for file in &discovery.files {
        if known.contains(file.path_str.as_str()) {
            continue;
        }
        if let Some(key) = move_key(file.source, &file.path) {
            fresh.entry(key).or_default().push(file);
        }
    }

    let mut out = Reconciliation::default();
    for row in rows {
        let path = Path::new(&row.source_path);
        let vanished = !row.deleted && !found.contains(row.source_path.as_str());
        if !vanished
            || discover::is_excluded(path, exclude_prefixes)
            || !discovery
                .complete_roots
                .iter()
                .any(|root| path.starts_with(root))
        {
            continue;
        }
        let candidates = move_key(row.source, path).and_then(|key| fresh.get_mut(&key));
        // Same storage first; the other way only when nothing else is left.
        let chosen = candidates.and_then(|list| {
            let at = list
                .iter()
                .rposition(|f| f.is_compressed == row.is_compressed)
                .or_else(|| list.len().checked_sub(1))?;
            Some(list.remove(at))
        });
        match chosen {
            Some(new_file) if new_file.is_compressed == row.is_compressed => {
                out.moves.push((row.id, new_file.path_str.clone()));
            }
            Some(new_file) => out.restored.push(Restored {
                id: row.id,
                new_path: new_file.path_str.clone(),
                new_mirror_rel: new_file.mirror_rel.clone(),
                compressed: new_file.is_compressed,
            }),
            None => out.deleted.push(row.id),
        }
    }
    out
}

fn reconcile_sources(
    env: &KbEnv,
    conn: &Connection,
    discovery: &Discovery,
    log: &SyncLog,
    summary: &mut Summary,
) -> Result<(), String> {
    let rows = db::load_files(conn)?;
    let plan = reconcile(&rows, discovery, &env.exclude_prefixes);
    if plan.moves.is_empty() && plan.restored.is_empty() && plan.deleted.is_empty() {
        return Ok(());
    }
    let mut deleted = plan.deleted;
    // A file stored another way is the same file only if what the mirror
    // already holds is the beginning of what is there now.
    let mut converted = Vec::new();
    for item in plan.restored {
        let Some(row) = rows.iter().find(|r| r.id == item.id) else {
            continue;
        };
        let same = env.data.mirror_path(&row.mirror_rel).and_then(|mirror| {
            super::mirror::content_continues(
                (
                    &mirror,
                    row.is_compressed,
                    Some(u64::try_from(row.mirror_offset).unwrap_or(0)),
                ),
                (Path::new(&item.new_path), item.compressed),
            )
        });
        match same {
            Ok(true) => converted.push(item),
            Ok(false) => {
                log.line(&format!(
                    "{} is not the content of {}: handled as a different file",
                    item.new_path, row.source_path
                ));
                deleted.push(item.id);
            }
            Err(message) => {
                // Cannot prove it: change nothing, look again next time.
                log.line(&format!("cannot compare {}: {message}", item.new_path));
                summary.errors.push(SyncError {
                    path: item.new_path.clone(),
                    stage: "reconcile".into(),
                    message,
                });
            }
        }
    }
    let when = now();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin reconcile: {e}"))?;
    for (id, new_path) in &plan.moves {
        db::set_source_path(&tx, *id, new_path)?;
    }
    for item in &converted {
        db::convert_file(
            &tx,
            item.id,
            &item.new_path,
            &item.new_mirror_rel,
            item.compressed,
        )?;
    }
    for id in &deleted {
        db::mark_deleted(&tx, *id, &when)?;
    }
    tx.commit().map_err(|e| format!("commit reconcile: {e}"))?;
    summary.moved_sources += (plan.moves.len() + converted.len()) as u64;
    summary.deleted_sources += deleted.len() as u64;
    Ok(())
}

/// Older versions took a rollout that Codex compressed (`X.jsonl` ->
/// `X.jsonl.zst`) for a new session and the old one for deleted, leaving the
/// same session twice. Join such a pair back: keep the row that follows the
/// source, drop the other from the index. Both mirrors stay in `raw/`.
///
/// Only when the deleted row's mirror is the beginning of the present row's
/// content; anything else is a different file and stays as it is.
fn join_twin_rows(
    env: &KbEnv,
    conn: &Connection,
    log: &SyncLog,
    summary: &mut Summary,
) -> Result<(), String> {
    let rows = db::load_files(conn)?;
    let mut present: HashMap<(Source, String), Vec<&FileRow>> = HashMap::new();
    for row in rows.iter().filter(|r| !r.deleted) {
        if let Some(key) = move_key(row.source, Path::new(&row.source_path)) {
            present.entry(key).or_default().push(row);
        }
    }
    let mut pairs: Vec<(&FileRow, &FileRow)> = Vec::new();
    for gone in rows.iter().filter(|r| r.deleted) {
        let Some(key) = move_key(gone.source, Path::new(&gone.source_path)) else {
            continue;
        };
        let Some(alive) = present
            .get(&key)
            .and_then(|list| list.iter().find(|r| r.is_compressed != gone.is_compressed))
        else {
            continue;
        };
        let (Ok(old), Ok(new)) = (
            env.data.mirror_path(&gone.mirror_rel),
            env.data.mirror_path(&alive.mirror_rel),
        ) else {
            continue;
        };
        match super::mirror::content_continues(
            (
                &old,
                gone.is_compressed,
                Some(u64::try_from(gone.mirror_offset).unwrap_or(0)),
            ),
            (&new, alive.is_compressed),
        ) {
            Ok(true) => pairs.push((gone, alive)),
            Ok(false) => {}
            Err(message) => log.line(&format!("cannot compare {}: {message}", gone.source_path)),
        }
    }
    if pairs.is_empty() {
        return Ok(());
    }
    let copy = db::snapshot(conn, &format!("before-twin-join-{}", compact_stamp()))?;
    log.line(&format!(
        "joining {} sessions stored twice (plain and compressed); kb.db copied to {}",
        pairs.len(),
        copy.display()
    ));
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin join: {e}"))?;
    for (gone, alive) in &pairs {
        log.line(&format!(
            "joined {} into {} (mirror {} kept in raw/)",
            gone.source_path, alive.source_path, gone.mirror_rel
        ));
        db::forget_file(&tx, gone.id)?;
    }
    tx.commit().map_err(|e| format!("commit join: {e}"))?;
    summary.joined_twins += pairs.len() as u64;
    Ok(())
}

fn compact_stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

// ---------------------------------------------------------------------------
// Re-deriving the index from the mirror
// ---------------------------------------------------------------------------

fn reindex_if_outdated(
    env: &KbEnv,
    conn: &Connection,
    log: &SyncLog,
    summary: &mut Summary,
) -> Result<(), String> {
    // Changing the excluded directories rebuilds too: sessions that stop being
    // excluded need their messages back, which only the mirror can give.
    let exclusions = env.exclude_cwd_prefixes.join("\n");
    if !env.force_reindex
        && db::get_meta(conn, INDEXER_VERSION_KEY)?.as_deref() == Some(INDEXER_VERSION)
        && db::get_meta(conn, EXCLUDE_CWD_KEY)?.unwrap_or_default() == exclusions
    {
        return Ok(());
    }
    let rows = db::load_files(conn)?;
    db::reset_message_index(conn)?;
    log.line(&format!(
        "indexer version changed: rebuilding {} session rows from the mirror",
        rows.len()
    ));
    let mut failed = false;
    for row in &rows {
        if let Err(message) = reindex_one(env, conn, row) {
            failed = true;
            log.line(&format!("ERROR reindex {}: {message}", row.source_path));
            summary.errors.push(SyncError {
                path: row.source_path.clone(),
                stage: "reindex".into(),
                message,
            });
        }
    }
    // The notes are not derived from `raw/` but from their own files; they are
    // re-indexed together with everything else so one command restores all.
    match super::notes::rebuild_all(conn, &env.data) {
        Ok(report) => {
            log.line(&format!(
                "notes: {} files, {} entries re-indexed",
                report.files, report.entries
            ));
            for (path, reason) in report.skipped {
                log.line(&format!("ERROR note file {path}: {reason}"));
                summary.errors.push(SyncError {
                    path,
                    stage: "notes".into(),
                    message: reason,
                });
            }
        }
        Err(message) => {
            failed = true;
            summary.errors.push(SyncError {
                path: env.data.notes().display().to_string(),
                stage: "notes".into(),
                message,
            });
        }
    }
    // Stay outdated after a failure so the next sync retries.
    if !failed {
        db::set_meta(conn, INDEXER_VERSION_KEY, INDEXER_VERSION)?;
        db::set_meta(conn, EXCLUDE_CWD_KEY, &exclusions)?;
    }
    Ok(())
}

fn reindex_one(env: &KbEnv, conn: &Connection, row: &FileRow) -> Result<(), String> {
    let mirror_path = env.data.mirror_path(&row.mirror_rel)?;
    let file = std::fs::File::open(&mirror_path)
        .map_err(|e| format!("cannot open mirror {}: {e}", mirror_path.display()))?;
    let mut indexer = Indexer::new(
        row.source,
        FileMeta::from_path(row.source, Path::new(&row.source_path)),
    );
    // `Indexer::new` starts from byte 0, so its message rows replace the stored ones.
    if row.is_compressed {
        let reader = zstd::stream::Decoder::new(file)
            .map_err(|e| format!("cannot decompress mirror: {e}"))?;
        let tail = super::index::for_each_line_chunk(reader, |chunk| {
            indexer.feed(chunk);
            Ok(())
        })?;
        indexer.feed(&tail.tail);
    } else {
        let upto = u64::try_from(row.mirror_offset).unwrap_or(0);
        super::index::for_each_line_chunk(std::io::Read::take(file, upto), |chunk| {
            indexer.feed(chunk);
            Ok(())
        })?;
    }
    let unknown = serde_json::to_string(indexer.unknown_types()).unwrap_or_else(|_| "{}".into());
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin: {e}"))?;
    let (replace_all, ops) = indexer.take_message_ops();
    db::apply_message_ops(&tx, row.id, replace_all, &ops)?;
    let record = indexer.record();
    db::upsert_session(
        &tx,
        row.id,
        row.source,
        &record,
        &indexer.state_json(),
        &unknown,
    )?;
    db::apply_cwd_exclusion(
        &tx,
        row.id,
        record.cwd.as_deref(),
        &env.exclude_cwd_prefixes,
    )?;
    tx.commit().map_err(|e| format!("commit: {e}"))
}

// ---------------------------------------------------------------------------
// One file
// ---------------------------------------------------------------------------

fn process_file(
    env: &KbEnv,
    conn: &Connection,
    file: &SourceFile,
    prior: Option<&FileRow>,
    log: &SyncLog,
    summary: &mut Summary,
) -> Result<(), String> {
    let mirror_path = env
        .data
        .mirror_path(prior.map_or(file.mirror_rel.as_str(), |r| r.mirror_rel.as_str()))?;
    let seen = Observed {
        size: file.size,
        mtime_ns: file.mtime_ns,
        compressed: file.is_compressed,
    };
    let decision = mirror::decide(&file.path, &mirror_path, prior, &seen)?;
    if decision == Decision::Unchanged {
        if let Some(row) = prior {
            db::mark_present_without_content_change(conn, row.id, &now())?;
        }
        return Ok(());
    }

    let meta = FileMeta::from_path(file.source, &file.path);
    let mut indexer = match (decision, prior) {
        (Decision::Append { from }, Some(row)) => {
            restore_indexer(conn, file.source, &meta, row, &mirror_path, from)?
        }
        _ => Indexer::new(file.source, meta),
    };
    let unknown_before = indexer.unknown_types().clone();
    let bad_before = indexer.bad_lines();

    let mirrored = mirror::execute(&file.path, &mirror_path, decision, &seen, &mut |chunk| {
        indexer.feed(chunk);
        Ok(())
    })?;

    commit_file(
        env,
        conn,
        file,
        prior,
        &mut indexer,
        &mirrored.cursor,
        summary,
    )?;

    summary.changed_files += 1;
    summary.copied_bytes += mirrored.copied_bytes;
    summary.longest_line_bytes = summary.longest_line_bytes.max(mirrored.longest_line);
    summary.bad_lines += indexer.bad_lines() - bad_before;
    if matches!(decision, Decision::Rewrite) || mirrored.rotated_previous {
        summary.rewritten_files += 1;
        log.line(&format!(
            "rewritten source, previous mirror kept aside: {}",
            file.path_str
        ));
    }
    for (kind, count) in indexer.unknown_types() {
        let delta = count - unknown_before.get(kind).copied().unwrap_or(0);
        if delta > 0 {
            *summary.unknown_types.entry(kind.clone()).or_insert(0) += delta;
        }
    }
    Ok(())
}

/// Resume the stored indexer state; if it is missing or unreadable, rebuild it
/// from the mirror itself (the mirror is the source of truth, up to the cursor).
fn restore_indexer(
    conn: &Connection,
    source: Source,
    meta: &FileMeta,
    row: &FileRow,
    mirror_path: &Path,
    upto: u64,
) -> Result<Indexer, String> {
    if let Some(stored) = db::load_indexer_state(conn, row.id)? {
        if let Some(indexer) = Indexer::restore(source, meta.clone(), &stored.indexer_state) {
            // Message offsets must continue exactly where the cursor is.
            if indexer.bytes_fed() == upto {
                return Ok(indexer);
            }
        }
    }
    let mut indexer = Indexer::new(source, meta.clone());
    // Replaying from byte 0: the rows it produces replace the stored ones.
    indexer.mark_rebuilt();
    let file = std::fs::File::open(mirror_path)
        .map_err(|e| format!("cannot reopen mirror {}: {e}", mirror_path.display()))?;
    let reader = std::io::Read::take(file, upto);
    super::index::for_each_line_chunk(reader, |chunk| {
        indexer.feed(chunk);
        Ok(())
    })?;
    Ok(indexer)
}

fn commit_file(
    env: &KbEnv,
    conn: &Connection,
    file: &SourceFile,
    prior: Option<&FileRow>,
    indexer: &mut Indexer,
    cursor: &Cursor,
    summary: &mut Summary,
) -> Result<(), String> {
    let when = now();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin: {e}"))?;
    let file_id = match prior {
        Some(row) => {
            db::update_cursor(&tx, row.id, cursor, &when)?;
            row.id
        }
        None => db::insert_file(
            &tx,
            &NewFile {
                source: file.source,
                source_path: &file.path_str,
                mirror_rel: &file.mirror_rel,
                is_compressed: file.is_compressed,
            },
            cursor,
            &when,
        )?,
    };
    let unknown = serde_json::to_string(indexer.unknown_types()).unwrap_or_else(|_| "{}".into());
    let (replace_all, ops) = indexer.take_message_ops();
    db::apply_message_ops(&tx, file_id, replace_all, &ops)?;
    let record = indexer.record();
    let is_new = db::upsert_session(
        &tx,
        file_id,
        file.source,
        &record,
        &indexer.state_json(),
        &unknown,
    )?;
    db::apply_cwd_exclusion(
        &tx,
        file_id,
        record.cwd.as_deref(),
        &env.exclude_cwd_prefixes,
    )?;
    tx.commit().map_err(|e| format!("commit: {e}"))?;
    if is_new {
        summary.new_sessions += 1;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Codex native titles
// ---------------------------------------------------------------------------

fn refresh_codex_titles(
    conn: &Connection,
    codex_root: &Path,
    summary: &mut Summary,
) -> Result<(), String> {
    let native = codex_titles::load(codex_root);
    if let Some(warning) = native.warning {
        summary.errors.push(SyncError {
            path: codex_root.display().to_string(),
            stage: "title_db".into(),
            message: warning,
        });
    }
    let rows = db::load_codex_titles(conn)?;
    let mut updates = Vec::new();
    for row in &rows {
        let had_native = matches!(
            row.title_source.as_deref(),
            Some("threads" | "session_index")
        );
        match native.titles.get(&row.session_id) {
            Some((title, source)) => {
                if row.title.as_deref() != Some(title.as_str())
                    || row.title_source.as_deref() != Some(source.as_str())
                {
                    updates.push((row.id, Some(title.as_str()), source.as_str()));
                }
            }
            // The native title disappeared (thread deleted): fall back to what
            // the indexer derived from the content.
            None if had_native => {
                updates.push((row.id, row.fallback_title.as_deref(), "first_message"));
            }
            // Otherwise the indexer-derived title is already stored.
            None => {}
        }
    }
    if updates.is_empty() {
        return Ok(());
    }
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin titles: {e}"))?;
    for (id, title, source) in updates {
        db::set_title(&tx, id, title, source)?;
    }
    tx.commit().map_err(|e| format!("commit titles: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    const CODEX_ID: &str = "019e7371-a6b9-7a50-9825-b7acd6255914";
    const CLAUDE_ID: &str = "11111111-2222-3333-4444-555555555555";

    struct Sandbox {
        dir: tempfile::TempDir,
        env: KbEnv,
        claude: PathBuf,
        codex: PathBuf,
    }

    impl Sandbox {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let claude = dir.path().join(".claude");
            let codex = dir.path().join(".codex");
            fs::create_dir_all(claude.join("projects")).unwrap();
            fs::create_dir_all(codex.join("sessions")).unwrap();
            let env = KbEnv {
                data: DataPaths::new(dir.path().join("AgentKB")),
                roots: SourceRoots {
                    claude: Some(claude.clone()),
                    codex: Some(codex.clone()),
                },
                exclude_prefixes: vec![],
                exclude_cwd_prefixes: vec![],
                mirror: None,
                progress: ProgressSink::default(),
                force_reindex: false,
            };
            Self {
                dir,
                env,
                claude,
                codex,
            }
        }

        fn claude_file(&self) -> PathBuf {
            self.claude
                .join(format!("projects/-Users-me-proj/{CLAUDE_ID}.jsonl"))
        }

        fn codex_file(&self) -> PathBuf {
            self.codex.join(format!(
                "sessions/2026/05/29/rollout-2026-05-29T11-14-48-{CODEX_ID}.jsonl"
            ))
        }

        fn codex_archived(&self) -> PathBuf {
            self.codex.join(format!(
                "archived_sessions/rollout-2026-05-29T11-14-48-{CODEX_ID}.jsonl"
            ))
        }

        fn sync(&self) -> Summary {
            run_sync(&self.env, "test").unwrap()
        }

        fn mirror_of(&self, rel: &str) -> PathBuf {
            self.env.data.raw().join(rel)
        }

        fn db(&self) -> Connection {
            db::open(&self.env.data.db()).unwrap()
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn append(path: &Path, text: &str) {
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(10))
            .ok();
    }

    fn claude_text() -> String {
        format!(
            "{}\n{}\n",
            r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","cwd":"/Users/me/proj","message":{"role":"user","content":"hello there"}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"role":"assistant","content":[]}}"#,
        )
    }

    fn codex_text() -> String {
        let meta = format!(
            r#"{{"timestamp":"2026-05-29T11:15:04Z","type":"session_meta","payload":{{"id":"{CODEX_ID}","cwd":"/Users/me/app","originator":"codex-tui","source":"cli"}}}}"#
        );
        let message = r#"{"timestamp":"2026-05-29T11:15:06Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a retry"}]}}"#;
        format!("{meta}\n{message}\n")
    }

    fn session_rows(conn: &Connection) -> Vec<(String, String, i64, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT s.source, s.session_id, s.message_count, f.source_status \
                 FROM sessions s JOIN files f ON f.id = s.file_id ORDER BY s.source, s.session_id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn new_files_are_mirrored_and_indexed() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        write(&sb.codex_file(), &codex_text());

        let s = sb.sync();
        assert_eq!(s.status, "ok", "{:?}", s.errors);
        assert_eq!(
            (s.scanned_files, s.changed_files, s.new_sessions),
            (2, 2, 2)
        );
        assert_eq!(
            fs::read(sb.mirror_of(&format!("claude/projects/-Users-me-proj/{CLAUDE_ID}.jsonl")))
                .unwrap(),
            fs::read(sb.claude_file()).unwrap()
        );
        let rows = session_rows(&sb.db());
        assert_eq!(
            rows[0],
            ("claude".into(), CLAUDE_ID.into(), 2, "present".into())
        );
        assert_eq!(
            rows[1],
            ("codex".into(), CODEX_ID.into(), 1, "present".into())
        );
    }

    #[test]
    fn nothing_changed_copies_nothing_and_touches_no_file() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        let again = sb.sync();
        assert_eq!(
            (again.changed_files, again.copied_bytes, again.new_sessions),
            (0, 0, 0)
        );
        assert_eq!(again.status, "ok");
    }

    #[test]
    fn appends_and_half_lines_are_completed_across_runs_and_stay_byte_identical() {
        let sb = Sandbox::new();
        let src = sb.claude_file();
        write(&src, &claude_text());
        sb.sync();

        append(
            &src,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:03Z","message":{"role":"user","content":"more"}}"#,
        );
        let half = sb.sync();
        assert_eq!(
            half.copied_bytes, 0,
            "an unterminated last line is not copied yet"
        );

        append(&src, "\n");
        let done = sb.sync();
        assert!(done.copied_bytes > 0);
        let rows = session_rows(&sb.db());
        assert_eq!(rows[0].2, 3);

        sb.sync();
        assert_eq!(
            fs::read(sb.mirror_of(&format!("claude/projects/-Users-me-proj/{CLAUDE_ID}.jsonl")))
                .unwrap(),
            fs::read(&src).unwrap()
        );
    }

    #[test]
    fn rewritten_source_keeps_the_old_mirror_and_reindexes() {
        let sb = Sandbox::new();
        let src = sb.claude_file();
        write(&src, &claude_text());
        sb.sync();
        write(&src, "{\"type\":\"user\",\"timestamp\":\"2027-01-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"brand new\"}}\n");
        let s = sb.sync();
        assert_eq!(s.rewritten_files, 1);
        let dir = sb.mirror_of("claude/projects/-Users-me-proj");
        let prev = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".prev-"))
            .count();
        assert_eq!(prev, 1);
        assert_eq!(session_rows(&sb.db())[0].2, 1);
    }

    #[test]
    fn deleted_source_keeps_its_mirror_and_is_marked_deleted() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        fs::remove_file(sb.claude_file()).unwrap();
        let s = sb.sync();
        assert_eq!(s.deleted_sources, 1);
        assert_eq!(session_rows(&sb.db())[0].3, "deleted");
        assert!(sb
            .mirror_of(&format!("claude/projects/-Users-me-proj/{CLAUDE_ID}.jsonl"))
            .exists());
        // And it stays that way without churn.
        assert_eq!(sb.sync().deleted_sources, 0);
        // A source that comes back is present again.
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        assert_eq!(session_rows(&sb.db())[0].3, "present");
    }

    #[test]
    fn codex_moved_to_archive_is_one_session_not_deleted_and_not_duplicated() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        sb.sync();

        fs::create_dir_all(sb.codex_archived().parent().unwrap()).unwrap();
        fs::rename(sb.codex_file(), sb.codex_archived()).unwrap();
        let s = sb.sync();
        assert_eq!(
            (s.moved_sources, s.deleted_sources, s.new_sessions),
            (1, 0, 0),
            "{:?}",
            s.errors
        );

        let conn = sb.db();
        let rows = session_rows(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "present");
        let (path, rel): (String, String) = conn
            .query_row("SELECT source_path, mirror_rel FROM files", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(path, sb.codex_archived().to_string_lossy());
        assert!(
            rel.starts_with("codex/sessions/"),
            "mirror stays where it was first made: {rel}"
        );
        assert!(!sb.mirror_of("codex/archived_sessions").exists());
    }

    fn compress_in_place(plain: &Path, text: &str) -> PathBuf {
        let zst = PathBuf::from(format!("{}.zst", plain.display()));
        fs::write(&zst, zstd::encode_all(text.as_bytes(), 3).unwrap()).unwrap();
        fs::remove_file(plain).unwrap();
        zst
    }

    fn message_count_of(conn: &Connection, word: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE text LIKE ?1",
            [format!("%{word}%")],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_rollout_compressed_in_place_is_the_same_session_not_a_new_one() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        sb.sync();
        let plain_mirror = sb.mirror_of(&format!(
            "codex/sessions/2026/05/29/rollout-2026-05-29T11-14-48-{CODEX_ID}.jsonl"
        ));
        assert!(plain_mirror.is_file());

        let zst = compress_in_place(&sb.codex_file(), &codex_text());
        let s = sb.sync();
        assert_eq!(
            (s.moved_sources, s.deleted_sources, s.new_sessions),
            (1, 0, 0),
            "{:?}",
            s.errors
        );
        let conn = sb.db();
        let rows = session_rows(&conn);
        assert_eq!(rows.len(), 1, "one session, not two");
        assert_eq!(rows[0].3, "present", "never shown as deleted");
        let (path, compressed): (String, i64) = conn
            .query_row("SELECT source_path, is_compressed FROM files", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((path.as_str(), compressed), (&*zst.to_string_lossy(), 1));
        assert_eq!(message_count_of(&conn, "retry"), 1, "indexed once");
        assert_eq!(found(&sb, "retry"), 1, "found once");
        assert!(plain_mirror.is_file(), "the plain mirror is kept");
        assert!(sb
            .mirror_of(&format!(
                "codex/sessions/2026/05/29/rollout-2026-05-29T11-14-48-{CODEX_ID}.jsonl.zst"
            ))
            .is_file());

        // Nothing more to do on the next pass.
        let again = sb.sync();
        assert_eq!(
            (again.changed_files, again.moved_sources, again.new_sessions),
            (0, 0, 0)
        );
    }

    #[test]
    fn a_compressed_rollout_that_was_unpacked_again_is_also_the_same_session() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        let zst = compress_in_place(&sb.codex_file(), &codex_text());
        sb.sync();
        assert!(zst.is_file());

        fs::remove_file(&zst).unwrap();
        write(&sb.codex_file(), &codex_text());
        let s = sb.sync();
        assert_eq!(
            (s.moved_sources, s.deleted_sources),
            (1, 0),
            "{:?}",
            s.errors
        );
        assert_eq!(session_rows(&sb.db()).len(), 1);
        assert_eq!(found(&sb, "retry"), 1);
    }

    #[test]
    fn a_compressed_file_with_other_content_is_a_new_file_and_nothing_is_joined() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        sb.sync();

        let other = codex_text().replace("add a retry", "write a poem");
        compress_in_place(&sb.codex_file(), &other);
        let s = sb.sync();
        assert_eq!(
            (s.moved_sources, s.deleted_sources),
            (0, 1),
            "{:?}",
            s.errors
        );
        assert_eq!(s.new_sessions, 1, "{:?}", s.errors);
        let rows = session_rows(&sb.db());
        assert_eq!(
            rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>(),
            ["deleted", "present"]
        );
        // And it stays that way: the next pass does not join them.
        let again = sb.sync();
        assert_eq!(again.joined_twins, 0);
        assert_eq!(session_rows(&sb.db()).len(), 2);
    }

    #[test]
    fn sessions_stored_twice_by_an_older_version_are_joined_once_and_both_mirrors_stay() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        sb.sync();
        // What an older version left: the plain row "deleted", the compressed
        // file taken for a new session.
        sb.db()
            .execute(
                "UPDATE files SET source_status='deleted', deleted_at='2026-10-15T00:00:00Z'",
                [],
            )
            .unwrap();
        compress_in_place(&sb.codex_file(), &codex_text());
        sb.sync();
        let rows = session_rows(&sb.db());
        assert_eq!(rows.len(), 2, "the duplicate exists: {rows:?}");
        assert_eq!(found(&sb, "retry"), 2, "the duplicate is searchable twice");
        let conn = sb.db();
        assert_eq!(message_count_of(&conn, "retry"), 2);
        drop(conn);

        let s = sb.sync();
        assert_eq!(s.joined_twins, 1, "{:?}", s.errors);
        let conn = sb.db();
        let rows = session_rows(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "present");
        assert_eq!(message_count_of(&conn, "retry"), 1, "indexed once");
        drop(conn);
        assert_eq!(found(&sb, "retry"), 1, "found once");
        let conn = sb.db();
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1);
        for name in ["jsonl", "jsonl.zst"] {
            assert!(
                sb.mirror_of(&format!(
                    "codex/sessions/2026/05/29/rollout-2026-05-29T11-14-48-{CODEX_ID}.{name}"
                ))
                .is_file(),
                "{name} mirror stays"
            );
        }
        let copies: Vec<_> = fs::read_dir(sb.env.data.db().parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("kb.db.before-twin-join-")
            })
            .collect();
        assert_eq!(copies.len(), 1, "kb.db was copied before the join");

        let again = sb.sync();
        assert_eq!(again.joined_twins, 0);
    }

    #[test]
    fn move_is_resolved_before_deletion_so_no_intermediate_deleted_state() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        sb.sync();
        let rows = db::load_files(&sb.db()).unwrap();

        fs::create_dir_all(sb.codex_archived().parent().unwrap()).unwrap();
        fs::rename(sb.codex_file(), sb.codex_archived()).unwrap();
        let found = discover::discover(&sb.env.roots, &[]);
        let plan = reconcile(&rows, &found, &[]);
        assert_eq!(plan.deleted, Vec::<i64>::new());
        assert_eq!(
            plan.moves,
            vec![(
                rows[0].id,
                sb.codex_archived().to_string_lossy().into_owned()
            )]
        );

        // Gone for real (no twin anywhere): deleted, not moved.
        fs::remove_file(sb.codex_archived()).unwrap();
        let found = discover::discover(&sb.env.roots, &[]);
        let plan = reconcile(&rows, &found, &[]);
        assert_eq!((plan.moves.len(), plan.deleted.len()), (0, 1));
    }

    #[test]
    fn session_first_seen_in_the_archive_mirrors_under_archived_sessions() {
        let sb = Sandbox::new();
        write(&sb.codex_archived(), &codex_text());
        let s = sb.sync();
        assert_eq!(s.new_sessions, 1);
        assert!(sb
            .mirror_of(&format!(
                "codex/archived_sessions/rollout-2026-05-29T11-14-48-{CODEX_ID}.jsonl"
            ))
            .is_file());
    }

    #[test]
    fn an_unreadable_source_root_never_declares_history_deleted() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        fs::remove_dir_all(sb.claude.join("projects")).unwrap();
        // projects/ gone entirely under an existing root: a complete, empty listing.
        assert_eq!(sb.sync().deleted_sources, 1);

        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        fs::remove_dir_all(&sb.claude).unwrap();
        // The whole root vanished (unmounted?): nothing may be marked deleted.
        let s = sb.sync();
        assert_eq!(s.deleted_sources, 0);
        assert_eq!(session_rows(&sb.db())[0].3, "present");
    }

    #[test]
    fn crash_after_mirror_append_before_commit_recovers_without_duplicates() {
        let sb = Sandbox::new();
        let src = sb.claude_file();
        write(&src, &claude_text());
        sb.sync();
        let rel = format!("claude/projects/-Users-me-proj/{CLAUDE_ID}.jsonl");
        let committed = fs::read(sb.mirror_of(&rel)).unwrap();

        // The "crashed" run: new bytes reached the mirror but the cursor did not move.
        append(&src, "{\"type\":\"user\",\"timestamp\":\"2026-01-01T00:00:09Z\",\"message\":{\"role\":\"user\",\"content\":\"x\"}}\n");
        let mut orphan = fs::OpenOptions::new()
            .append(true)
            .open(sb.mirror_of(&rel))
            .unwrap();
        orphan.write_all(b"{\"type\":\"user\",\"timestamp\":\"2026-01-01T00:00:09Z\",\"message\":{\"role\":\"user\",\"content\":\"x\"}}\n").unwrap();
        drop(orphan);
        assert!(fs::metadata(sb.mirror_of(&rel)).unwrap().len() > committed.len() as u64);

        let s = sb.sync();
        assert_eq!(s.status, "ok", "{:?}", s.errors);
        assert_eq!(
            fs::read(sb.mirror_of(&rel)).unwrap(),
            fs::read(&src).unwrap()
        );
        assert_eq!(
            session_rows(&sb.db())[0].2,
            3,
            "the line is indexed exactly once"
        );
    }

    #[test]
    fn missing_indexer_state_is_rebuilt_from_the_mirror() {
        let sb = Sandbox::new();
        let src = sb.claude_file();
        write(&src, &claude_text());
        sb.sync();
        sb.db()
            .execute("UPDATE sessions SET indexer_state='garbage'", [])
            .unwrap();
        append(&src, "{\"type\":\"user\",\"timestamp\":\"2026-01-01T00:00:09Z\",\"message\":{\"role\":\"user\",\"content\":\"x\"}}\n");
        let s = sb.sync();
        assert_eq!(s.status, "ok", "{:?}", s.errors);
        assert_eq!(session_rows(&sb.db())[0].2, 3);
    }

    #[test]
    fn concurrent_second_run_is_blocked_by_the_lock() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        let _held = SyncLock::acquire(&sb.env.data.lock(), Duration::ZERO)
            .unwrap()
            .unwrap();

        let env = sb.env.clone();
        let started = Instant::now();
        let second = std::thread::spawn(move || run_sync(&env, "test").unwrap())
            .join()
            .unwrap();
        assert_eq!(second.status, "skipped");
        assert_eq!(second.reason.as_deref(), Some("locked"));
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "retries before giving up"
        );
        assert!(
            !sb.env.data.db().exists(),
            "a skipped run must not touch the database"
        );
    }

    #[test]
    fn three_syncs_end_with_every_mirror_identical_to_its_source() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        write(&sb.codex_file(), &codex_text());
        sb.sync();
        append(&sb.claude_file(), "{\"type\":\"assistant\",\"timestamp\":\"2026-01-01T00:00:05Z\",\"message\":{\"role\":\"assistant\",\"content\":[]}}\n");
        append(&sb.codex_file(), "{\"timestamp\":\"2026-05-29T11:16:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n");
        sb.sync();
        let third = sb.sync();
        assert_eq!(third.copied_bytes, 0);

        for (src, rel) in [
            (
                sb.claude_file(),
                format!("claude/projects/-Users-me-proj/{CLAUDE_ID}.jsonl"),
            ),
            (
                sb.codex_file(),
                format!("codex/sessions/2026/05/29/rollout-2026-05-29T11-14-48-{CODEX_ID}.jsonl"),
            ),
        ] {
            assert_eq!(
                fs::read(sb.mirror_of(&rel)).unwrap(),
                fs::read(&src).unwrap(),
                "{rel}"
            );
        }
    }

    #[test]
    fn codex_titles_come_from_the_state_db_and_follow_renames() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        let state = sb.codex.join("state_5.sqlite");
        let conn = Connection::open(&state).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA journal_mode=WAL; CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL);
             INSERT INTO threads VALUES ('{CODEX_ID}', 'Native title');"
        ))
        .unwrap();
        drop(conn);

        let s = sb.sync();
        assert_eq!(s.status, "ok", "{:?}", s.errors);
        let title = |db: &Connection| -> (String, String) {
            db.query_row("SELECT title, title_source FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
        };
        assert_eq!(title(&sb.db()), ("Native title".into(), "threads".into()));

        let conn = Connection::open(&state).unwrap();
        conn.execute("UPDATE threads SET title='Renamed'", [])
            .unwrap();
        drop(conn);
        // New content must not clobber the native title either.
        append(&sb.codex_file(), "{\"timestamp\":\"2026-05-29T11:16:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n");
        sb.sync();
        assert_eq!(title(&sb.db()), ("Renamed".into(), "threads".into()));
    }

    #[test]
    fn a_native_title_that_is_a_pasted_status_block_gives_way_to_the_first_message() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        let conn = Connection::open(sb.codex.join("state_5.sqlite")).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL);
             INSERT INTO threads VALUES ('{CODEX_ID}',
               'Version: 2.1.296 Session name: demo Session ID: 52261b64-a33d-43c1-9bb');"
        ))
        .unwrap();
        drop(conn);
        let s = sb.sync();
        assert_eq!(s.status, "ok", "{:?}", s.errors);
        let (title, source): (String, String) = sb
            .db()
            .query_row("SELECT title, title_source FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            (title.as_str(), source.as_str()),
            ("add a retry", "first_message")
        );
    }

    // ----- message index -------------------------------------------------

    fn user_line(ts: &str, text: &str) -> String {
        serde_json::json!({"type":"user","timestamp":ts,"cwd":"/Users/me/proj",
            "message":{"role":"user","content":text}})
        .to_string()
            + "\n"
    }

    fn assistant_line(ts: &str, blocks: serde_json::Value) -> String {
        serde_json::json!({"type":"assistant","timestamp":ts,
            "message":{"role":"assistant","content":blocks}})
        .to_string()
            + "\n"
    }

    fn result_line(ts: &str, id: &str, text: &str) -> String {
        serde_json::json!({"type":"user","timestamp":ts,
            "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":text}]}})
        .to_string()
            + "\n"
    }

    fn tool_line(ts: &str, id: &str, command: &str) -> String {
        assistant_line(
            ts,
            serde_json::json!([{"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}]),
        )
    }

    fn message_rows(conn: &Connection) -> Vec<(String, u64, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT s.session_id, m.seq, m.kind, m.text FROM messages m \
                 JOIN sessions s ON s.file_id = m.file_id ORDER BY s.session_id, m.seq",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                u64::try_from(r.get::<_, i64>(1)?).unwrap(),
                r.get(2)?,
                r.get(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    fn found(sb: &Sandbox, query: &str) -> usize {
        let params = crate::kb::search::SearchParams {
            query: query.to_string(),
            limit: Some(20),
            ..Default::default()
        };
        crate::kb::search::search_in(&sb.env.data, &params)
            .unwrap()
            .matched_sessions
    }

    fn keyed_user(ts: &str, uuid: &str, text: &str) -> String {
        serde_json::json!({"type":"user","uuid":uuid,"timestamp":ts,"cwd":"/Users/me/proj",
            "message":{"role":"user","content":text}})
        .to_string()
            + "\n"
    }

    fn keyed_assistant(ts: &str, uuid: &str, text: &str) -> String {
        serde_json::json!({"type":"assistant","uuid":uuid,"timestamp":ts,
            "message":{"role":"assistant","content":[{"type":"text","text":text}]}})
        .to_string()
            + "\n"
    }

    fn numbered(conn: &Connection) -> Vec<(u64, Option<String>)> {
        let mut stmt = conn
            .prepare("SELECT seq, msg_key FROM messages ORDER BY seq")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((u64::try_from(r.get::<_, i64>(0)?).unwrap(), r.get(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Notes point at messages by `seq` (and the `msg_key` of the first one).
    /// If the two ways of building the index ever numbered differently, every
    /// existing note would point at the wrong message.
    #[test]
    fn seq_and_message_keys_are_the_same_after_incremental_builds_and_a_reindex() {
        let mut sb = Sandbox::new();
        let path = sb.claude_file();
        write(
            &path,
            &(keyed_user("2026-01-01T00:00:01Z", "u1", "first question")
                + &keyed_assistant("2026-01-01T00:00:02Z", "a1", "first answer")),
        );
        sb.sync();
        // A kb lookup turn (not indexed, but it still takes a number), then more.
        append(
            &path,
            &(keyed_user("2026-01-01T00:01:00Z", "u2", "second question")
                + &tool_line("2026-01-01T00:01:02Z", "t1", "agentkb --kb-search lock")
                + &keyed_assistant("2026-01-01T00:01:05Z", "a2", "second answer")),
        );
        sb.sync();
        append(
            &path,
            &(keyed_user("2026-01-01T00:02:00Z", "u3", "third question")
                + &keyed_assistant("2026-01-01T00:02:02Z", "a3", "third answer")),
        );
        sb.sync();
        let incremental = numbered(&sb.db());
        // The lookup turn is dropped as a pure Q&A turn, but its messages
        // still used up numbers 3-5.
        assert_eq!(
            incremental,
            vec![
                (1, Some("u1".into())),
                (2, Some("a1".into())),
                (6, Some("u3".into())),
                (7, Some("a3".into())),
            ]
        );

        sb.env.force_reindex = true;
        let rebuilt = sb.sync();
        assert_eq!(rebuilt.status, "ok", "{:?}", rebuilt.errors);
        assert_eq!(numbered(&sb.db()), incremental);
    }

    #[test]
    fn a_reindex_restores_the_notes_from_their_files() {
        let mut sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        let file = crate::kb::notes::testing::sample("claude", CLAUDE_ID, 2);
        crate::kb::notes::write(&sb.env.data, &file).unwrap();
        // The database has never seen the file: an ordinary sync picks it up.
        sb.sync();
        let notes_in = |sb: &Sandbox| -> i64 {
            sb.db()
                .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(notes_in(&sb), 2);
        // Lose the tables, then rebuild everything.
        sb.db()
            .execute_batch("DROP TABLE notes_fts; DROP TABLE notes; DROP TABLE digest_sessions;")
            .unwrap();
        sb.env.force_reindex = true;
        let summary = sb.sync();
        assert_eq!(summary.status, "ok", "{:?}", summary.errors);
        assert_eq!(notes_in(&sb), 2);
        assert_eq!(
            crate::kb::notes::digested_seq(&sb.db(), "claude", CLAUDE_ID).unwrap(),
            Some(42)
        );
    }

    fn search_notes_with(
        sb: &Sandbox,
        query: &str,
        notes: Option<usize>,
    ) -> crate::kb::search::SearchResponse {
        let params = crate::kb::search::SearchParams {
            query: query.to_string(),
            limit: Some(20),
            notes,
            ..Default::default()
        };
        crate::kb::search::search_in(&sb.env.data, &params).unwrap()
    }

    #[test]
    fn notes_come_with_the_search_ahead_of_the_messages_and_can_be_turned_off() {
        let sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &(user_line("2026-01-01T00:00:01Z", "线程池里出现死锁怎么办")
                + &assistant_line(
                    "2026-01-01T00:00:02Z",
                    serde_json::json!([{"type":"text","text":"按固定顺序加锁就能避免死锁"}]),
                )),
        );
        sb.sync();
        let file = crate::kb::notes::testing::sample("claude", CLAUDE_ID, 2);
        crate::kb::notes::write(&sb.env.data, &file).unwrap();
        sb.sync();

        let response = search_notes_with(&sb, "死锁", None);
        assert_eq!(response.notes.len(), 2);
        let first = &response.notes[0];
        assert_eq!(first.kind, "note");
        assert_eq!(first.session.session_id, CLAUDE_ID);
        assert_eq!(first.evidence.seq_from, 3);
        assert_eq!(first.evidence.msg_key.as_deref(), Some("uuid-3"));
        assert!(first
            .show
            .contains(&format!("--kb-show claude:{CLAUDE_ID} --around 3")));
        assert!(first.resume.available);
        assert_eq!(
            response.results.len(),
            1,
            "the message hits are still there"
        );
        assert!(response.results[0].has_note);

        // The command-line JSON keeps what an agent needs and nothing of the
        // human-only detail; `notes` is only ever an added key.
        let json = crate::kb::search::agent_json(&response, false);
        let note = &json["notes"][0];
        assert!(note["show"].as_str().unwrap().contains("--kb-show"));
        assert!(note["conclusion"].is_string() && note["title"].is_string());
        assert!(note["evidence"].get("msg_key").is_none());
        assert!(json["results"][0].get("has_note").is_some());
        let verbose = crate::kb::search::agent_json(&response, true);
        assert!(verbose["notes"][0]["evidence"].get("msg_key").is_some());

        let none = search_notes_with(&sb, "死锁", Some(0));
        assert!(none.notes.is_empty());
        assert_eq!(none.results.len(), 1);
        let one = search_notes_with(&sb, "死锁", Some(1));
        assert_eq!(one.notes.len(), 1);

        let text = crate::kb::search::format_search_text(&response);
        assert!(text.find("[note 1]").unwrap() < text.find("1. [claude").unwrap());

        // Notes of a session whose working directory is excluded are hidden too.
        let mut sb = sb;
        sb.env.exclude_cwd_prefixes = vec!["/Users/me/proj".into()];
        sb.sync();
        assert!(search_notes_with(&sb, "死锁", None).notes.is_empty());
    }

    #[test]
    fn a_note_found_only_by_its_keywords_is_returned_even_when_no_message_matches() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        let mut file = crate::kb::notes::testing::sample("claude", CLAUDE_ID, 1);
        file.entries[0].keywords = vec!["WindowServer".into(), "IOSurface".into()];
        crate::kb::notes::write(&sb.env.data, &file).unwrap();
        sb.sync();
        let response = search_notes_with(&sb, "IOSurface", None);
        assert!(response.results.is_empty());
        assert_eq!(response.notes.len(), 1);
        assert!(
            response.hint.is_none(),
            "no 'no match' hint when a note matched"
        );
        // Relaxed matching: one of two words is not enough on its own at 0.4 coverage, two of three is.
        assert_eq!(
            search_notes_with(&sb, "WindowServer 内存泄漏 IOSurface", None)
                .notes
                .len(),
            1
        );
        assert!(search_notes_with(&sb, "完全无关的内容 其他", None)
            .notes
            .is_empty());
    }

    #[test]
    fn the_digest_work_directory_is_always_excluded_from_the_index() {
        std::env::set_var("CCHV_TEST_HOME", "/tmp/agentkb-test-home");
        let config = KbConfig {
            data_dir: PathBuf::from("/data/AgentKB"),
            ..KbConfig::default()
        };
        let env = KbEnv::from_config(&config);
        assert!(env
            .exclude_cwd_prefixes
            .contains(&"/data/AgentKB/digest-work".to_string()));
        assert!(crate::kb::db::is_cwd_excluded(
            Some("/data/AgentKB/digest-work"),
            &env.exclude_cwd_prefixes
        ));
    }

    #[test]
    fn messages_are_searchable_and_an_incremental_build_equals_a_rebuild() {
        let mut sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &(user_line("2026-01-01T00:00:01Z", "连接池耗尽怎么办")
                + &assistant_line(
                    "2026-01-01T00:00:02Z",
                    serde_json::json!([{"type":"text","text":"调大 maxActive，并检查 HikariCP"}]),
                )),
        );
        write(&sb.codex_file(), &codex_text());
        let first = sb.sync();
        assert_eq!(first.status, "ok", "{:?}", first.errors);
        assert_eq!(found(&sb, "连接池"), 1);
        assert_eq!(found(&sb, "HikariCP"), 1);
        assert_eq!(found(&sb, "add retry"), 1, "codex messages too");

        append(
            &sb.claude_file(),
            &(user_line("2026-01-01T00:00:03Z", "再看看死锁")
                + &tool_line("2026-01-01T00:00:04Z", "t1", "jstack 123")
                + &result_line("2026-01-01T00:00:05Z", "t1", "BLOCKED waiting for lock")),
        );
        sb.sync();
        assert_eq!(found(&sb, "jstack"), 1, "tool calls are indexed");
        assert_eq!(found(&sb, "BLOCKED"), 1, "tool results are indexed");
        let incremental = message_rows(&sb.db());
        assert_eq!(incremental.len(), 6);

        let scores = |sb: &Sandbox| -> Vec<(String, f64)> {
            crate::kb::search::search_in(
                &sb.env.data,
                &crate::kb::search::SearchParams {
                    query: "连接池 jstack".into(),
                    mode: Some("any".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .results
            .iter()
            .map(|r| (r.session.session_id.clone(), r.score))
            .collect()
        };
        let before = scores(&sb);
        assert!(!before.is_empty());

        // A full rebuild from the mirror gives the very same rows.
        sb.env.force_reindex = true;
        let rebuilt = sb.sync();
        assert_eq!(rebuilt.status, "ok", "{:?}", rebuilt.errors);
        assert_eq!(message_rows(&sb.db()), incremental);
        assert_eq!(found(&sb, "连接池"), 1);
        // …and the same relevance scores, however often it is rebuilt.
        assert_eq!(scores(&sb), before);
        sb.sync();
        let again = sb.sync();
        assert_eq!(again.status, "ok");
        assert_eq!(scores(&sb), before);
        let fts: i64 = sb
            .db()
            .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts, 6, "no orphaned index entries");
    }

    #[test]
    fn a_rewritten_source_replaces_its_messages() {
        let sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &(user_line("2026-01-01T00:00:01Z", "旧的问题 oldword")
                + &assistant_line(
                    "2026-01-01T00:00:02Z",
                    serde_json::json!([{"type":"text","text":"旧的回答"}]),
                )),
        );
        sb.sync();
        assert_eq!(found(&sb, "oldword"), 1);
        // Shorter and different: a rewrite, not an append.
        write(
            &sb.claude_file(),
            &user_line("2026-02-01T00:00:01Z", "新的问题 newword"),
        );
        let s = sb.sync();
        assert_eq!(s.rewritten_files, 1);
        assert_eq!(found(&sb, "newword"), 1);
        assert_eq!(found(&sb, "oldword"), 0, "the old text left the index");
        let conn = sb.db();
        let (rows, fts): (i64, i64) = (
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
                .unwrap(),
        );
        assert_eq!((rows, fts), (1, 1));
    }

    #[test]
    fn lost_indexer_state_replays_the_mirror_without_duplicating_rows() {
        let sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &user_line("2026-01-01T00:00:01Z", "第一条 alpha"),
        );
        sb.sync();
        sb.db()
            .execute("UPDATE sessions SET indexer_state = '{}'", [])
            .unwrap();
        append(
            &sb.claude_file(),
            &user_line("2026-01-01T00:00:02Z", "第二条 beta"),
        );
        let s = sb.sync();
        assert_eq!(s.status, "ok", "{:?}", s.errors);
        let rows = message_rows(&sb.db());
        assert_eq!(
            rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>(),
            vec!["第一条 alpha", "第二条 beta"]
        );
        assert_eq!(found(&sb, "alpha"), 1);
    }

    #[test]
    fn secrets_never_reach_the_index() {
        let sb = Sandbox::new();
        let key = "sk-ant-api03-AbCdEfGhIjKlMnOp1234";
        write(
            &sb.claude_file(),
            &(user_line(
                "2026-01-01T00:00:01Z",
                &format!("deploy with DB_PASSWORD=hunter2 and {key}"),
            ) + &tool_line("2026-01-01T00:00:02Z", "t1", "env")
                + &result_line(
                    "2026-01-01T00:00:03Z",
                    "t1",
                    &format!("TOKEN_VALUE\nAuthorization: Bearer abcdef1234567890xyz\n{key}"),
                )),
        );
        sb.sync();
        let conn = sb.db();
        for row in message_rows(&conn) {
            for secret in [key, "hunter2", "abcdef1234567890xyz"] {
                assert!(!row.3.contains(secret), "{} leaks {secret}", row.3);
            }
        }
        assert_eq!(found(&sb, "hunter2"), 0);
        assert_eq!(found(&sb, "deploy"), 1);
    }

    #[test]
    fn searching_the_history_does_not_make_it_searchable() {
        let sb = Sandbox::new();
        let kb = "/Applications/AgentKB.app/Contents/MacOS/AgentKB --kb-search \"deadlock\"";
        // Two Q&A turns that only used the knowledge base…
        let mut text = String::new();
        for (n, id) in ["k1", "k2"].iter().enumerate() {
            let ts = format!("2026-01-01T00:0{n}:00Z");
            text += &user_line(&ts, "之前那个 zebra 问题怎么解决的");
            text += &tool_line(&ts, id, kb);
            text += &result_line(
                &ts,
                id,
                "{\"agentkb_marker\":\"agentkb-result-v1\",\"x\":\"zebra\"}",
            );
            text += &assistant_line(
                &ts,
                serde_json::json!([{"type":"text","text":"当时是 zebra 配置错了"}]),
            );
        }
        write(&sb.claude_file(), &text);
        sb.sync();
        assert_eq!(found(&sb, "zebra"), 0, "pure Q&A turns are not indexed");

        // …then a turn that searched and went on to change things.
        append(
            &sb.claude_file(),
            &(user_line("2026-01-01T01:00:00Z", "照着之前的 giraffe 方案修一下")
                + &tool_line("2026-01-01T01:00:01Z", "k3", kb)
                + &result_line(
                    "2026-01-01T01:00:02Z",
                    "k3",
                    "{\"agentkb_marker\":\"agentkb-result-v1\"}",
                )
                + &tool_line("2026-01-01T01:00:03Z", "b1", "git commit -m flamingo")
                + &result_line("2026-01-01T01:00:04Z", "b1", "done flamingo")
                + &assistant_line(
                    "2026-01-01T01:00:05Z",
                    serde_json::json!([{"type":"text","text":"已提交 flamingo"}]),
                )),
        );
        sb.sync();
        assert_eq!(
            found(&sb, "giraffe"),
            1,
            "the question of a working turn is kept"
        );
        assert_eq!(found(&sb, "flamingo"), 1, "the work is searchable");
        assert_eq!(found(&sb, "zebra"), 0);
        assert_eq!(
            found(&sb, "agentkb_marker"),
            0,
            "kb output is never indexed"
        );
    }

    #[test]
    fn messages_survive_the_source_being_deleted() {
        let sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &user_line("2026-01-01T00:00:01Z", "unique-wombat question"),
        );
        sb.sync();
        fs::remove_file(sb.claude_file()).unwrap();
        sb.sync();
        assert_eq!(found(&sb, "wombat"), 1);
        let params = crate::kb::search::SearchParams {
            query: "wombat".into(),
            ..Default::default()
        };
        let hit = &crate::kb::search::search_in(&sb.env.data, &params)
            .unwrap()
            .results[0];
        assert_eq!(hit.session.source_status, "deleted");
        assert!(!hit.resume.available);
    }

    #[test]
    fn show_returns_the_neighbours_and_full_reads_the_original_line() {
        let sb = Sandbox::new();
        let long = format!("{}TAILMARK", "详细说明 ".repeat(5000));
        let mut text = String::new();
        for n in 1..=6 {
            text += &user_line(&format!("2026-01-01T00:00:0{n}Z"), &format!("问题 {n}"));
            text += &assistant_line(
                &format!("2026-01-01T00:01:0{n}Z"),
                serde_json::json!([{"type":"text","text": if n == 3 { long.clone() } else { format!("回答 {n}") }}]),
            );
        }
        write(&sb.claude_file(), &text);
        sb.sync();
        let index = crate::kb::search::Index::open(&sb.env.data).unwrap();
        let selector = format!("claude:{CLAUDE_ID}");

        let shown = crate::kb::search::show(
            &index,
            &crate::kb::search::ShowParams {
                session: selector.clone(),
                around: Some(6),
                context: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let seqs: Vec<u64> = shown.messages.iter().map(|m| m.seq).collect();
        assert_eq!(seqs, vec![5, 6, 7], "one visible message either side");
        assert!(shown.messages[1].focus);
        assert_eq!(shown.range.total_messages, 12);
        assert!(shown.resume.command.unwrap().contains("claude --resume"));

        let capped = crate::kb::search::show(
            &index,
            &crate::kb::search::ShowParams {
                session: CLAUDE_ID[..8].to_string(),
                around: Some(6),
                context: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(capped.messages[0].truncated, "default output is capped");
        assert!(!capped.messages[0].text.contains("TAILMARK"));

        // 16 KB cap on the index, but `--full` re-reads the mirror line.
        let full = crate::kb::search::show(
            &index,
            &crate::kb::search::ShowParams {
                session: selector,
                around: Some(6),
                context: Some(0),
                full: Some(true),
                max_chars: Some(1_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            full.messages[0].text.ends_with("TAILMARK"),
            "{}",
            &full.messages[0].text[full.messages[0].text.len().saturating_sub(40)..]
        );
        assert!(!full.messages[0].truncated);

        let missing = crate::kb::search::show(
            &index,
            &crate::kb::search::ShowParams {
                session: "claude:99999999".into(),
                ..Default::default()
            },
        );
        assert!(missing.is_err());
    }

    #[test]
    fn filters_relaxed_matching_and_grouping() {
        let sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &(user_line("2026-01-01T00:00:01Z", "redis 连接池 耗尽")
                + &assistant_line(
                    "2026-01-01T00:00:02Z",
                    serde_json::json!([{"type":"text","text":"redis 连接池 要调大"}]),
                )
                + &assistant_line(
                    "2026-01-01T00:00:03Z",
                    serde_json::json!([{"type":"text","text":"redis 超时另说"}]),
                )),
        );
        write(&sb.codex_file(), &codex_text());
        sb.sync();
        let search = |query: &str, tweak: &dyn Fn(&mut crate::kb::search::SearchParams)| {
            let mut params = crate::kb::search::SearchParams {
                query: query.to_string(),
                ..Default::default()
            };
            tweak(&mut params);
            crate::kb::search::search_in(&sb.env.data, &params).unwrap()
        };
        let all = search("redis 连接池", &|p| p.mode = Some("all".into()));
        assert_eq!((all.mode_used, all.returned), ("all", 1));
        // Auto relaxes when the strict pass finds fewer than three sessions,
        // and the session that has every word still comes first.
        let auto = search("redis 连接池", &|_| {});
        assert_eq!(
            (auto.mode_used, auto.results[0].session.source.as_str()),
            ("any", "claude")
        );
        assert_eq!(
            all.results[0].hit_messages, 2,
            "messages of one session are merged"
        );
        assert_eq!(all.results[0].hits.len(), 2);
        assert!(!all.results[0].hits[0].highlights.is_empty());

        // A word nobody has: strict finds nothing, relaxed falls back to the rest.
        let relaxed = search("redis nonexistentword", &|_| {});
        assert_eq!((relaxed.mode_used, relaxed.returned), ("any", 1));
        let strict = search("redis nonexistentword", &|p| p.mode = Some("all".into()));
        assert_eq!(strict.returned, 0);
        assert!(strict.hint.is_some());

        assert_eq!(
            search("redis", &|p| p.source = Some("codex".into())).returned,
            0
        );
        assert_eq!(
            search("redis", &|p| p.source = Some("claude".into())).returned,
            1
        );
        assert_eq!(
            search("redis", &|p| p.project = Some("PROJ".into())).returned,
            1
        );
        assert_eq!(
            search("redis", &|p| p.project = Some("other".into())).returned,
            0
        );
        assert_eq!(
            search("redis", &|p| p.since = Some("2030-01-01T00:00:00Z".into())).returned,
            0
        );
        assert_eq!(
            search("redis", &|p| p.kinds = Some(vec!["tool_use".into()])).returned,
            0
        );
        assert_eq!(
            search("redis", &|p| p.per_session = Some(1)).results[0]
                .hits
                .len(),
            1
        );
        assert!(search("redis", &|_| {}).data_as_of.is_some());
    }

    fn search_with(
        sb: &Sandbox,
        query: &str,
        tweak: &dyn Fn(&mut crate::kb::search::SearchParams),
    ) -> crate::kb::search::SearchResponse {
        let mut params = crate::kb::search::SearchParams {
            query: query.to_string(),
            ..Default::default()
        };
        tweak(&mut params);
        crate::kb::search::search_in(&sb.env.data, &params).unwrap()
    }

    fn subagent_file(sb: &Sandbox, name: &str) -> PathBuf {
        sb.claude.join(format!(
            "projects/-Users-me-proj/{CLAUDE_ID}/subagents/{name}.jsonl"
        ))
    }

    #[test]
    fn sub_agent_hits_fold_into_the_session_that_started_them() {
        let sb = Sandbox::new();
        // The parent itself does not mention the word; its two sub-agents do.
        write(
            &sb.claude_file(),
            &user_line("2026-01-01T00:00:01Z", "start"),
        );
        for (name, at) in [("agent-a", "02"), ("agent-b", "03")] {
            write(
                &subagent_file(&sb, name),
                &(user_line(
                    &format!("2026-01-01T00:00:{at}Z"),
                    "look into the zebra bug",
                ) + &assistant_line(
                    &format!("2026-01-01T00:00:{at}Z"),
                    serde_json::json!([{"type":"text","text":"the zebra bug is a race"}]),
                )),
            );
        }
        sb.sync();

        let folded = search_with(&sb, "zebra", &|_| {});
        assert_eq!(folded.returned, 1, "one entry for the parent");
        let entry = &folded.results[0];
        assert_eq!(entry.session.session_id, CLAUDE_ID);
        assert_eq!((entry.hit_messages, entry.subagent_hits), (0, 4));
        assert!(entry.hits.iter().all(|h| h
            .subagent_session_id
            .as_deref()
            .is_some_and(|id| id.starts_with("agent-"))));
        assert!(entry.hits[0].show.contains("agent-"));

        // `--include-subagents`: each sub-agent session is an entry of its own.
        let expanded = search_with(&sb, "zebra", &|p| p.expand_subagents = Some(true));
        assert_eq!(expanded.returned, 2);
        assert!(expanded.results.iter().all(|r| r.session.is_subagent));
        // `--no-subagents` drops them altogether.
        assert_eq!(
            search_with(&sb, "zebra", &|p| p.include_subagents = Some(false)).returned,
            0
        );
    }

    #[test]
    fn a_limit_counts_parent_sessions_not_their_sub_agents() {
        let sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &user_line("2026-01-01T00:00:01Z", "the zebra bug"),
        );
        for name in ["agent-a", "agent-b", "agent-c"] {
            write(
                &subagent_file(&sb, name),
                &user_line("2026-01-01T00:00:02Z", "the zebra bug again"),
            );
        }
        write(
            &sb.codex_file(),
            &codex_text().replace("add a retry", "zebra too"),
        );
        sb.sync();
        let found = search_with(&sb, "zebra", &|p| p.limit = Some(2));
        assert_eq!(found.returned, 2, "claude parent and the codex session");
        assert!(found.results.iter().all(|r| !r.session.is_subagent));
        assert_eq!(found.matched_sessions, 2);
    }

    #[test]
    fn command_output_is_compact_but_keeps_what_the_skill_text_uses() {
        let sb = Sandbox::new();
        let long = "zebra ".to_string() + &"very long question ".repeat(200);
        write(
            &sb.claude_file(),
            &(user_line("2026-01-01T00:00:01Z", &long)
                + &assistant_line(
                    "2026-01-01T00:00:02Z",
                    serde_json::json!([{"type":"text","text":
                        format!("zebra {} the answer is a race", "filler ".repeat(30))}]),
                )),
        );
        sb.sync();
        let response = search_with(&sb, "zebra", &|_| {});

        let compact = crate::kb::search::agent_json(&response, false);
        let verbose = crate::kb::search::agent_json(&response, true);
        let hit = &compact["results"][0];
        // A long first message does not become a long title.
        let title = hit["session"]["title"].as_str().unwrap();
        assert!(
            title.chars().count() <= crate::kb::search::TITLE_CHARS + 1,
            "{title}"
        );
        // Noise is gone…
        for key in ["tokens", "paths", "highlights", "msg_key", "show", "cwd"] {
            assert!(
                !compact.to_string().contains(&format!("\"{key}\":")),
                "{key}"
            );
            assert!(
                verbose.to_string().contains(&format!("\"{key}\":")),
                "{key}"
            );
        }
        assert!(compact.to_string().len() < verbose.to_string().len());

        // Everything the v1 skill text reads is still there, under the same name.
        for path in [
            "/data_as_of",
            "/results/0/session/title",
            "/results/0/session/last_activity_at",
            "/results/0/session/source",
            "/results/0/session/session_id",
            "/results/0/hits/0/snippet",
            "/results/0/hits/0/seq",
            "/results/0/resume/command",
            "/results/0/resume/available",
        ] {
            assert!(!compact.pointer(path).unwrap().is_null(), "{path}");
        }

        // An answer's excerpt reaches past the matched words to the conclusion.
        let answer = response.results[0]
            .hits
            .iter()
            .find(|h| h.kind == "assistant")
            .unwrap();
        assert!(
            answer.snippet.contains("the answer is a race"),
            "{}",
            answer.snippet
        );
        let question = response.results[0]
            .hits
            .iter()
            .find(|h| h.kind == "user")
            .unwrap();
        assert!(question.snippet.chars().count() < 220);
    }

    /// Every dotted key path of a JSON value, arrays flattened:
    /// `results.hits.snippet`, `notes.show`, …
    fn key_paths(value: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, inner) in map {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    out.push(path.clone());
                    key_paths(inner, &path, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    key_paths(item, prefix, out);
                }
            }
            _ => {}
        }
    }

    /// The fields a skill text tells the agent to read: the backticked words
    /// that look like JSON paths (`hits[].snippet`, `resume.command`, `notes`).
    fn fields_in(skill: &str) -> std::collections::BTreeSet<String> {
        let is_path = regex::Regex::new(r"^[a-z_]+(\[\])?(\.[a-z_]+(\[\])?)*$").unwrap();
        skill
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|token| is_path.is_match(token))
            .map(|token| token.replace("[]", ""))
            .filter(|field| !["claude", "codex", "cd"].contains(&field.as_str()))
            .collect()
    }

    /// Skill texts already installed on people's machines (v1, v2) must keep
    /// working: a field is never renamed or dropped from the default output.
    /// The newest text is checked from the template itself, so a field it
    /// mentions cannot be missing either.
    #[test]
    fn every_field_the_skill_texts_read_is_in_the_default_output() {
        let sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &user_line("2026-01-01T00:00:01Z", "start the zebra bug"),
        );
        write(
            &subagent_file(&sb, "agent-a"),
            &(user_line("2026-01-01T00:00:02Z", "look into the zebra bug")
                + &assistant_line(
                    "2026-01-01T00:00:03Z",
                    serde_json::json!([{"type":"text","text":"the zebra bug is a race"}]),
                )),
        );
        sb.sync();
        let mut file = crate::kb::notes::testing::sample("claude", CLAUDE_ID, 1);
        file.entries[0].keywords = vec!["zebra".into()];
        crate::kb::notes::write(&sb.env.data, &file).unwrap();
        sb.sync();

        let response = search_notes_with(&sb, "zebra", None);
        let json = crate::kb::search::agent_json(&response, false);
        let mut paths = Vec::new();
        key_paths(&json, "", &mut paths);
        let present = |field: &str| {
            paths
                .iter()
                .any(|p| p == field || p.ends_with(&format!(".{field}")))
        };

        // Frozen: what the v1 and v2 texts read.
        for field in [
            "data_as_of",
            "results",
            "session.title",
            "session.last_activity_at",
            "session.source",
            "session.session_id",
            "hits.snippet",
            "hits.seq",
            "hits.subagent_session_id",
            "subagent_hits",
            "resume.command",
            "resume.available",
        ] {
            assert!(present(field), "an installed v1/v2 skill reads {field}");
        }
        // Whatever the current text mentions, as written in the template.
        let current = fields_in(&crate::kb::skill::render(
            crate::kb::skill::Target::Claude,
            "/x/agentkb",
        ));
        assert!(current.contains("notes.show") && current.contains("hits.snippet"));
        for field in &current {
            assert!(present(field), "the current skill reads {field}");
        }
    }

    fn scratch_file(sb: &Sandbox) -> PathBuf {
        sb.claude
            .join("projects/-Users-me-scratch-t1/22222222-2222-4222-8222-222222222222.jsonl")
    }

    fn scratch_text(extra: &str) -> String {
        serde_json::json!({"type":"user","timestamp":"2026-01-02T00:00:01Z",
            "cwd":"/Users/me/scratch/t1",
            "message":{"role":"user","content":format!("quokka test question {extra}")}})
        .to_string()
            + "\n"
    }

    #[test]
    fn excluded_working_directories_stay_backed_up_but_leave_search_and_the_list() {
        let mut sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &user_line("2026-01-01T00:00:01Z", "quokka real work"),
        );
        write(&scratch_file(&sb), &scratch_text(""));
        sb.sync();
        let visible = |sb: &Sandbox| {
            let filter = crate::kb::status::SessionFilter::default();
            crate::kb::status::list_sessions(&sb.env, &filter)
                .unwrap()
                .len()
        };
        assert_eq!((found(&sb, "quokka"), visible(&sb)), (2, 2));

        // A rule that only shares a name prefix does not match (whole components).
        sb.env.exclude_cwd_prefixes = vec!["/Users/me/scr".into()];
        sb.sync();
        assert_eq!(found(&sb, "quokka"), 2);

        sb.env.exclude_cwd_prefixes = vec!["/Users/me/scratch/".into()];
        sb.sync();
        assert_eq!(found(&sb, "quokka"), 1, "the test session left the index");
        assert_eq!(visible(&sb), 1);
        let all = crate::kb::status::SessionFilter {
            include_excluded: Some(true),
            ..Default::default()
        };
        assert_eq!(
            crate::kb::status::list_sessions(&sb.env, &all)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            crate::kb::status::facets(&sb.env).unwrap().projects.len(),
            1
        );
        // The raw copy is still there and identical to the source.
        let row = session_rows(&sb.db());
        assert_eq!(row.len(), 2);
        let mirror = sb.mirror_of(
            "claude/projects/-Users-me-scratch-t1/22222222-2222-4222-8222-222222222222.jsonl",
        );
        assert_eq!(
            fs::read(&mirror).unwrap(),
            fs::read(scratch_file(&sb)).unwrap()
        );

        // New lines in an excluded session are collected but not indexed.
        append(&scratch_file(&sb), &scratch_text("again"));
        sb.sync();
        assert_eq!(found(&sb, "quokka"), 1);
        assert_eq!(
            fs::read(&mirror).unwrap(),
            fs::read(scratch_file(&sb)).unwrap()
        );

        // Dropping the rule brings the messages back from the mirror.
        sb.env.exclude_cwd_prefixes = vec![];
        sb.sync();
        assert_eq!((found(&sb, "quokka"), visible(&sb)), (2, 2));
        assert_eq!(
            message_rows(&sb.db())
                .iter()
                .filter(|r| r.0 == "22222222-2222-4222-8222-222222222222")
                .count(),
            2
        );
    }

    #[test]
    fn unreadable_titles_db_degrades_to_a_warning_not_a_failed_sync() {
        let sb = Sandbox::new();
        write(&sb.codex_file(), &codex_text());
        fs::write(sb.codex.join("state_5.sqlite"), b"garbage").unwrap();
        let s = sb.sync();
        assert_eq!(s.errors.len(), 1);
        assert_eq!(s.errors[0].stage, "title_db");
        assert_eq!(s.new_sessions, 1, "collection itself went through");
        let title: String = sb
            .db()
            .query_row("SELECT title FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "add a retry");
    }

    #[test]
    fn exclusion_list_stops_collection_and_does_not_mark_old_rows_deleted() {
        let mut sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        sb.env.exclude_prefixes = vec![sb.claude.join("projects").to_string_lossy().into_owned()];
        let s = sb.sync();
        assert_eq!((s.scanned_files, s.deleted_sources), (0, 0));
        assert_eq!(session_rows(&sb.db())[0].3, "present");
    }

    #[test]
    fn unknown_record_types_are_counted_not_fatal() {
        let sb = Sandbox::new();
        write(
            &sb.claude_file(),
            &format!("{}{}\n", claude_text(), r#"{"type":"from-the-future"}"#),
        );
        let s = sb.sync();
        assert_eq!(s.status, "ok");
        assert_eq!(s.unknown_types.get("from-the-future"), Some(&1));
    }

    #[test]
    fn agent_label_titles_are_stable_across_syncs_and_native_titles_revert_when_lost() {
        let sb = Sandbox::new();
        let meta = format!(
            r#"{{"timestamp":"2026-05-29T11:15:04Z","type":"session_meta","payload":{{"id":"{CODEX_ID}","cwd":"/x","parent_thread_id":"P","agent_nickname":"Ohm","agent_path":"/root/a/review"}}}}"#
        );
        write(&sb.codex_file(), &format!("{meta}\n"));
        let title = |db: &Connection| -> (Option<String>, Option<String>) {
            db.query_row("SELECT title, title_source FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
        };
        sb.sync();
        let first = title(&sb.db());
        assert_eq!(
            first,
            (Some("Ohm · review".into()), Some("agent_meta".into()))
        );
        sb.sync();
        assert_eq!(title(&sb.db()), first, "no flip-flop between refreshes");

        // A native title appears, then the thread is deleted from Codex's db.
        let state = sb.codex.join("state_5.sqlite");
        let conn = Connection::open(&state).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL);
             INSERT INTO threads VALUES ('{CODEX_ID}', 'Native');"
        ))
        .unwrap();
        sb.sync();
        assert_eq!(
            title(&sb.db()),
            (Some("Native".into()), Some("threads".into()))
        );
        conn.execute("DELETE FROM threads", []).unwrap();
        drop(conn);
        sb.sync();
        assert_eq!(title(&sb.db()).0.as_deref(), Some("Ohm · review"));
    }

    #[test]
    fn outdated_indexer_version_rebuilds_rows_from_the_mirror_once() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        let conn = sb.db();
        // Pretend an older program indexed this session badly.
        conn.execute(
            "UPDATE sessions SET title='[Image #1] stale', title_source='first_message', message_count=99, indexer_state='{}'",
            [],
        )
        .unwrap();
        conn.execute("UPDATE meta SET value='1' WHERE key='indexer_version'", [])
            .unwrap();
        drop(conn);

        let s = sb.sync();
        assert_eq!(s.status, "ok", "{:?}", s.errors);
        let (title, count): (String, i64) = sb
            .db()
            .query_row("SELECT title, message_count FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((title.as_str(), count), ("hello there", 2));
        assert_eq!(
            db::get_meta(&sb.db(), "indexer_version")
                .unwrap()
                .as_deref(),
            Some(INDEXER_VERSION)
        );
        assert_eq!(s.copied_bytes, 0, "rebuilding the index copies nothing");
    }

    #[test]
    fn a_failed_rebuild_is_retried_on_the_next_sync() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        sb.sync();
        sb.db()
            .execute("UPDATE meta SET value='1' WHERE key='indexer_version'", [])
            .unwrap();
        let mirror = sb.mirror_of(&format!("claude/projects/-Users-me-proj/{CLAUDE_ID}.jsonl"));
        let saved = fs::read(&mirror).unwrap();
        fs::remove_file(&mirror).unwrap();
        let failed = sb.sync();
        assert!(failed.errors.iter().any(|e| e.stage == "reindex"));
        assert_eq!(
            db::get_meta(&sb.db(), "indexer_version")
                .unwrap()
                .as_deref(),
            Some("1")
        );

        fs::write(&mirror, saved).unwrap();
        let ok = sb.sync();
        assert!(
            ok.errors.iter().all(|e| e.stage != "reindex"),
            "{:?}",
            ok.errors
        );
        assert_eq!(
            db::get_meta(&sb.db(), "indexer_version")
                .unwrap()
                .as_deref(),
            Some(INDEXER_VERSION)
        );
    }

    #[test]
    fn empty_sessions_are_hidden_from_the_list_unless_asked_for() {
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        let empty = sb
            .claude
            .join("projects/-Users-me-proj/22222222-3333-4444-5555-666666666666.jsonl");
        write(&empty, "{\"type\":\"mode\",\"mode\":\"x\"}\n");
        sb.sync();

        let default = super::super::status::list_sessions(
            &sb.env,
            &super::super::status::SessionFilter::default(),
        )
        .unwrap();
        assert_eq!(default.len(), 1);
        let all = super::super::status::list_sessions(
            &sb.env,
            &super::super::status::SessionFilter {
                include_empty: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(all.len(), 2);
    }

    // ---- backup wired into the collection ----

    fn fake_disk(
        sb: &Sandbox,
    ) -> (
        PathBuf,
        crate::kb::volume::fake::FakeQuery,
        super::super::sync::MirrorTarget,
    ) {
        use crate::kb::volume::fake::{volume, FakeQuery};
        let disk = sb.dir.path().join("disk");
        fs::create_dir_all(&disk).unwrap();
        let query = FakeQuery::with([crate::kb::volume::VolumeInfo {
            mount_point: disk.clone(),
            ..volume("DISK", "/x", false, "exfat")
        }]);
        let target = MirrorTarget {
            target_dir: disk.join("AgentKB-backup"),
            volume_uuid: "DISK".into(),
        };
        (disk, query, target)
    }

    #[test]
    fn a_collection_is_followed_by_a_byte_identical_backup() {
        let mut sb = Sandbox::new();
        let (_disk, query, target) = fake_disk(&sb);
        sb.env.mirror = Some(target.clone());
        write(&sb.claude_file(), &claude_text());
        let summary = run_sync_with(&sb.env, "test", &query).unwrap();
        assert_eq!(summary.status, "ok", "{summary:?}");
        let backup = summary.backup.expect("backup ran");
        assert_eq!(backup.status, "ok");
        let rel = "claude/projects/-Users-me-proj/".to_string() + CLAUDE_ID + ".jsonl";
        assert_eq!(
            fs::read(sb.mirror_of(&rel)).unwrap(),
            fs::read(target.target_dir.join("raw").join(&rel)).unwrap()
        );
        // A later append to the source flows through collect then backup.
        append(&sb.claude_file(), "{\"type\":\"mode\",\"mode\":\"y\"}\n");
        let again = run_sync_with(&sb.env, "test", &query).unwrap();
        assert_eq!(again.backup.unwrap().appended_files, 1);
        assert_eq!(
            fs::read(sb.mirror_of(&rel)).unwrap(),
            fs::read(target.target_dir.join("raw").join(&rel)).unwrap()
        );
    }

    #[test]
    fn an_unplugged_disk_never_fails_or_dirties_the_collection() {
        use crate::kb::volume::fake::FakeQuery;
        let mut sb = Sandbox::new();
        let (disk, _query, target) = fake_disk(&sb);
        sb.env.mirror = Some(target.clone());
        write(&sb.claude_file(), &claude_text());
        // Unplugged: only the system disk answers, and the folder exists as a plain one.
        let system_only = FakeQuery::with([crate::kb::volume::VolumeInfo {
            mount_point: sb.dir.path().to_path_buf(),
            ..crate::kb::volume::fake::volume("SYS", "/", true, "apfs")
        }]);
        fs::create_dir_all(&target.target_dir).unwrap();
        let summary = run_sync_with(&sb.env, "test", &system_only).unwrap();
        assert_eq!(summary.status, "ok");
        assert_eq!(summary.backup.as_ref().unwrap().status, "skipped");
        assert!(summary.errors.is_empty());
        assert!(fs::read_dir(&target.target_dir).unwrap().next().is_none());
        assert!(disk.is_dir());
    }

    #[test]
    fn list_filters_narrow_by_client_project_text_and_cwd() {
        use super::super::status::{list_sessions, SessionFilter};
        let sb = Sandbox::new();
        write(&sb.claude_file(), &claude_text());
        write(&sb.codex_file(), &codex_text());
        sb.sync();
        let count = |f: SessionFilter| list_sessions(&sb.env, &f).unwrap().len();
        assert_eq!(count(SessionFilter::default()), 2);
        assert_eq!(
            count(SessionFilter {
                source: Some("codex".into()),
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            count(SessionFilter {
                query: Some("zzz-no-such-title".into()),
                ..Default::default()
            }),
            0
        );
        assert_eq!(
            count(SessionFilter {
                since: Some("2999-01-01T00:00:00Z".into()),
                ..Default::default()
            }),
            0
        );
        assert_eq!(
            count(SessionFilter {
                until: Some("2999-01-01T00:00:00Z".into()),
                ..Default::default()
            }),
            2
        );
        let first = list_sessions(&sb.env, &SessionFilter::default()).unwrap();
        let project = first[0].project_name.clone();
        let hits = count(SessionFilter {
            project: Some(project),
            ..Default::default()
        });
        assert!(hits >= 1);
        let cwd = first
            .iter()
            .find_map(|s| s.cwd.clone())
            .expect("a session has a cwd");
        assert!(
            count(SessionFilter {
                cwd: Some(cwd),
                ..Default::default()
            }) >= 1
        );
        let facets = super::super::status::facets(&sb.env).unwrap();
        assert!(!facets.projects.is_empty());
    }
}
