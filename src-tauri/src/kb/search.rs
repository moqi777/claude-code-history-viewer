//! Full-text search over the message index (`kb.db`), shared by the
//! `--kb-search` / `--kb-show` commands and the home page search box.
//!
//! Read-only on purpose: no sync lock, no writes, no collection. Opened the
//! normal way first; if that fails (a sandboxed agent cannot create the WAL
//! `-shm` file next to a database nobody has open) it is opened `immutable=1`,
//! which is exact when no process holds the database.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::{
    params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension,
};
use serde::{Deserialize, Serialize};

use super::index::messages::{Extractor, Kind, MARKER};
use super::index::tokenize::{self, QueryGroup};
use super::paths::DataPaths;
use super::redact::redact;
use super::sync::INDEXER_VERSION;
use super::Source;

pub const DEFAULT_LIMIT: usize = 5;
pub const MAX_LIMIT: usize = 20;
pub const DEFAULT_PER_SESSION: usize = 2;
pub const MAX_PER_SESSION: usize = 5;
/// Messages pulled from the index before grouping them into sessions.
const CANDIDATES: usize = 300;
/// Fewer sessions than this from the strict pass → relax to "any word".
const MIN_STRICT_SESSIONS: usize = 3;
/// Share of the query words a message must contain in relaxed mode.
const MIN_COVERAGE: f64 = 0.4;
/// Weight of tool traffic relative to conversation text.
const TOOL_WEIGHT: f64 = 0.4;
const SNIPPET_BEFORE: usize = 60;
const SNIPPET_AFTER: usize = 140;

#[derive(Debug)]
pub enum SearchError {
    /// No database yet (never collected or not configured).
    NoData(String),
    /// The index is being (re)built.
    NotReady(String),
    Other(String),
}

impl SearchError {
    pub fn message(&self) -> &str {
        match self {
            Self::NoData(m) | Self::NotReady(m) | Self::Other(m) => m,
        }
    }

    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::NoData(_) => 2,
            Self::NotReady(_) => 3,
            Self::Other(_) => 1,
        }
    }
}

impl From<rusqlite::Error> for SearchError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Other(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    Normal,
    Immutable,
}

pub struct Index {
    pub conn: Connection,
    pub read_mode: ReadMode,
    pub data: DataPaths,
}

fn open_with(path: &Path, immutable: bool) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = if immutable {
        Connection::open_with_flags(
            super::index::codex_titles::sqlite_uri(path, "immutable=1"),
            flags,
        )?
    } else {
        Connection::open_with_flags(path, flags)?
    };
    conn.busy_timeout(Duration::from_secs(5))?;
    // Opening is lazy: touch the file so a sandbox failure shows up here.
    conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(conn)
}

impl Index {
    pub fn open(data: &DataPaths) -> Result<Self, SearchError> {
        let path = data.db();
        if !path.is_file() {
            return Err(SearchError::NoData(format!(
                "no knowledge base at {} yet; run a collection first",
                path.display()
            )));
        }
        let (conn, read_mode) = match open_with(&path, false) {
            Ok(conn) => (conn, ReadMode::Normal),
            Err(_) => (
                open_with(&path, true)
                    .map_err(|e| SearchError::Other(format!("cannot open kb.db: {e}")))?,
                ReadMode::Immutable,
            ),
        };
        let index = Self {
            conn,
            read_mode,
            data: data.clone(),
        };
        index.check_ready()?;
        Ok(index)
    }

    fn check_ready(&self) -> Result<(), SearchError> {
        let has_messages: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE name='messages_fts'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let version: Option<String> = if has_messages {
            self.conn
                .query_row(
                    "SELECT value FROM meta WHERE key='indexer_version'",
                    [],
                    |r| r.get(0),
                )
                .optional()?
        } else {
            None
        };
        if version.as_deref() == Some(INDEXER_VERSION) {
            Ok(())
        } else {
            Err(SearchError::NotReady(
                "the search index is still being built; try again in a minute".into(),
            ))
        }
    }

    /// When the last successful collection finished.
    pub fn data_as_of(&self) -> Option<String> {
        self.conn
            .query_row(
                "SELECT finished_at FROM sync_runs WHERE ok=1 AND finished_at IS NOT NULL \
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok()
    }
}

// ---------------------------------------------------------------------------
// Parameters and results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchParams {
    pub query: String,
    pub source: Option<String>,
    pub client: Option<String>,
    pub project: Option<String>,
    /// RFC 3339 lower / upper bound on the session's last activity.
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<usize>,
    pub per_session: Option<usize>,
    /// Default true: much real investigation happens in sub-agents.
    pub include_subagents: Option<bool>,
    /// `auto` (default), `all` or `any`.
    pub mode: Option<String>,
    pub kinds: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub source: String,
    pub session_id: String,
    pub title: Option<String>,
    pub client: Option<String>,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub started_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub source_status: String,
    pub is_subagent: bool,
    pub parent_session_id: Option<String>,
    pub message_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Resume {
    pub available: bool,
    pub command: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Paths {
    pub mirror: String,
    pub original: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub seq: u64,
    pub kind: String,
    pub at: Option<String>,
    /// Viewer message id (Claude `uuid`); absent for Codex.
    pub msg_key: Option<String>,
    /// Excerpt without markers…
    pub snippet: String,
    /// …and the matched ranges in it, as character offsets `[start, end)`.
    pub highlights: Vec<[usize; 2]>,
    pub show: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionHit {
    pub rank: usize,
    pub score: f64,
    pub session: SessionInfo,
    pub resume: Resume,
    pub paths: Paths,
    pub hit_messages: usize,
    pub hits: Vec<Hit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub agentkb_marker: &'static str,
    pub query: String,
    pub tokens: Vec<String>,
    pub mode_used: &'static str,
    pub read_mode: ReadMode,
    pub data_as_of: Option<String>,
    pub matched_sessions: usize,
    pub returned: usize,
    pub results: Vec<SessionHit>,
    pub hint: Option<String>,
}

struct Candidate {
    file_id: i64,
    seq: u64,
    kind: Kind,
    ts: Option<String>,
    msg_key: Option<String>,
    text: String,
    score: f64,
}

struct SessionRow {
    info: SessionInfo,
    mirror_rel: String,
    source_path: String,
}

// ---------------------------------------------------------------------------
// Searching
// ---------------------------------------------------------------------------

fn quoted(token: &str) -> String {
    format!("\"{}\"", token.replace('"', ""))
}

fn group_expr(group: &QueryGroup) -> String {
    match group {
        QueryGroup::Term(t) => quoted(t),
        QueryGroup::Compound { whole, parts } => {
            let all = parts
                .iter()
                .map(|p| quoted(p))
                .collect::<Vec<_>>()
                .join(" AND ");
            format!("({} OR ({all}))", quoted(whole))
        }
    }
}

fn match_expr(groups: &[QueryGroup], all: bool) -> String {
    groups
        .iter()
        .map(group_expr)
        .collect::<Vec<_>>()
        .join(if all { " AND " } else { " OR " })
}

fn token_set(text: &str) -> HashSet<String> {
    tokenize::tokens(text).into_iter().collect()
}

fn coverage(groups: &[QueryGroup], present: &HashSet<String>) -> f64 {
    if groups.is_empty() {
        return 1.0;
    }
    let hit = groups
        .iter()
        .filter(|g| match g {
            QueryGroup::Term(t) => present.contains(t),
            QueryGroup::Compound { whole, parts } => {
                present.contains(whole) || parts.iter().all(|p| present.contains(p))
            }
        })
        .count();
    hit as f64 / groups.len() as f64
}

fn fetch_candidates(
    index: &Index,
    groups: &[QueryGroup],
    all: bool,
    params: &SearchParams,
) -> Result<Vec<Candidate>, SearchError> {
    let mut sql = String::from(
        "SELECT m.file_id, m.seq, m.kind, m.ts, m.msg_key, m.text, \
                bm25(messages_fts, 1.0, ?2) AS rank \
         FROM messages_fts \
         JOIN messages m ON m.id = messages_fts.rowid \
         JOIN sessions s ON s.file_id = m.file_id \
         JOIN files f ON f.id = m.file_id \
         WHERE messages_fts MATCH ?1",
    );
    let mut args: Vec<SqlValue> = vec![
        SqlValue::Text(match_expr(groups, all)),
        SqlValue::Real(TOOL_WEIGHT),
    ];
    let mut push = |clause: &str, value: SqlValue, sql: &mut String| {
        args.push(value);
        sql.push_str(&format!(
            " AND {}",
            clause.replace('?', &format!("?{}", args.len()))
        ));
    };
    if let Some(source) = params.source.as_deref().filter(|s| !s.is_empty()) {
        push("s.source = ?", SqlValue::Text(source.to_string()), &mut sql);
    }
    if let Some(client) = params.client.as_deref().filter(|s| !s.is_empty()) {
        push("s.client = ?", SqlValue::Text(client.to_string()), &mut sql);
    }
    if let Some(project) = params.project.as_deref().filter(|s| !s.is_empty()) {
        push(
            "instr(lower(COALESCE(s.project_name,'') || ' ' || COALESCE(s.cwd,'')), lower(?)) > 0",
            SqlValue::Text(project.to_string()),
            &mut sql,
        );
    }
    if let Some(since) = params.since.as_deref().filter(|s| !s.is_empty()) {
        push(
            "COALESCE(s.last_activity_at, f.last_synced_at) >= ?",
            SqlValue::Text(since.to_string()),
            &mut sql,
        );
    }
    if let Some(until) = params.until.as_deref().filter(|s| !s.is_empty()) {
        push(
            "COALESCE(s.last_activity_at, f.last_synced_at) <= ?",
            SqlValue::Text(until.to_string()),
            &mut sql,
        );
    }
    if params.include_subagents == Some(false) {
        sql.push_str(" AND s.is_subagent = 0");
    }
    if let Some(kinds) = &params.kinds {
        let valid: Vec<&str> = kinds
            .iter()
            .filter_map(|k| Kind::parse(k.trim()).map(Kind::as_str))
            .collect();
        if !valid.is_empty() {
            let list = valid
                .iter()
                .map(|k| format!("'{k}'"))
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND m.kind IN ({list})"));
        }
    }
    sql.push_str(&format!(" ORDER BY rank LIMIT {CANDIDATES}"));
    let mut stmt = index.conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
        let kind: String = r.get(2)?;
        let rank: f64 = r.get(6)?;
        Ok(Candidate {
            file_id: r.get(0)?,
            seq: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
            kind: Kind::parse(&kind).unwrap_or(Kind::User),
            ts: r.get(3)?,
            msg_key: r.get(4)?,
            text: r.get(5)?,
            // bm25() is lower-is-better and negative.
            score: -rank,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn session_row(index: &Index, file_id: i64) -> Result<Option<SessionRow>, SearchError> {
    Ok(index
        .conn
        .query_row(
            "SELECT s.source, s.session_id, s.title, s.client, s.project_name, s.cwd, \
                    s.started_at, s.last_activity_at, f.source_status, s.is_subagent, \
                    s.parent_session_id, s.message_count, f.mirror_rel, f.source_path \
             FROM sessions s JOIN files f ON f.id = s.file_id WHERE s.file_id = ?1",
            [file_id],
            |r| {
                Ok(SessionRow {
                    info: SessionInfo {
                        source: r.get(0)?,
                        session_id: r.get(1)?,
                        title: r.get(2)?,
                        client: r.get(3)?,
                        project: r.get(4)?,
                        cwd: r.get(5)?,
                        started_at: r.get(6)?,
                        last_activity_at: r.get(7)?,
                        source_status: r.get(8)?,
                        is_subagent: r.get::<_, i64>(9)? != 0,
                        parent_session_id: r.get(10)?,
                        message_count: u64::try_from(r.get::<_, i64>(11)?).unwrap_or(0),
                    },
                    mirror_rel: r.get(12)?,
                    source_path: r.get(13)?,
                })
            },
        )
        .optional()?)
}

fn recency_factor(last_activity: Option<&str>) -> f64 {
    let Some(ts) = last_activity.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()) else {
        return 1.0;
    };
    let age_days =
        (chrono::Utc::now() - ts.with_timezone(&chrono::Utc)).num_seconds() as f64 / 86_400.0;
    1.0 + 0.15 * (1.0 - age_days / 365.0).clamp(0.0, 1.0)
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The command that reopens the session in its own tool.
pub fn resume_for(info: &SessionInfo, exe: &str) -> Resume {
    if info.source_status == "deleted" {
        return Resume {
            available: false,
            command: None,
            note: Some(format!(
                "The original session file was deleted, so it cannot be resumed; \
                 read the backup with: {exe} --kb-show {}:{} --head 10",
                info.source, info.session_id
            )),
        };
    }
    let (id, note) = match (&info.parent_session_id, info.is_subagent) {
        (Some(parent), true) => (
            parent.clone(),
            Some(
                "This is a sub-agent session; the command resumes its parent session.".to_string(),
            ),
        ),
        _ => (info.session_id.clone(), None),
    };
    match info.source.as_str() {
        "claude" => match info.cwd.as_deref().filter(|c| !c.is_empty()) {
            Some(cwd) => Resume {
                available: true,
                command: Some(format!("cd {} && claude --resume {id}", shell_quote(cwd))),
                note,
            },
            None => Resume {
                available: false,
                command: None,
                note: Some(
                    "The working directory is unknown, so the session cannot be resumed from here."
                        .into(),
                ),
            },
        },
        _ => Resume {
            available: true,
            command: Some(format!("codex resume {id}")),
            note,
        },
    }
}

fn current_exe_string() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "AgentKB".to_string())
}

/// Character ranges of every query token inside `text`, merged.
fn match_ranges(text: &str, groups: &[QueryGroup]) -> Vec<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let mut byte_ranges: Vec<(usize, usize)> = Vec::new();
    for group in groups {
        for token in group.all_tokens() {
            let needle = token.to_string();
            let haystack = if token.is_ascii() { &lower } else { text };
            let mut from = 0;
            while let Some(pos) = haystack[from..].find(&needle) {
                let start = from + pos;
                byte_ranges.push((start, start + needle.len()));
                from = start + needle.len().max(1);
            }
        }
    }
    byte_ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in byte_ranges {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// An excerpt around the first match, with matched ranges as char offsets.
pub fn snippet(text: &str, groups: &[QueryGroup]) -> (String, Vec<[usize; 2]>) {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let ranges = match_ranges(&flat, groups);
    let chars: Vec<(usize, char)> = flat.char_indices().collect();
    let char_index_of = |byte: usize| chars.partition_point(|(b, _)| *b < byte);
    let first_char = ranges.first().map_or(0, |(s, _)| char_index_of(*s));
    let start = first_char.saturating_sub(SNIPPET_BEFORE);
    let end = (first_char + SNIPPET_AFTER).min(chars.len());
    let begin_byte = chars.get(start).map_or(flat.len(), |(b, _)| *b);
    let end_byte = chars.get(end).map_or(flat.len(), |(b, _)| *b);
    let mut out = String::new();
    let lead = if start > 0 { "…" } else { "" };
    out.push_str(lead);
    out.push_str(&flat[begin_byte..end_byte]);
    if end < chars.len() {
        out.push('…');
    }
    let shift = lead.chars().count();
    let highlights = ranges
        .iter()
        .filter(|(s, e)| *s >= begin_byte && *e <= end_byte)
        .map(|(s, e)| {
            let a = char_index_of(*s) - start + shift;
            let b = char_index_of(*e) - start + shift;
            [a, b]
        })
        .collect();
    (out, highlights)
}

/// Insert `<<`/`>>` around the highlighted ranges (for text output and agents).
pub fn mark(snippet: &str, highlights: &[[usize; 2]]) -> String {
    let chars: Vec<char> = snippet.chars().collect();
    let mut out = String::new();
    let mut cursor = 0;
    for [s, e] in highlights {
        if *s < cursor || *e > chars.len() {
            continue;
        }
        out.extend(&chars[cursor..*s]);
        out.push_str("<<");
        out.extend(&chars[*s..*e]);
        out.push_str(">>");
        cursor = *e;
    }
    out.extend(&chars[cursor..]);
    out
}

fn run_pass(
    index: &Index,
    groups: &[QueryGroup],
    all: bool,
    params: &SearchParams,
) -> Result<Vec<Candidate>, SearchError> {
    let mut candidates = fetch_candidates(index, groups, all, params)?;
    if !all {
        // Relaxed pass: keep messages that contain enough of the query, and
        // let coverage scale the score.
        candidates.retain_mut(|c| {
            let cov = coverage(groups, &token_set(&c.text));
            c.score *= 0.5 + cov;
            cov >= MIN_COVERAGE
        });
    }
    Ok(candidates)
}

pub fn search(index: &Index, params: &SearchParams) -> Result<SearchResponse, SearchError> {
    let groups = tokenize::query_groups(&params.query);
    if groups.is_empty() {
        return Err(SearchError::Other(
            "the query has no searchable words (only punctuation or filler words)".into(),
        ));
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let per_session = params
        .per_session
        .unwrap_or(DEFAULT_PER_SESSION)
        .clamp(1, MAX_PER_SESSION);

    let mode = params.mode.as_deref().unwrap_or("auto");
    let (candidates, mode_used) = match mode {
        "all" => (run_pass(index, &groups, true, params)?, "all"),
        "any" => (run_pass(index, &groups, false, params)?, "any"),
        _ => {
            let strict = run_pass(index, &groups, true, params)?;
            let sessions: HashSet<i64> = strict.iter().map(|c| c.file_id).collect();
            if sessions.len() >= MIN_STRICT_SESSIONS || groups.len() == 1 {
                (strict, "all")
            } else {
                (run_pass(index, &groups, false, params)?, "any")
            }
        }
    };

    // Group the messages by session.
    let mut by_session: BTreeMap<i64, Vec<Candidate>> = BTreeMap::new();
    for c in candidates {
        by_session.entry(c.file_id).or_default().push(c);
    }
    let matched_sessions = by_session.len();
    let exe = current_exe_string();
    let mut results: Vec<SessionHit> = Vec::new();
    for (file_id, mut messages) in by_session {
        let Some(row) = session_row(index, file_id)? else {
            continue;
        };
        messages.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut score = messages[0].score;
        if let Some(second) = messages.get(1) {
            score += 0.25 * second.score;
        }
        if let Some(third) = messages.get(2) {
            score += 0.1 * third.score;
        }
        score *= recency_factor(row.info.last_activity_at.as_deref());
        if row.info.is_subagent {
            score *= 0.8;
        }
        let hit_messages = messages.len();
        let hits = messages
            .into_iter()
            .take(per_session)
            .map(|m| {
                let (excerpt, highlights) = snippet(&redact(&m.text), &groups);
                Hit {
                    seq: m.seq,
                    kind: m.kind.as_str().to_string(),
                    at: m.ts,
                    msg_key: m.msg_key,
                    snippet: excerpt,
                    highlights,
                    show: format!(
                        "{exe} --kb-show {}:{} --around {}",
                        row.info.source, row.info.session_id, m.seq
                    ),
                }
            })
            .collect();
        let mut info = row.info;
        info.title = info.title.map(|t| redact(&t).into_owned());
        let mirror = index.data.mirror_path(&row.mirror_rel).map_or_else(
            |_| row.mirror_rel.clone(),
            |p| p.to_string_lossy().into_owned(),
        );
        results.push(SessionHit {
            rank: 0,
            score: (score * 100.0).round() / 100.0,
            resume: resume_for(&info, &exe),
            paths: Paths {
                mirror,
                original: row.source_path,
            },
            session: info,
            hit_messages,
            hits,
        });
    }
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
    results.truncate(limit);
    for (i, r) in results.iter_mut().enumerate() {
        r.rank = i + 1;
    }
    let hint = results.is_empty().then(|| {
        "No match. Try fewer or different words (a synonym, the English/Chinese variant, \
         a class or error name), or widen --since/--source/--project."
            .to_string()
    });
    let mut tokens: Vec<String> = Vec::new();
    for g in &groups {
        for t in g.all_tokens() {
            if !tokens.iter().any(|x| x == t) {
                tokens.push(t.to_string());
            }
        }
    }
    Ok(SearchResponse {
        agentkb_marker: MARKER,
        query: params.query.clone(),
        tokens,
        mode_used,
        read_mode: index.read_mode,
        data_as_of: index.data_as_of(),
        matched_sessions,
        returned: results.len(),
        results,
        hint,
    })
}

/// Run `search` against the data directory, retrying once when an immutable
/// read fails (a writer may have started in between).
pub fn search_in(data: &DataPaths, params: &SearchParams) -> Result<SearchResponse, SearchError> {
    let first =
        Index::open(data).and_then(|index| search(&index, params).map(|r| (index.read_mode, r)));
    match first {
        Ok((_, response)) => Ok(response),
        Err(SearchError::Other(_) | SearchError::NotReady(_)) => {
            std::thread::sleep(Duration::from_millis(150));
            let index = Index::open(data)?;
            search(&index, params)
        }
        Err(other) => Err(other),
    }
}

// ---------------------------------------------------------------------------
// Show
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShowParams {
    /// `<source>:<session id>` or a session id (prefix of at least 8 chars).
    pub session: String,
    pub around: Option<u64>,
    pub context: Option<u64>,
    pub head: Option<u64>,
    pub tail: Option<u64>,
    pub kinds: Option<Vec<String>>,
    pub max_chars: Option<usize>,
    pub full: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShownMessage {
    pub seq: u64,
    pub kind: String,
    pub at: Option<String>,
    pub text: String,
    pub truncated: bool,
    pub focus: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Range {
    pub from_seq: u64,
    pub to_seq: u64,
    pub total_messages: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowResponse {
    pub agentkb_marker: &'static str,
    pub read_mode: ReadMode,
    pub data_as_of: Option<String>,
    pub session: SessionInfo,
    pub resume: Resume,
    pub paths: Paths,
    pub range: Range,
    pub messages: Vec<ShownMessage>,
}

pub const DEFAULT_MAX_CHARS: usize = 2000;
const DEFAULT_CONTEXT: u64 = 4;
const DEFAULT_HEAD: u64 = 6;

fn resolve_session(index: &Index, selector: &str) -> Result<(i64, SessionRow), SearchError> {
    let (source, id) = match selector.split_once(':') {
        Some((s, rest)) if Source::parse(s).is_some() => (Some(s.to_string()), rest.to_string()),
        _ => (None, selector.to_string()),
    };
    if id.len() < 8 {
        return Err(SearchError::Other(
            "give the full session id or at least its first 8 characters".into(),
        ));
    }
    let like = format!("{}%", id.replace('%', "").replace('_', "\\_"));
    let mut stmt = index.conn.prepare(
        "SELECT s.file_id, s.source, s.session_id, s.title FROM sessions s \
         WHERE s.session_id LIKE ?1 ESCAPE '\\' AND (?2 IS NULL OR s.source = ?2) \
         ORDER BY s.last_activity_at DESC LIMIT 6",
    )?;
    let rows: Vec<(i64, String, String, Option<String>)> = stmt
        .query_map(rusqlite::params![like, source], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    match rows.as_slice() {
        [] => Err(SearchError::Other(format!("no session matches {selector}"))),
        [(file_id, ..)] => {
            let row = session_row(index, *file_id)?
                .ok_or_else(|| SearchError::Other("session vanished".into()))?;
            Ok((*file_id, row))
        }
        many => Err(SearchError::Other(format!(
            "{selector} is ambiguous; candidates: {}",
            many.iter()
                .map(|(_, source, id, title)| format!(
                    "{source}:{id} ({})",
                    title.as_deref().unwrap_or("untitled")
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ))),
    }
}

fn cut_chars(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    (text.chars().take(max).collect::<String>() + "…", true)
}

/// The untruncated text of a stored message, re-read from the mirror line.
fn full_text(
    index: &Index,
    row: &SessionRow,
    file_id: i64,
    seq: u64,
    kind: Kind,
    byte_off: u64,
) -> Option<String> {
    let path = index.data.mirror_path(&row.mirror_rel).ok()?;
    let source = Source::parse(&row.info.source)?;
    let line = read_line_at(&path, byte_off)?;
    let mut extractor = Extractor::uncapped();
    extractor.feed(source, &line);
    let rows: Vec<_> = extractor
        .take_ops()
        .into_iter()
        .filter_map(|op| match op {
            super::index::messages::MsgOp::Insert(r) if r.kind == kind => Some(r),
            _ => None,
        })
        .collect();
    // Several rows can share a line; pick the same position among them.
    let position: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE file_id=?1 AND byte_off=?2 AND kind=?3 AND seq<?4",
            rusqlite::params![
                file_id,
                i64::try_from(byte_off).unwrap_or(0),
                kind.as_str(),
                i64::try_from(seq).unwrap_or(0)
            ],
            |r| r.get(0),
        )
        .ok()?;
    rows.get(usize::try_from(position).ok()?)
        .or_else(|| rows.first())
        .map(|r| r.text.clone())
}

fn read_line_at(path: &Path, offset: u64) -> Option<Vec<u8>> {
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
    let file = std::fs::File::open(path).ok()?;
    let mut line = Vec::new();
    if path.extension().is_some_and(|e| e == "zst") {
        let decoder = zstd::stream::Decoder::new(file).ok()?;
        let mut reader = BufReader::new(decoder);
        std::io::copy(&mut reader.by_ref().take(offset), &mut std::io::sink()).ok()?;
        reader.read_until(b'\n', &mut line).ok()?;
    } else {
        let mut file = file;
        file.seek(SeekFrom::Start(offset)).ok()?;
        BufReader::new(file).read_until(b'\n', &mut line).ok()?;
    }
    (!line.is_empty()).then_some(line)
}

pub fn show(index: &Index, params: &ShowParams) -> Result<ShowResponse, SearchError> {
    let (file_id, row) = resolve_session(index, params.session.trim())?;
    let total: u64 = index
        .conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE file_id=?1",
            [file_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| u64::try_from(n).unwrap_or(0))?;
    let kinds: Vec<String> = match &params.kinds {
        Some(k) if !k.is_empty() => k
            .iter()
            .filter_map(|k| Kind::parse(k.trim()).map(|k| k.as_str().to_string()))
            .collect(),
        _ => vec!["user".into(), "assistant".into()],
    };
    let kind_list = kinds
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");
    let context = params.context.unwrap_or(DEFAULT_CONTEXT).min(30);
    // Select by position among the visible kinds, so context counts messages
    // the reader can see rather than tool traffic.
    let ordered: Vec<(u64, String, Option<String>, String, bool, u64)> = {
        let mut stmt = index.conn.prepare(&format!(
            "SELECT seq, kind, ts, text, truncated, byte_off FROM messages \
             WHERE file_id=?1 AND kind IN ({kind_list}) ORDER BY seq"
        ))?;
        let rows = stmt.query_map([file_id], |r| {
            Ok((
                u64::try_from(r.get::<_, i64>(0)?).unwrap_or(0),
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, i64>(4)? != 0,
                u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
            ))
        })?;
        let collected: Result<Vec<_>, _> = rows.collect();
        collected?
    };
    let (lo, hi) = if let Some(around) = params.around {
        let center = ordered.partition_point(|m| m.0 < around);
        let lo = center.saturating_sub(usize::try_from(context).unwrap_or(4));
        let hi = (center + usize::try_from(context).unwrap_or(4) + 1).min(ordered.len());
        (lo, hi)
    } else if let Some(tail) = params.tail {
        (
            ordered
                .len()
                .saturating_sub(usize::try_from(tail).unwrap_or(0)),
            ordered.len(),
        )
    } else {
        let head = usize::try_from(params.head.unwrap_or(DEFAULT_HEAD)).unwrap_or(6);
        (0, head.min(ordered.len()))
    };
    let max_chars = params.max_chars.unwrap_or(DEFAULT_MAX_CHARS).max(100);
    let full = params.full == Some(true);
    let mut messages = Vec::new();
    for (seq, kind, at, text, truncated, byte_off) in &ordered[lo..hi] {
        let focus = params.around == Some(*seq);
        let mut body = text.clone();
        let mut was_truncated = *truncated;
        if full && (focus || params.around.is_none()) && *truncated {
            if let Some(raw) =
                Kind::parse(kind).and_then(|k| full_text(index, &row, file_id, *seq, k, *byte_off))
            {
                body = raw;
                was_truncated = false;
            }
        }
        let (cut, was_cut) = cut_chars(&redact(&body), max_chars);
        messages.push(ShownMessage {
            seq: *seq,
            kind: kind.clone(),
            at: at.clone(),
            text: cut,
            truncated: was_truncated || was_cut,
            focus,
        });
    }
    let exe = current_exe_string();
    let mut info = row.info.clone();
    info.title = info.title.map(|t| redact(&t).into_owned());
    let mirror = index.data.mirror_path(&row.mirror_rel).map_or_else(
        |_| row.mirror_rel.clone(),
        |p| p.to_string_lossy().into_owned(),
    );
    Ok(ShowResponse {
        agentkb_marker: MARKER,
        read_mode: index.read_mode,
        data_as_of: index.data_as_of(),
        resume: resume_for(&info, &exe),
        paths: Paths {
            mirror,
            original: row.source_path,
        },
        range: Range {
            from_seq: messages.first().map_or(0, |m| m.seq),
            to_seq: messages.last().map_or(0, |m| m.seq),
            total_messages: total,
        },
        session: info,
        messages,
    })
}

pub fn show_in(data: &DataPaths, params: &ShowParams) -> Result<ShowResponse, SearchError> {
    let index = Index::open(data)?;
    show(&index, params)
}

// ---------------------------------------------------------------------------
// Human-readable output
// ---------------------------------------------------------------------------

fn local_time(iso: Option<&str>) -> String {
    iso.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn format_search_text(response: &SearchResponse) -> String {
    let mut out = format!(
        "Found {} session(s), showing {} (data as of {}; mode {}).\n",
        response.matched_sessions,
        response.returned,
        local_time(response.data_as_of.as_deref()),
        response.mode_used
    );
    for r in &response.results {
        let s = &r.session;
        out.push_str(&format!(
            "\n{}. [{}{}] {} — {} · {}\n",
            r.rank,
            s.source,
            s.client
                .as_deref()
                .map(|c| format!(" · {c}"))
                .unwrap_or_default(),
            s.title.as_deref().unwrap_or("(untitled)"),
            s.project.as_deref().unwrap_or("-"),
            local_time(s.last_activity_at.as_deref())
        ));
        match (&r.resume.command, &r.resume.note) {
            (Some(cmd), _) => out.push_str(&format!("   resume: {cmd}\n")),
            (None, Some(note)) => out.push_str(&format!("   resume: unavailable — {note}\n")),
            _ => {}
        }
        for h in &r.hits {
            out.push_str(&format!(
                "   #{} {}  {}\n",
                h.seq,
                h.kind,
                mark(&h.snippet, &h.highlights)
            ));
        }
        if r.hit_messages > r.hits.len() {
            out.push_str(&format!(
                "   ({} more matching messages)\n",
                r.hit_messages - r.hits.len()
            ));
        }
        if let Some(h) = r.hits.first() {
            out.push_str(&format!("   context: {}\n", h.show));
        }
    }
    if let Some(hint) = &response.hint {
        out.push_str(&format!("\n{hint}\n"));
    }
    out
}

pub fn format_show_text(response: &ShowResponse) -> String {
    let s = &response.session;
    let mut out = format!(
        "{} [{}] {} · {}\n",
        s.title.as_deref().unwrap_or("(untitled)"),
        s.source,
        s.project.as_deref().unwrap_or("-"),
        local_time(s.last_activity_at.as_deref())
    );
    if let Some(cmd) = &response.resume.command {
        out.push_str(&format!("resume: {cmd}\n"));
    }
    out.push_str(&format!(
        "messages {}-{} of {} (data as of {})\n",
        response.range.from_seq,
        response.range.to_seq,
        response.range.total_messages,
        local_time(response.data_as_of.as_deref())
    ));
    for m in &response.messages {
        out.push_str(&format!(
            "\n--- #{} {} {}{}{}\n{}\n",
            m.seq,
            m.kind,
            local_time(m.at.as_deref()),
            if m.focus { "  <== match" } else { "" },
            if m.truncated { "  (truncated)" } else { "" },
            m.text
        ));
    }
    out
}

/// Parse `7d`, `2w`, `12h` or `YYYY-MM-DD` into an RFC 3339 instant.
pub fn parse_when(text: &str) -> Result<String, String> {
    let text = text.trim();
    let fmt = "%Y-%m-%dT%H:%M:%SZ";
    if let Some(num) = text.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
        return Ok((chrono::Utc::now() - chrono::Duration::days(num))
            .format(fmt)
            .to_string());
    }
    if let Some(num) = text.strip_suffix('w').and_then(|n| n.parse::<i64>().ok()) {
        return Ok((chrono::Utc::now() - chrono::Duration::weeks(num))
            .format(fmt)
            .to_string());
    }
    if let Some(num) = text.strip_suffix('h').and_then(|n| n.parse::<i64>().ok()) {
        return Ok((chrono::Utc::now() - chrono::Duration::hours(num))
            .format(fmt)
            .to_string());
    }
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map(|d| format!("{d}T00:00:00Z"))
        .map_err(|_| format!("cannot read {text:?} as a time; use 7d, 2w, 12h or YYYY-MM-DD"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(q: &str) -> Vec<QueryGroup> {
        tokenize::query_groups(q)
    }

    #[test]
    fn snippets_center_on_the_match_and_report_char_ranges() {
        let text = format!("{}死锁{}", "前".repeat(100), "后".repeat(200));
        let (excerpt, ranges) = snippet(&text, &groups("死锁"));
        assert!(excerpt.starts_with('…') && excerpt.ends_with('…'));
        assert_eq!(ranges.len(), 1);
        let [s, e] = ranges[0];
        let chars: Vec<char> = excerpt.chars().collect();
        assert_eq!(chars[s..e].iter().collect::<String>(), "死锁");
        assert_eq!(mark(&excerpt, &ranges).matches("<<死锁>>").count(), 1);
    }

    #[test]
    fn latin_matches_ignore_case_and_overlapping_tokens_merge() {
        let (excerpt, ranges) = snippet(
            "java.lang.NoSuchMethodError at Foo",
            &groups("NoSuchMethodError"),
        );
        let marked = mark(&excerpt, &ranges);
        assert!(marked.contains("<<NoSuchMethodError>>"), "{marked}");
        let (excerpt, ranges) = snippet("连接池耗尽了", &groups("连接池"));
        assert_eq!(mark(&excerpt, &ranges), "<<连接池>>耗尽了");
    }

    #[test]
    fn resume_commands() {
        let mut info = SessionInfo {
            source: "claude".into(),
            session_id: "abc-123".into(),
            title: None,
            client: None,
            project: None,
            cwd: Some("/Users/me/my proj".into()),
            started_at: None,
            last_activity_at: None,
            source_status: "present".into(),
            is_subagent: false,
            parent_session_id: None,
            message_count: 1,
        };
        let r = resume_for(&info, "AgentKB");
        assert_eq!(
            r.command.as_deref(),
            Some("cd '/Users/me/my proj' && claude --resume abc-123")
        );
        info.source = "codex".into();
        assert_eq!(
            resume_for(&info, "AgentKB").command.as_deref(),
            Some("codex resume abc-123")
        );
        info.is_subagent = true;
        info.parent_session_id = Some("parent-1".into());
        let r = resume_for(&info, "AgentKB");
        assert_eq!(r.command.as_deref(), Some("codex resume parent-1"));
        assert!(r.note.is_some());
        info.source_status = "deleted".into();
        let r = resume_for(&info, "AgentKB");
        assert!(!r.available && r.command.is_none());
        assert!(r.note.unwrap().contains("--kb-show"));
    }

    #[test]
    fn times_are_parsed() {
        assert!(parse_when("7d").unwrap().ends_with('Z'));
        assert_eq!(parse_when("2026-09-01").unwrap(), "2026-09-01T00:00:00Z");
        assert!(parse_when("yesterday").is_err());
    }

    #[test]
    fn match_expressions() {
        assert_eq!(
            match_expr(&groups("死锁 redis"), true),
            "\"死锁\" AND \"redis\""
        );
        assert_eq!(
            match_expr(&groups("死锁 redis"), false),
            "\"死锁\" OR \"redis\""
        );
        assert_eq!(
            match_expr(&groups("NoSuchMethodError"), true),
            "(\"nosuchmethoderror\" OR (\"no\" AND \"such\" AND \"method\" AND \"error\"))"
        );
    }
}
