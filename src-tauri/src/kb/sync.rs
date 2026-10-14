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
pub const INDEXER_VERSION: &str = "2";
const INDEXER_VERSION_KEY: &str = "indexer_version";

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
    /// `Some` only when the disk backup is enabled and fully configured.
    pub mirror: Option<MirrorTarget>,
    pub progress: ProgressSink,
}

impl KbEnv {
    pub fn from_config(config: &KbConfig) -> Self {
        let mirror = config.mirror.enabled.then(|| {
            Some(MirrorTarget {
                target_dir: config.mirror.target_dir.clone()?,
                volume_uuid: config.mirror.volume_uuid.clone()?,
            })
        });
        Self {
            data: DataPaths::new(config.data_dir.clone()),
            roots: SourceRoots::resolve(config),
            exclude_prefixes: config.exclude_prefixes.clone(),
            mirror: mirror.flatten(),
            progress: ProgressSink::default(),
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

    reindex_if_outdated(env, conn, log, summary)?;
    reconcile_sources(env, conn, &discovery, summary)?;

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
    pub deleted: Vec<i64>,
}

/// Codex moves a rollout between `sessions/` and `archived_sessions/` keeping
/// its filename, so the filename (which carries the session id) is its identity.
fn move_key(source: Source, compressed: bool, path: &Path) -> Option<(Source, bool, String)> {
    if source != Source::Codex {
        return None;
    }
    let name = path.file_name()?.to_str()?.to_string();
    Some((source, compressed, name))
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
    let mut fresh: HashMap<(Source, bool, String), Vec<&SourceFile>> = HashMap::new();
    for file in &discovery.files {
        if known.contains(file.path_str.as_str()) {
            continue;
        }
        if let Some(key) = move_key(file.source, file.is_compressed, &file.path) {
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
        let candidate = move_key(row.source, row.is_compressed, path)
            .and_then(|key| fresh.get_mut(&key))
            .and_then(Vec::pop);
        match candidate {
            Some(new_file) => out.moves.push((row.id, new_file.path_str.clone())),
            None => out.deleted.push(row.id),
        }
    }
    out
}

fn reconcile_sources(
    env: &KbEnv,
    conn: &Connection,
    discovery: &Discovery,
    summary: &mut Summary,
) -> Result<(), String> {
    let rows = db::load_files(conn)?;
    let plan = reconcile(&rows, discovery, &env.exclude_prefixes);
    if plan.moves.is_empty() && plan.deleted.is_empty() {
        return Ok(());
    }
    let when = now();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin reconcile: {e}"))?;
    for (id, new_path) in &plan.moves {
        db::set_source_path(&tx, *id, new_path)?;
    }
    for id in &plan.deleted {
        db::mark_deleted(&tx, *id, &when)?;
    }
    tx.commit().map_err(|e| format!("commit reconcile: {e}"))?;
    summary.moved_sources += plan.moves.len() as u64;
    summary.deleted_sources += plan.deleted.len() as u64;
    Ok(())
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
    if db::get_meta(conn, INDEXER_VERSION_KEY)?.as_deref() == Some(INDEXER_VERSION) {
        return Ok(());
    }
    let rows = db::load_files(conn)?;
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
    // Stay outdated after a failure so the next sync retries.
    if !failed {
        db::set_meta(conn, INDEXER_VERSION_KEY, INDEXER_VERSION)?;
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
    db::upsert_session(
        &tx,
        row.id,
        row.source,
        &indexer.record(),
        &indexer.state_json(),
        &unknown,
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

    commit_file(conn, file, prior, &indexer, &mirrored.cursor, summary)?;

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
            return Ok(indexer);
        }
    }
    let mut indexer = Indexer::new(source, meta.clone());
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
    conn: &Connection,
    file: &SourceFile,
    prior: Option<&FileRow>,
    indexer: &Indexer,
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
    let is_new = db::upsert_session(
        &tx,
        file_id,
        file.source,
        &indexer.record(),
        &indexer.state_json(),
        &unknown,
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
                mirror: None,
                progress: ProgressSink::default(),
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
