//! Reading and writing the digest bookkeeping tables (`digest_rounds`,
//! `digest_items`) and the small `meta` flags around them. This is run state
//! (retries, failures), not user data: losing it costs nothing but the
//! failure counters, because what has been digested is told by `notes/`.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::kb::db;

pub const BLOCKED_UNTIL: &str = "digest_blocked_until";
pub const BLOCKED_REASON: &str = "digest_blocked_reason";
pub const BLOCKED_MESSAGE: &str = "digest_blocked_message";
pub const FIRST_ROUND_SEEN: &str = "digest_first_round_seen";
/// How many finished rounds are kept for the history on the home page.
const ROUNDS_KEPT: i64 = 60;

fn err(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Round {
    pub round_id: String,
    pub kind: String,
    pub cutoff_at: String,
    pub planned_at: String,
    pub completed_at: Option<String>,
    pub state: String,
    pub trigger: String,
    pub engine: String,
    pub model: String,
}

const ROUND_COLUMNS: &str =
    "round_id, kind, cutoff_at, planned_at, completed_at, state, trigger, engine, model";

fn round_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Round> {
    Ok(Round {
        round_id: r.get(0)?,
        kind: r.get(1)?,
        cutoff_at: r.get(2)?,
        planned_at: r.get(3)?,
        completed_at: r.get(4)?,
        state: r.get(5)?,
        trigger: r.get(6)?,
        engine: r.get(7)?,
        model: r.get(8)?,
    })
}

pub fn get_round(conn: &Connection, round_id: &str) -> Result<Option<Round>, String> {
    conn.query_row(
        &format!("SELECT {ROUND_COLUMNS} FROM digest_rounds WHERE round_id=?1"),
        [round_id],
        round_from,
    )
    .optional()
    .map_err(|e| err("read round", e))
}

pub fn insert_round(conn: &Connection, round: &Round) -> Result<(), String> {
    conn.execute(
        &format!("INSERT INTO digest_rounds ({ROUND_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"),
        params![
            round.round_id,
            round.kind,
            round.cutoff_at,
            round.planned_at,
            round.completed_at,
            round.state,
            round.trigger,
            round.engine,
            round.model
        ],
    )
    .map_err(|e| err("insert round", e))?;
    Ok(())
}

/// Open rounds, oldest first.
pub fn open_rounds(conn: &Connection) -> Result<Vec<Round>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {ROUND_COLUMNS} FROM digest_rounds WHERE state='open' ORDER BY planned_at, rowid"
        ))
        .map_err(|e| err("prepare rounds", e))?;
    let rows = stmt
        .query_map([], round_from)
        .map_err(|e| err("read rounds", e))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| err("read rounds", e))
}

/// The newest round that finished (`done`), for the "last round" card.
pub fn last_done_round(conn: &Connection) -> Result<Option<Round>, String> {
    conn.query_row(
        &format!(
            "SELECT {ROUND_COLUMNS} FROM digest_rounds WHERE state='done' \
             ORDER BY completed_at DESC, rowid DESC LIMIT 1"
        ),
        [],
        round_from,
    )
    .optional()
    .map_err(|e| err("read last round", e))
}

pub fn any_round(conn: &Connection) -> Result<bool, String> {
    conn.query_row("SELECT 1 FROM digest_rounds LIMIT 1", [], |_| Ok(()))
        .optional()
        .map(|v| v.is_some())
        .map_err(|e| err("read rounds", e))
}

pub fn set_round_state(
    conn: &Connection,
    round_id: &str,
    state: &str,
    completed_at: Option<&str>,
) -> Result<(), String> {
    conn.execute(
        "UPDATE digest_rounds SET state=?2, completed_at=?3 WHERE round_id=?1",
        params![round_id, state, completed_at],
    )
    .map_err(|e| err("update round", e))?;
    Ok(())
}

/// Rounds of this kind that are still open stop being worked on (their
/// unfinished sessions are planned again by the next round).
pub fn supersede_open(conn: &Connection, kind: &str, except: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE digest_rounds SET state='superseded' WHERE state='open' AND kind=?1 AND round_id<>?2",
        params![kind, except],
    )
    .map_err(|e| err("supersede rounds", e))?;
    Ok(())
}

pub fn prune_rounds(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "DELETE FROM digest_rounds WHERE state<>'open' AND rowid <= \
         (SELECT COALESCE(MAX(rowid),0) FROM digest_rounds) - ?1",
        [ROUNDS_KEPT],
    )
    .map_err(|e| err("prune rounds", e))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Item {
    pub round_id: String,
    pub source: String,
    pub session_id: String,
    pub state: String,
    pub attempts: u32,
    pub next_try_at: Option<String>,
    pub last_error: Option<String>,
    pub seq_from: Option<u64>,
    pub seq_to: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub duration_ms: Option<u64>,
    pub finished_at: Option<String>,
}

const ITEM_COLUMNS: &str =
    "round_id, source, session_id, state, attempts, next_try_at, last_error, \
     seq_from, seq_to, input_tokens, output_tokens, duration_ms, finished_at";

fn unsigned(value: Option<i64>) -> Option<u64> {
    value.map(|v| u64::try_from(v).unwrap_or(0))
}

fn item_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        round_id: r.get(0)?,
        source: r.get(1)?,
        session_id: r.get(2)?,
        state: r.get(3)?,
        attempts: u32::try_from(r.get::<_, i64>(4)?).unwrap_or(0),
        next_try_at: r.get(5)?,
        last_error: r.get(6)?,
        seq_from: unsigned(r.get(7)?),
        seq_to: unsigned(r.get(8)?),
        input_tokens: unsigned(r.get(9)?),
        output_tokens: unsigned(r.get(10)?),
        duration_ms: unsigned(r.get(11)?),
        finished_at: r.get(12)?,
    })
}

pub fn add_item(
    conn: &Connection,
    round_id: &str,
    source: &str,
    session_id: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR IGNORE INTO digest_items (round_id, source, session_id, state) \
         VALUES (?1,?2,?3,'pending')",
        params![round_id, source, session_id],
    )
    .map_err(|e| err("add item", e))?;
    Ok(())
}

pub fn items(conn: &Connection, round_id: &str) -> Result<Vec<Item>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM digest_items WHERE round_id=?1 ORDER BY rowid"
        ))
        .map_err(|e| err("prepare items", e))?;
    let rows = stmt
        .query_map([round_id], item_from)
        .map_err(|e| err("read items", e))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| err("read items", e))
}

/// The first pending item whose retry time has come.
pub fn next_due_item(conn: &Connection, round_id: &str, now: &str) -> Result<Option<Item>, String> {
    conn.query_row(
        &format!(
            "SELECT {ITEM_COLUMNS} FROM digest_items WHERE round_id=?1 AND state='pending' \
             AND (next_try_at IS NULL OR next_try_at<=?2) ORDER BY rowid LIMIT 1"
        ),
        params![round_id, now],
        item_from,
    )
    .optional()
    .map_err(|e| err("read next item", e))
}

pub fn has_due_item(conn: &Connection, now: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM digest_items i JOIN digest_rounds r ON r.round_id=i.round_id \
         WHERE r.state='open' AND (i.state='running' OR \
           (i.state='pending' AND (i.next_try_at IS NULL OR i.next_try_at<=?1))) LIMIT 1",
        [now],
        |_| Ok(()),
    )
    .optional()
    .map(|v| v.is_some())
    .map_err(|e| err("look for due items", e))
}

/// Calling the AI is about to start: counted at once, so a crash loop still
/// runs out of attempts.
pub fn mark_running(conn: &Connection, item: &Item) -> Result<(), String> {
    conn.execute(
        "UPDATE digest_items SET state='running', attempts=attempts+1, next_try_at=NULL \
         WHERE round_id=?1 AND source=?2 AND session_id=?3",
        params![item.round_id, item.source, item.session_id],
    )
    .map_err(|e| err("mark running", e))?;
    Ok(())
}

pub struct Done {
    pub seq_from: u64,
    pub seq_to: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    pub messages: u64,
    pub covered: u64,
}

pub fn mark_done(conn: &Connection, item: &Item, done: &Done, now: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE digest_items SET state='done', last_error=NULL, next_try_at=NULL, seq_from=?4, \
         seq_to=?5, input_tokens=?6, output_tokens=?7, duration_ms=?8, finished_at=?9, \
         messages=?10, covered=?11 WHERE round_id=?1 AND source=?2 AND session_id=?3",
        params![
            item.round_id,
            item.source,
            item.session_id,
            i64::try_from(done.seq_from).unwrap_or(i64::MAX),
            i64::try_from(done.seq_to).unwrap_or(i64::MAX),
            i64::try_from(done.input_tokens).unwrap_or(i64::MAX),
            i64::try_from(done.output_tokens).unwrap_or(i64::MAX),
            i64::try_from(done.duration_ms).unwrap_or(i64::MAX),
            now,
            i64::try_from(done.messages).unwrap_or(i64::MAX),
            i64::try_from(done.covered).unwrap_or(i64::MAX)
        ],
    )
    .map_err(|e| err("mark done", e))?;
    Ok(())
}

/// Back to waiting. `refund` gives back the attempt that was counted when the
/// call started (the failure was not the session's fault).
pub fn mark_pending(
    conn: &Connection,
    item: &Item,
    refund: bool,
    next_try_at: Option<&str>,
    error: Option<&str>,
) -> Result<(), String> {
    conn.execute(
        "UPDATE digest_items SET state='pending', attempts=MAX(0, attempts-?4), next_try_at=?5, \
         last_error=COALESCE(?6, last_error) WHERE round_id=?1 AND source=?2 AND session_id=?3",
        params![
            item.round_id,
            item.source,
            item.session_id,
            i64::from(refund),
            next_try_at,
            error
        ],
    )
    .map_err(|e| err("mark pending", e))?;
    Ok(())
}

/// Out of attempts. `seq_to` is the newest message at that time: the session
/// is not tried again until it has newer ones.
pub fn mark_failed(
    conn: &Connection,
    item: &Item,
    error: &str,
    newest_seq: u64,
    now: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE digest_items SET state='failed', last_error=?4, next_try_at=NULL, seq_to=?5, \
         finished_at=?6 WHERE round_id=?1 AND source=?2 AND session_id=?3",
        params![
            item.round_id,
            item.source,
            item.session_id,
            error,
            i64::try_from(newest_seq).unwrap_or(i64::MAX),
            now
        ],
    )
    .map_err(|e| err("mark failed", e))?;
    Ok(())
}

pub fn mark_skipped(conn: &Connection, item: &Item, reason: &str, now: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE digest_items SET state='skipped', last_error=?4, next_try_at=NULL, finished_at=?5 \
         WHERE round_id=?1 AND source=?2 AND session_id=?3",
        params![item.round_id, item.source, item.session_id, reason, now],
    )
    .map_err(|e| err("mark skipped", e))?;
    Ok(())
}

/// Items left `running` by a process that died.
pub fn running_items(conn: &Connection) -> Result<Vec<Item>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM digest_items WHERE state='running'"
        ))
        .map_err(|e| err("prepare running", e))?;
    let rows = stmt
        .query_map([], item_from)
        .map_err(|e| err("read running", e))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| err("read running", e))
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Counts {
    pub pending: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
}

impl Counts {
    pub const fn total(&self) -> usize {
        self.pending + self.running + self.done + self.failed + self.skipped
    }
}

pub fn counts(conn: &Connection, round_id: &str) -> Result<Counts, String> {
    let mut out = Counts::default();
    for item in items(conn, round_id)? {
        match item.state.as_str() {
            "pending" => out.pending += 1,
            "running" => out.running += 1,
            "done" => out.done += 1,
            "failed" => out.failed += 1,
            _ => out.skipped += 1,
        }
        out.input_tokens += item.input_tokens.unwrap_or(0);
        out.output_tokens += item.output_tokens.unwrap_or(0);
        out.duration_ms += item.duration_ms.unwrap_or(0);
    }
    Ok(out)
}

/// The message kinds a digest reads. `input::load_items`, the candidate
/// count and the failure checks all use this one list (kind/seq only).
pub const INPUT_KINDS: &str = "'user','assistant','tool_use','tool_error'";

/// Highest seq of a digest-readable message of the session (0 when none).
pub fn newest_input_seq(conn: &Connection, source: &str, session_id: &str) -> Result<u64, String> {
    conn.query_row(
        &format!(
            "SELECT COALESCE(MAX(m.seq),0) FROM messages m JOIN sessions s ON s.file_id=m.file_id \
             WHERE s.source=?1 AND s.session_id=?2 AND m.kind IN ({INPUT_KINDS})"
        ),
        [source, session_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| u64::try_from(n).unwrap_or(0))
    .map_err(|e| err("read newest input", e))
}

/// Failed sessions that are still unresolved. A failure is resolved when every
/// digest-readable message inside its range (`seq <= seq_to`) is covered by the
/// digest cursor, so a tail of `tool_result` rows never keeps it alive; it also
/// stops counting once the session has grown readable messages past its range.
/// Worked out at read time; history rows are never changed.
pub fn failures(conn: &Connection) -> Result<Vec<Item>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM digest_items i WHERE i.state='failed' \
             AND EXISTS (SELECT 1 FROM messages m JOIN sessions s ON s.file_id=m.file_id \
                 WHERE s.source=i.source AND s.session_id=i.session_id \
                 AND m.kind IN ({INPUT_KINDS}) AND m.seq<=COALESCE(i.seq_to,0) \
                 AND m.seq>COALESCE((SELECT digested_seq FROM digest_sessions d \
                     WHERE d.source=i.source AND d.session_id=i.session_id),0)) \
             AND NOT EXISTS (SELECT 1 FROM messages m JOIN sessions s ON s.file_id=m.file_id \
                 WHERE s.source=i.source AND s.session_id=i.session_id \
                 AND m.kind IN ({INPUT_KINDS}) AND m.seq>COALESCE(i.seq_to,0)) \
             ORDER BY i.finished_at DESC"
        ))
        .map_err(|e| err("prepare failures", e))?;
    let rows = stmt
        .query_map([], item_from)
        .map_err(|e| err("read failures", e))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let item = row.map_err(|e| err("read failures", e))?;
        if seen.insert((item.source.clone(), item.session_id.clone())) {
            out.push(item);
        }
    }
    Ok(out)
}

/// A session that failed all its attempts and has nothing new since.
/// `newest_input_seq` is the newest digest-readable message (not `tool_result`).
pub fn is_held(
    conn: &Connection,
    source: &str,
    session_id: &str,
    newest_input_seq: u64,
) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM digest_items WHERE source=?1 AND session_id=?2 AND state='failed' \
         AND COALESCE(seq_to,0) >= ?3 LIMIT 1",
        params![
            source,
            session_id,
            i64::try_from(newest_input_seq).unwrap_or(i64::MAX)
        ],
        |_| Ok(()),
    )
    .optional()
    .map(|v| v.is_some())
    .map_err(|e| err("check held", e))
}

// ---- meta flags ----------------------------------------------------------

pub fn blocked(conn: &Connection) -> Result<Option<(String, String, String)>, String> {
    let Some(until) = db::get_meta(conn, BLOCKED_UNTIL)? else {
        return Ok(None);
    };
    Ok(Some((
        until,
        db::get_meta(conn, BLOCKED_REASON)?.unwrap_or_default(),
        db::get_meta(conn, BLOCKED_MESSAGE)?.unwrap_or_default(),
    )))
}

pub fn set_blocked(
    conn: &Connection,
    until: &str,
    reason: &str,
    message: &str,
) -> Result<(), String> {
    db::set_meta(conn, BLOCKED_UNTIL, until)?;
    db::set_meta(conn, BLOCKED_REASON, reason)?;
    db::set_meta(conn, BLOCKED_MESSAGE, message)
}

pub fn clear_blocked(conn: &Connection) -> Result<(), String> {
    db::delete_meta(conn, BLOCKED_UNTIL)?;
    db::delete_meta(conn, BLOCKED_REASON)?;
    db::delete_meta(conn, BLOCKED_MESSAGE)
}
