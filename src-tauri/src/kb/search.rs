//! Full-text search over the message index (`kb.db`), shared by the
//! `--kb-search` / `--kb-show` commands and the home page search box.
//!
//! Read-only on purpose: no sync lock, no writes, no collection. Opened the
//! normal way first; if that fails (a sandboxed agent cannot create the WAL
//! `-shm` file next to a database nobody has open) it is opened `immutable=1`,
//! which is exact when no process holds the database.

use std::collections::HashMap;
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
pub const DEFAULT_NOTES: usize = 3;
pub const MAX_NOTES: usize = 10;
/// A session never takes more than this many of the note slots.
const NOTES_PER_SESSION: usize = 2;
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
/// An assistant's answer usually states its conclusion after the matched
/// words, so its excerpt reaches further.
const SNIPPET_AFTER_ANSWER: usize = 300;
/// Titles longer than this are cut in the command output (a pasted question can
/// run to kilobytes).
pub const TITLE_CHARS: usize = 60;
/// A sub-agent's hits count a little less than the main session's own.
const SUBAGENT_WEIGHT: f64 = 0.8;

/// A session whose title has the query's words is what the user is likely
/// after, however few of its messages repeat them: the score grows by up to
/// this fraction of itself, scaled by how many of the words the title has.
const TITLE_BOOST: f64 = 0.5;

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
    /// List sub-agent sessions as entries of their own instead of folding
    /// them into the session that started them.
    pub expand_subagents: Option<bool>,
    /// `auto` (default), `all` or `any`.
    pub mode: Option<String>,
    pub kinds: Option<Vec<String>>,
    /// How many AI-written notes to return ahead of the messages; `Some(0)`
    /// turns them off. Default [`DEFAULT_NOTES`].
    pub notes: Option<usize>,
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
    /// The device an imported session came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
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
    /// Set when the hit is in a sub-agent session folded into this entry:
    /// `--kb-show` that session, not the entry's, to read around `seq`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_session_id: Option<String>,
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
    /// The session has AI-written notes (see `notes` in the response).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_note: bool,
    pub hit_messages: usize,
    /// With semantic search on: `keyword`, `semantic` or `both`.
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_kind: Option<&'static str>,
    /// Cosine similarity of the best piece, for `semantic` and `both`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_score: Option<f64>,
    /// Matching messages from sub-agent sessions folded into this entry.
    #[serde(skip_serializing_if = "is_zero")]
    pub subagent_hits: usize,
    pub hits: Vec<Hit>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // the signature serde's skip_serializing_if needs
fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteEvidence {
    pub seq_from: u64,
    pub seq_to: u64,
    pub msg_key: Option<String>,
    /// When the first message was sent and its kind: how Codex messages
    /// (which have no key) are found again.
    pub at: Option<String>,
    pub kind: Option<String>,
}

/// One entry of a session's notes, as found by a search.
#[derive(Debug, Clone, Serialize)]
pub struct NoteHit {
    /// Always `"note"`: tells it apart from message hits.
    pub kind: &'static str,
    pub rank: usize,
    pub score: f64,
    /// `n1`, `n2`… inside the session's note file.
    pub entry_id: String,
    /// The digest round that wrote it.
    pub round: String,
    /// `claude/<model>`: which AI wrote it.
    pub engine: String,
    pub title: String,
    pub problem: String,
    pub cause: String,
    pub solution: String,
    pub conclusion: String,
    pub keywords: Vec<String>,
    pub evidence: NoteEvidence,
    /// When the AI wrote it: notes are summaries, not the conversation.
    pub digested_at: String,
    pub session: SessionInfo,
    pub paths: Paths,
    /// Read the messages the note is based on.
    pub show: String,
    pub resume: Resume,
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
    /// Notes the daily digest wrote, best first. Absent keys of older
    /// versions of this output are unaffected: this is only ever added.
    pub notes: Vec<NoteHit>,
    pub results: Vec<SessionHit>,
    pub hint: Option<String>,
    /// Whether the vectors were used, and if not, why.
    pub semantic: super::semantic::SemanticInfo,
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
    let origin = super::db::origin_column(&index.conn);
    Ok(index
        .conn
        .query_row(
            &format!(
                "SELECT s.source, s.session_id, s.title, s.client, s.project_name, s.cwd, \
                        s.started_at, s.last_activity_at, {}, s.is_subagent, \
                        s.parent_session_id, s.message_count, f.mirror_rel, f.source_path, \
                        {origin} \
                 FROM sessions s JOIN files f ON f.id = s.file_id WHERE s.file_id = ?1",
                super::db::status_expr(origin)
            ),
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
                        origin: r.get(14)?,
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

/// The file of the session `source`/`session_id`, when it is in the index.
fn file_of_session(
    index: &Index,
    source: &str,
    session_id: &str,
) -> Result<Option<i64>, SearchError> {
    Ok(index
        .conn
        .query_row(
            "SELECT file_id FROM sessions WHERE source=?1 AND session_id=?2 AND is_subagent=0 LIMIT 1",
            rusqlite::params![source, session_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// 1.0 for a title without any query word, up to `1 + TITLE_BOOST` for one with all.
fn title_factor(title: Option<&str>, groups: &[QueryGroup]) -> f64 {
    let Some(title) = title else {
        return 1.0;
    };
    1.0 + TITLE_BOOST * coverage(groups, &token_set(title))
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
    if info.source_status == "elsewhere" {
        return Resume {
            available: false,
            command: None,
            note: Some(format!(
                "This session came from another device{}, so it cannot be resumed here; \
                 read the backup with: {exe} --kb-show {}:{} --head 10",
                info.origin
                    .as_deref()
                    .map_or_else(String::new, |o| format!(" ({o})")),
                info.source,
                info.session_id
            )),
        };
    }
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
    snippet_with(text, groups, SNIPPET_AFTER)
}

fn snippet_with(text: &str, groups: &[QueryGroup], after: usize) -> (String, Vec<[usize; 2]>) {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let ranges = match_ranges(&flat, groups);
    let chars: Vec<(usize, char)> = flat.char_indices().collect();
    let char_index_of = |byte: usize| chars.partition_point(|(b, _)| *b < byte);
    let first_char = ranges.first().map_or(0, |(s, _)| char_index_of(*s));
    let start = first_char.saturating_sub(SNIPPET_BEFORE);
    let end = (first_char + after).min(chars.len());
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

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

struct NoteRow {
    id: i64,
    entry_id: String,
    round_id: String,
    engine: String,
    model: String,
    file_id: i64,
    source: String,
    session_id: String,
    title: String,
    problem: String,
    cause: String,
    solution: String,
    conclusion: String,
    keywords: String,
    seq_from: i64,
    seq_to: i64,
    msg_key: Option<String>,
    msg_at: Option<String>,
    msg_kind: Option<String>,
    digested_at: String,
    rank: f64,
}

fn note_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRow> {
    Ok(NoteRow {
        id: r.get(0)?,
        file_id: r.get(1)?,
        source: r.get(2)?,
        session_id: r.get(3)?,
        title: r.get(4)?,
        problem: r.get(5)?,
        cause: r.get(6)?,
        solution: r.get(7)?,
        conclusion: r.get(8)?,
        keywords: r.get(9)?,
        seq_from: r.get(10)?,
        seq_to: r.get(11)?,
        msg_key: r.get(12)?,
        digested_at: r.get(13)?,
        rank: r.get(14)?,
        entry_id: r.get(15)?,
        round_id: r.get(16)?,
        engine: r.get(17)?,
        model: r.get(18)?,
        msg_at: r.get(19)?,
        msg_kind: r.get(20)?,
    })
}

fn has_note_tables(index: &Index) -> Result<bool, SearchError> {
    Ok(index.conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('notes','notes_fts')",
        [],
        |r| r.get::<_, i64>(0),
    )? == 2)
}

fn fetch_notes(
    index: &Index,
    groups: &[QueryGroup],
    all: bool,
    params: &SearchParams,
) -> Result<Vec<NoteRow>, SearchError> {
    let mut sql = String::from(
        "SELECT n.id, s.file_id, n.source, n.session_id, n.title, n.problem, n.cause, n.solution, \
                n.conclusion, n.keywords, n.seq_from, n.seq_to, n.msg_key, n.digested_at, \
                bm25(notes_fts, 3.0, 4.0, 1.0) AS rank, n.entry_id, n.round_id, n.engine, n.model, \
                n.msg_at, n.msg_kind \
         FROM notes_fts \
         JOIN notes n ON n.id = notes_fts.rowid \
         JOIN sessions s ON s.source = n.source AND s.session_id = n.session_id AND s.is_subagent = 0 \
         JOIN files f ON f.id = s.file_id \
         WHERE notes_fts MATCH ?1 AND s.cwd_excluded = 0",
    );
    let mut args: Vec<SqlValue> = vec![SqlValue::Text(match_expr(groups, all))];
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
    sql.push_str(" ORDER BY rank LIMIT 60");
    let mut stmt = index.conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), note_row_from)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The best notes for the query, and whether they matched only loosely (not
/// every word): strict match first (every word), relaxed
/// only when strict finds nothing, like the message search. At most
/// [`NOTES_PER_SESSION`] per session so one chatty session cannot fill the list.
fn search_notes(
    index: &Index,
    groups: &[QueryGroup],
    params: &SearchParams,
    limit: usize,
    exe: &str,
) -> Result<(Vec<NoteHit>, bool), SearchError> {
    if limit == 0 || !has_note_tables(index)? {
        return Ok((Vec::new(), false));
    }
    let mode = params.mode.as_deref().unwrap_or("auto");
    let mut rows = match mode {
        "any" => Vec::new(),
        _ => fetch_notes(index, groups, true, params)?,
    };
    let mut relaxed = false;
    if rows.is_empty() && mode != "all" {
        relaxed = true;
        rows = fetch_notes(index, groups, false, params)?;
    }
    let mut scored: Vec<(f64, NoteRow)> = Vec::new();
    for row in rows {
        // bm25() is lower-is-better and negative.
        let mut score = -row.rank;
        if relaxed {
            let text = format!(
                "{} {} {} {} {} {}",
                row.title, row.keywords, row.problem, row.cause, row.solution, row.conclusion
            );
            let cov = coverage(groups, &token_set(&text));
            if cov < MIN_COVERAGE {
                continue;
            }
            score *= 0.5 + cov;
        }
        scored.push((score, row));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));

    let mut per_session: HashMap<(String, String), usize> = HashMap::new();
    let mut out = Vec::new();
    for (score, row) in scored {
        let taken = per_session
            .entry((row.source.clone(), row.session_id.clone()))
            .or_default();
        if *taken >= NOTES_PER_SESSION {
            continue;
        }
        let Some(note) = note_hit(index, row, score, exe)? else {
            continue;
        };
        *taken += 1;
        out.push(note);
        if out.len() >= limit {
            break;
        }
    }
    // Recency may reorder; the numbers shown follow the final order.
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    for (i, note) in out.iter_mut().enumerate() {
        note.rank = i + 1;
    }
    Ok((out, relaxed))
}

/// A found row as the output shows it; `None` when its session left the index.
fn note_hit(
    index: &Index,
    row: NoteRow,
    score: f64,
    exe: &str,
) -> Result<Option<NoteHit>, SearchError> {
    let Some(session) = session_row(index, row.file_id)? else {
        return Ok(None);
    };
    let mut info = session.info;
    info.title = info.title.map(|t| redact(&t).into_owned());
    let keywords: Vec<String> = serde_json::from_str(&row.keywords).unwrap_or_default();
    Ok(Some(NoteHit {
        kind: "note",
        rank: 0,
        score: (score * recency_factor(info.last_activity_at.as_deref()) * 100.0).round() / 100.0,
        entry_id: row.entry_id,
        round: row.round_id,
        engine: format!("{}/{}", row.engine, row.model),
        title: row.title,
        problem: row.problem,
        cause: row.cause,
        solution: row.solution,
        conclusion: row.conclusion,
        keywords,
        evidence: NoteEvidence {
            seq_from: u64::try_from(row.seq_from).unwrap_or(0),
            seq_to: u64::try_from(row.seq_to).unwrap_or(0),
            msg_key: row.msg_key,
            at: row.msg_at,
            kind: row.msg_kind,
        },
        digested_at: row.digested_at,
        show: format!(
            "{exe} --kb-show {}:{} --around {}",
            row.source, row.session_id, row.seq_from
        ),
        resume: resume_for(&info, exe),
        paths: Paths {
            mirror: index.data.mirror_path(&session.mirror_rel).map_or_else(
                |_| session.mirror_rel.clone(),
                |p| p.to_string_lossy().into_owned(),
            ),
            original: session.source_path.clone(),
        },
        session: info,
    }))
}

/// The notes page: newest first, optionally of one source / one session.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NotesFilter {
    pub source: Option<String>,
    pub session_id: Option<String>,
    /// RFC 3339 lower bound on the note's date.
    pub since: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotesPage {
    pub total: usize,
    pub notes: Vec<NoteHit>,
}

pub fn list_notes(data: &DataPaths, filter: &NotesFilter) -> Result<NotesPage, SearchError> {
    let path = data.db();
    if !path.is_file() {
        return Ok(NotesPage {
            total: 0,
            notes: Vec::new(),
        });
    }
    let conn = open_with(&path, false)
        .or_else(|_| open_with(&path, true))
        .map_err(|e| SearchError::Other(format!("cannot open kb.db: {e}")))?;
    let index = Index {
        conn,
        read_mode: ReadMode::Normal,
        data: data.clone(),
    };
    if !has_note_tables(&index)? {
        return Ok(NotesPage {
            total: 0,
            notes: Vec::new(),
        });
    }
    let mut clause = String::from("s.cwd_excluded = 0");
    let mut args: Vec<SqlValue> = Vec::new();
    for (value, sql) in [
        (&filter.source, "n.source = ?"),
        (&filter.session_id, "n.session_id = ?"),
        (&filter.since, "n.digested_at >= ?"),
    ] {
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            args.push(SqlValue::Text(v.to_string()));
            clause.push_str(&format!(
                " AND {}",
                sql.replace('?', &format!("?{}", args.len()))
            ));
        }
    }
    let from = format!(
        "FROM notes n JOIN sessions s ON s.source = n.source AND s.session_id = n.session_id \
         AND s.is_subagent = 0 WHERE {clause}"
    );
    let total: i64 = index.conn.query_row(
        &format!("SELECT COUNT(*) {from}"),
        params_from_iter(args.iter()),
        |r| r.get(0),
    )?;
    let limit = filter.limit.unwrap_or(50).clamp(1, 200);
    let offset = filter.offset.unwrap_or(0);
    let mut stmt = index.conn.prepare(&format!(
        "SELECT n.id, s.file_id, n.source, n.session_id, n.title, n.problem, n.cause, n.solution, \
                n.conclusion, n.keywords, n.seq_from, n.seq_to, n.msg_key, n.digested_at, \
                0.0, n.entry_id, n.round_id, n.engine, n.model, n.msg_at, n.msg_kind \
         {from} ORDER BY n.digested_at DESC, n.id LIMIT {limit} OFFSET {offset}"
    ))?;
    let rows = stmt.query_map(params_from_iter(args.iter()), note_row_from)?;
    let exe = current_exe_string();
    let mut notes = Vec::new();
    for row in rows {
        if let Some(mut note) = note_hit(&index, row?, 0.0, &exe)? {
            note.rank = offset + notes.len() + 1;
            notes.push(note);
        }
    }
    Ok(NotesPage {
        total: usize::try_from(total).unwrap_or(0),
        notes,
    })
}

/// Notes matching `params.query` (the notes page's search box).
pub fn find_notes_in(
    data: &DataPaths,
    params: &SearchParams,
    limit: usize,
) -> Result<Vec<NoteHit>, SearchError> {
    let groups = tokenize::query_groups(&params.query);
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let index = Index::open(data)?;
    search_notes(
        &index,
        &groups,
        params,
        limit.clamp(1, 50),
        &current_exe_string(),
    )
    .map(|(notes, _)| notes)
}

fn session_has_note(index: &Index, source: &str, session_id: &str) -> bool {
    has_note_tables(index).unwrap_or(false)
        && index
            .conn
            .query_row(
                "SELECT 1 FROM notes WHERE source=?1 AND session_id=?2 LIMIT 1",
                rusqlite::params![source, session_id],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
}

/// The sessions of the notes that matched come first in `results`, in the
/// notes' order, whether or not any of their messages matched: a note is a
/// summary somebody already wrote about exactly this, so the session it
/// belongs to is the best candidate. A session in both lists appears once,
/// with its message hits. Returns how many sessions had to be added.
fn promote_note_sessions(
    index: &Index,
    notes: &[NoteHit],
    results: &mut Vec<SessionHit>,
    exe: &str,
) -> Result<usize, SearchError> {
    let mut front: Vec<SessionHit> = Vec::new();
    let mut added = 0;
    for note in notes {
        let same = |r: &SessionHit| {
            r.session.source == note.session.source
                && r.session.session_id == note.session.session_id
        };
        if front.iter().any(same) {
            continue;
        }
        if let Some(at) = results.iter().position(same) {
            front.push(results.remove(at));
            continue;
        }
        let Some(file_id) = file_of_session(index, &note.session.source, &note.session.session_id)?
        else {
            continue;
        };
        let Some(row) = session_row(index, file_id)? else {
            continue;
        };
        let mut info = row.info;
        info.title = info.title.map(|t| redact(&t).into_owned());
        front.push(SessionHit {
            rank: 0,
            score: note.score,
            resume: resume_for(&info, exe),
            paths: Paths {
                mirror: note.paths.mirror.clone(),
                original: row.source_path,
            },
            has_note: true,
            session: info,
            hit_messages: 0,
            match_kind: None,
            semantic_score: None,
            subagent_hits: 0,
            hits: Vec::new(),
        });
        added += 1;
    }
    front.append(results);
    *results = front;
    Ok(added)
}

/// Any session of this source with this id, sub-agent or not.
fn file_of_any_session(
    index: &Index,
    source: &str,
    session_id: &str,
) -> Result<Option<i64>, SearchError> {
    Ok(index
        .conn
        .query_row(
            "SELECT file_id FROM sessions WHERE source=?1 AND session_id=?2 LIMIT 1",
            rusqlite::params![source, session_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// The same session filters the keyword query applies in SQL, for a session
/// only the vectors found.
fn passes_filters(params: &SearchParams, info: &SessionInfo) -> bool {
    let eq =
        |want: &Option<String>, have: Option<&str>| match want.as_deref().filter(|w| !w.is_empty())
        {
            None => true,
            Some(w) => have == Some(w),
        };
    if !eq(&params.source, Some(info.source.as_str()))
        || !eq(&params.client, info.client.as_deref())
    {
        return false;
    }
    if let Some(project) = params.project.as_deref().filter(|p| !p.is_empty()) {
        let hay = format!(
            "{} {}",
            info.project.as_deref().unwrap_or_default(),
            info.cwd.as_deref().unwrap_or_default()
        )
        .to_lowercase();
        if !hay.contains(&project.to_lowercase()) {
            return false;
        }
    }
    let when = info.last_activity_at.as_deref();
    if let (Some(since), Some(when)) = (params.since.as_deref().filter(|s| !s.is_empty()), when) {
        if when < since {
            return false;
        }
    }
    if let (Some(until), Some(when)) = (params.until.as_deref().filter(|s| !s.is_empty()), when) {
        if when > until {
            return false;
        }
    }
    !(params.include_subagents == Some(false) && info.is_subagent)
}

/// The message a vector piece came from (`m:<seq>:<n>`), as a hit.
fn message_hit(
    index: &Index,
    file_id: i64,
    info: &SessionInfo,
    key: &str,
    exe: &str,
) -> Option<Hit> {
    let seq: i64 = key.strip_prefix("m:")?.split(':').next()?.parse().ok()?;
    let (kind, ts, msg_key, text): (String, Option<String>, Option<String>, String) = index
        .conn
        .query_row(
            "SELECT kind, ts, msg_key, text FROM messages WHERE file_id=?1 AND seq=?2",
            rusqlite::params![file_id, seq],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .ok()??;
    let (excerpt, _) = cut_chars(&redact(&text), 300);
    Some(Hit {
        seq: u64::try_from(seq).unwrap_or(0),
        kind,
        at: ts,
        msg_key,
        snippet: excerpt,
        subagent_session_id: None,
        highlights: Vec::new(),
        show: format!(
            "{exe} --kb-show {}:{} --around {seq}",
            info.source, info.session_id
        ),
    })
}

/// Merge the sessions the vectors found into `results` (keyword result, best
/// first) with reciprocal rank fusion. Returns how many sessions only the
/// vectors found were added.
fn merge_semantic(
    index: &Index,
    params: &SearchParams,
    results: &mut Vec<SessionHit>,
    semantic: &super::semantic::rank::SemanticInput,
    keyword_all: bool,
    exe: &str,
) -> Result<usize, SearchError> {
    use super::semantic::rank;
    struct VecHit {
        file_id: i64,
        row: SessionRow,
        score: f32,
        best: (String, String),
    }
    let expand = params.expand_subagents == Some(true);
    let mut vec_hits: Vec<VecHit> = Vec::new();
    for vs in &semantic.sessions {
        let Some(mut file_id) = file_of_any_session(index, &vs.source, &vs.session_id)? else {
            continue;
        };
        let Some(mut row) = session_row(index, file_id)? else {
            continue;
        };
        if !passes_filters(params, &row.info) {
            continue;
        }
        // like the keyword result, a sub-agent's hit belongs to the session that started it
        if !expand && row.info.is_subagent {
            if let Some(parent) = row.info.parent_session_id.clone() {
                if let Some(pf) = file_of_session(index, &row.info.source, &parent)? {
                    if let Some(prow) = session_row(index, pf)? {
                        file_id = pf;
                        row = prow;
                    }
                }
            }
        }
        if vec_hits.iter().any(|h| {
            h.row.info.source == row.info.source && h.row.info.session_id == row.info.session_id
        }) {
            continue;
        }
        vec_hits.push(VecHit {
            file_id,
            row,
            score: vs.score,
            best: vs.best.clone(),
        });
    }
    let key_of = |r: &SessionHit| (r.session.source.clone(), r.session.session_id.clone());
    let kw_keys: Vec<(String, String)> = results.iter().map(key_of).collect();
    let vec_keys: Vec<(String, String)> = vec_hits
        .iter()
        .map(|h| (h.row.info.source.clone(), h.row.info.session_id.clone()))
        .collect();
    let fused = rank::fuse(&kw_keys, keyword_all, &vec_keys);

    let mut old: Vec<Option<SessionHit>> = std::mem::take(results).into_iter().map(Some).collect();
    let mut out: Vec<SessionHit> = Vec::with_capacity(old.len() + vec_hits.len());
    let mut added = 0;
    for f in &fused {
        let key = (f.source.clone(), f.session_id.clone());
        let vec_hit = vec_hits
            .iter()
            .position(|h| (h.row.info.source.clone(), h.row.info.session_id.clone()) == key);
        let existing = kw_keys
            .iter()
            .position(|k| *k == key)
            .and_then(|i| old[i].take());
        let score = (f.score * 1000.0).round() / 10.0;
        match (existing, vec_hit) {
            (Some(mut hit), v) => {
                hit.score = score;
                hit.match_kind = Some(if f.in_vector { "both" } else { "keyword" });
                hit.semantic_score =
                    v.map(|i| (f64::from(vec_hits[i].score) * 1000.0).round() / 1000.0);
                out.push(hit);
            }
            (None, Some(i)) => {
                let h = &vec_hits[i];
                let mut info = h.row.info.clone();
                info.title = info.title.map(|t| redact(&t).into_owned());
                let mirror = index.data.mirror_path(&h.row.mirror_rel).map_or_else(
                    |_| h.row.mirror_rel.clone(),
                    |p| p.to_string_lossy().into_owned(),
                );
                let hits: Vec<Hit> = message_hit(index, h.file_id, &info, &h.best.1, exe)
                    .into_iter()
                    .collect();
                out.push(SessionHit {
                    rank: 0,
                    score,
                    resume: resume_for(&info, exe),
                    paths: Paths {
                        mirror,
                        original: h.row.source_path.clone(),
                    },
                    has_note: session_has_note(index, &info.source, &info.session_id),
                    session: info,
                    hit_messages: 0,
                    match_kind: Some("semantic"),
                    semantic_score: Some((f64::from(h.score) * 1000.0).round() / 1000.0),
                    subagent_hits: 0,
                    hits,
                });
                added += 1;
            }
            (None, None) => {}
        }
    }
    // keyword results beyond the merge depth keep their order after the merged ones
    out.extend(old.into_iter().flatten().map(|mut r| {
        r.match_kind = Some("keyword");
        r
    }));
    *results = out;
    Ok(added)
}

pub fn search(index: &Index, params: &SearchParams) -> Result<SearchResponse, SearchError> {
    let input = semantic_input(index, params);
    search_with(index, params, input)
}

/// What the vector side contributes to this query. Never fails: anything
/// missing is a reason in the response and the keyword result stands alone.
fn semantic_input(index: &Index, params: &SearchParams) -> super::semantic::rank::SemanticInput {
    use super::semantic::rank;
    // A test that never sandboxed HOME must not read the real settings.
    #[cfg(test)]
    if std::env::var_os("CCHV_TEST_HOME").is_none() {
        return rank::SemanticInput::off();
    }
    let Ok(config) = super::config::KbConfig::load() else {
        return rank::SemanticInput::off();
    };
    rank::prepare(
        &index.data,
        config.semantic.enabled,
        params.kinds.as_ref().is_some_and(|k| !k.is_empty()),
        &params.query,
    )
}

pub fn search_with(
    index: &Index,
    params: &SearchParams,
    semantic: super::semantic::rank::SemanticInput,
) -> Result<SearchResponse, SearchError> {
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

    // Fold sub-agent sessions into the session that started them (unless asked
    // to list them separately), then group the messages by entry.
    let expand = params.expand_subagents == Some(true);
    let mut rows: BTreeMap<i64, SessionRow> = BTreeMap::new();
    let mut group_of: BTreeMap<i64, i64> = BTreeMap::new();
    for c in &candidates {
        if group_of.contains_key(&c.file_id) {
            continue;
        }
        let Some(row) = session_row(index, c.file_id)? else {
            continue;
        };
        let mut group = c.file_id;
        if !expand && row.info.is_subagent {
            if let Some(parent) = &row.info.parent_session_id {
                if let Some(parent_file) = file_of_session(index, &row.info.source, parent)? {
                    group = parent_file;
                }
            }
        }
        group_of.insert(c.file_id, group);
        rows.insert(c.file_id, row);
    }
    let mut by_group: BTreeMap<i64, Vec<Candidate>> = BTreeMap::new();
    for mut c in candidates {
        let Some(&group) = group_of.get(&c.file_id) else {
            continue;
        };
        if rows.get(&c.file_id).is_some_and(|r| r.info.is_subagent) {
            c.score *= SUBAGENT_WEIGHT;
        }
        by_group.entry(group).or_default().push(c);
    }
    let matched_sessions = by_group.len();
    let exe = current_exe_string();
    let mut results: Vec<SessionHit> = Vec::new();
    for (group, mut messages) in by_group {
        if let std::collections::btree_map::Entry::Vacant(slot) = rows.entry(group) {
            let Some(row) = session_row(index, group)? else {
                continue;
            };
            slot.insert(row);
        }
        let row = &rows[&group];
        messages.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut score = messages[0].score;
        if let Some(second) = messages.get(1) {
            score += 0.25 * second.score;
        }
        if let Some(third) = messages.get(2) {
            score += 0.1 * third.score;
        }
        score *= recency_factor(row.info.last_activity_at.as_deref());
        score *= title_factor(row.info.title.as_deref(), &groups);
        let hit_messages = messages.iter().filter(|m| m.file_id == group).count();
        let subagent_hits = messages.len() - hit_messages;
        let hits = messages
            .into_iter()
            .take(per_session)
            .map(|m| {
                let after = if m.kind == Kind::Assistant {
                    SNIPPET_AFTER_ANSWER
                } else {
                    SNIPPET_AFTER
                };
                let (excerpt, highlights) = snippet_with(&redact(&m.text), &groups, after);
                let from = rows.get(&m.file_id).unwrap_or(row);
                Hit {
                    seq: m.seq,
                    kind: m.kind.as_str().to_string(),
                    at: m.ts,
                    msg_key: m.msg_key,
                    snippet: excerpt,
                    subagent_session_id: (m.file_id != group).then(|| from.info.session_id.clone()),
                    highlights,
                    show: format!(
                        "{exe} --kb-show {}:{} --around {}",
                        from.info.source, from.info.session_id, m.seq
                    ),
                }
            })
            .collect();
        let mut info = row.info.clone();
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
                original: row.source_path.clone(),
            },
            has_note: session_has_note(index, &info.source, &info.session_id),
            session: info,
            hit_messages,
            match_kind: None,
            semantic_score: None,
            subagent_hits,
            hits,
        });
    }
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
    let keyword_mode = mode_used;
    let mut mode_used = mode_used;
    let mut matched_sessions = matched_sessions;
    if !semantic.sessions.is_empty() {
        let added = merge_semantic(
            index,
            params,
            &mut results,
            &semantic,
            keyword_mode == "all",
            &exe,
        )?;
        matched_sessions += added;
        mode_used = "hybrid";
    }
    let notes_limit = params.notes.unwrap_or(DEFAULT_NOTES).min(MAX_NOTES);
    let (notes, notes_are_loose) = search_notes(index, &groups, params, notes_limit, &exe)?;
    // Only a note that has every word of the query says "this session is about
    // exactly this"; a loose one must not push the message hits down.
    let added = if notes_are_loose {
        0
    } else {
        promote_note_sessions(index, &notes, &mut results, &exe)?
    };
    let matched_sessions = matched_sessions + added;
    results.truncate(limit);
    for (i, r) in results.iter_mut().enumerate() {
        r.rank = i + 1;
    }
    let hint = (results.is_empty() && notes.is_empty()).then(|| {
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
        notes,
        results,
        hint,
        semantic: semantic.info,
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
    let attempt = || Index::open(data).and_then(|index| show(&index, params));
    match attempt() {
        // Same single retry as `search_in`: a writer may have started in between.
        Err(SearchError::Other(_) | SearchError::NotReady(_)) => {
            std::thread::sleep(Duration::from_millis(150));
            attempt()
        }
        other => other,
    }
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

/// `title` cut to [`TITLE_CHARS`] characters.
pub fn short_title(title: &str) -> String {
    let flat: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= TITLE_CHARS {
        return flat;
    }
    flat.chars().take(TITLE_CHARS).collect::<String>() + "…"
}

/// Fields an agent has no use for: offsets for drawing highlights, the
/// message id of the viewer, the file locations (the skill forbids reading
/// them), a ready-made `--kb-show` line that repeats the program path, and
/// session facts the resume command already carries.
const HUMAN_ONLY_KEYS: [&str; 13] = [
    "tokens",
    "paths",
    "highlights",
    "msg_key",
    "show",
    "cwd",
    "started_at",
    "message_count",
    "is_subagent",
    "parent_session_id",
    "entry_id",
    "round",
    "engine",
];

fn strip_for_agent(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|key, _| !HUMAN_ONLY_KEYS.contains(&key.as_str()));
            if let Some(serde_json::Value::String(title)) =
                map.get_mut("session").and_then(|s| s.get_mut("title"))
            {
                *title = short_title(title);
            }
            map.values_mut().for_each(strip_for_agent);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_for_agent),
        _ => {}
    }
}

/// The JSON the command line prints. Compact by default; `verbose` is the full
/// structure the home page search also gets.
///
/// Fields the installed skill texts mention are never removed or renamed here
/// (a test pins them): an older skill must keep working.
pub fn agent_json<T: Serialize>(value: &T, verbose: bool) -> serde_json::Value {
    let mut json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    if !verbose {
        // A note's `show` is how an agent reads the evidence, so it stays
        // (unlike the `show` of every message hit, which it can rebuild).
        let notes = json.as_object_mut().and_then(|o| o.remove("notes"));
        strip_for_agent(&mut json);
        if let (Some(serde_json::Value::Array(mut notes)), Some(object)) =
            (notes, json.as_object_mut())
        {
            for note in &mut notes {
                let show = note.get("show").cloned();
                strip_for_agent(note);
                if let (Some(show), Some(map)) = (show, note.as_object_mut()) {
                    map.insert("show".into(), show);
                }
                if let Some(map) = note.as_object_mut() {
                    for key in NOTE_VERBOSE_ONLY {
                        map.remove(key);
                    }
                }
            }
            object.insert("notes".into(), serde_json::Value::Array(notes));
        }
    }
    json
}

/// Note fields only `--verbose` prints. Empty while the default output stays
/// under the size budget (10 KB on average over `P3-eval.md`, measured on the
/// sample library in P4 step 4); if it does not, `problem`, `cause`,
/// `solution`, `keywords` and `evidence` go here.
const NOTE_VERBOSE_ONLY: [&str; 0] = [];

pub fn format_search_text(response: &SearchResponse) -> String {
    let mut out = format!(
        "Found {} session(s), showing {} (data as of {}; mode {}).\n",
        response.matched_sessions,
        response.returned,
        local_time(response.data_as_of.as_deref()),
        response.mode_used
    );
    if !response.notes.is_empty() {
        out.push_str(
            "\nNotes (written by AgentKB's daily AI digest; check the original with `show`):\n",
        );
        for note in &response.notes {
            out.push_str(&format!(
                "\n[note {}] {} — {} · {}\n",
                note.rank,
                note.title,
                note.session.source,
                local_time(Some(&note.digested_at))
            ));
            for (label, text) in [
                ("problem", &note.problem),
                ("cause", &note.cause),
                ("solution", &note.solution),
                ("conclusion", &note.conclusion),
            ] {
                if !text.is_empty() {
                    out.push_str(&format!("   {label}: {text}\n"));
                }
            }
            out.push_str(&format!("   show: {}\n", note.show));
        }
    }
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
            s.title
                .as_deref()
                .map_or_else(|| "(untitled)".to_string(), short_title),
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
        if r.subagent_hits > 0 {
            out.push_str(&format!(
                "   (+{} matching messages in sub-agent sessions)\n",
                r.subagent_hits
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
        s.title
            .as_deref()
            .map_or_else(|| "(untitled)".to_string(), short_title),
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
    fn a_title_with_the_query_words_raises_the_score_by_how_many_it_has() {
        let g = groups("电池 耗电");
        assert!((title_factor(None, &g) - 1.0).abs() < 1e-9);
        assert!((title_factor(Some("今天吃什么"), &g) - 1.0).abs() < 1e-9);
        let one = title_factor(Some("找出耗电大户"), &g);
        let both = title_factor(Some("笔记本电池耗电过快"), &g);
        assert!(one > 1.0 && both > one, "{one} {both}");
        assert!((both - (1.0 + TITLE_BOOST)).abs() < 1e-9);
    }

    #[test]
    fn long_titles_are_cut_on_a_character_boundary_and_flattened() {
        let long = format!("第一行\n{}", "问".repeat(3000));
        let short = short_title(&long);
        assert_eq!(short.chars().count(), TITLE_CHARS + 1);
        assert!(short.starts_with("第一行 问") && short.ends_with('…'));
        assert_eq!(short_title("  a \n b  "), "a b");
        assert_eq!(
            short_title(&"x".repeat(TITLE_CHARS)),
            "x".repeat(TITLE_CHARS)
        );
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
            origin: None,
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
        info.source_status = "elsewhere".into();
        info.origin = Some("MacBook-A".into());
        let r = resume_for(&info, "AgentKB");
        assert!(!r.available && r.command.is_none());
        let note = r.note.unwrap();
        assert!(note.contains("another device (MacBook-A)") && note.contains("--kb-show"));
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
