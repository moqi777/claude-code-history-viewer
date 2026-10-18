//! What is sent to the AI for one session: a header, the notes written so far
//! (when the session continues an earlier digest), and the new messages.
//!
//! Messages come from the index (`messages`), which is already redacted and
//! free of `--kb-search` / `--kb-show` traffic; `raw/` is never read. Tool
//! output is not sent (it is bulk file content), only the command that was
//! run and errors. Sub-agent sessions are folded into their parent as one
//! block each (task + final answer).

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::kb::notes::NotesFile;
use crate::kb::redact::redact;

/// Characters of one request, cut at the start of a user turn. Measured: a
/// 100k-character request is about 70k tokens (see the design, §4.3).
pub const CHUNK_CHARS: usize = 50_000;
/// Rough tokens per character of the rendered input, measured on real runs.
pub const TOKENS_PER_CHAR: f64 = 0.73;
/// Fixed cost of one call: system prompt and schema.
pub const CALL_OVERHEAD_TOKENS: u64 = 2_500;

const CAP_USER: usize = 1200;
const CAP_ASSISTANT_HEAD: usize = 1100;
const CAP_ASSISTANT_TAIL: usize = 400;
const CAP_TOOL_USE: usize = 160;
const CAP_TOOL_ERROR: usize = 300;
const CAP_SUB_TASK: usize = 400;
const CAP_SUB_RESULT: usize = 1500;
const CAP_TITLE: usize = 120;

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub file_id: i64,
    pub source: String,
    pub session_id: String,
    pub title: Option<String>,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub last_activity_at: Option<String>,
}

pub fn session_info(
    conn: &Connection,
    source: &str,
    session_id: &str,
) -> Result<Option<SessionInfo>, String> {
    conn.query_row(
        "SELECT file_id, source, session_id, title, project_name, cwd, last_activity_at \
         FROM sessions WHERE source=?1 AND session_id=?2 AND is_subagent=0",
        params![source, session_id],
        |r| {
            Ok(SessionInfo {
                file_id: r.get(0)?,
                source: r.get(1)?,
                session_id: r.get(2)?,
                title: r.get(3)?,
                project: r.get(4)?,
                cwd: r.get(5)?,
                last_activity_at: r.get(6)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("read session: {e}"))
}

/// One piece of the conversation, in time order.
#[derive(Debug, Clone)]
enum Item {
    Message {
        seq: u64,
        user_turn_start: bool,
        ts: Option<String>,
        text: String,
    },
    SubAgent {
        text: String,
        ts: Option<String>,
    },
}

impl Item {
    fn chars(&self) -> usize {
        match self {
            Self::Message { text, .. } | Self::SubAgent { text, .. } => text.chars().count() + 1,
        }
    }
}

fn head(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut} […]")
    }
}

fn head_tail(text: &str, head_chars: usize, tail_chars: usize) -> String {
    let total = text.chars().count();
    if total <= head_chars + tail_chars + 8 {
        return text.to_string();
    }
    let first: String = text.chars().take(head_chars).collect();
    let last: String = text.chars().skip(total - tail_chars).collect();
    format!("{first} […] {last}")
}

fn short_ts(ts: Option<&str>) -> String {
    // "2026-10-15T03:12:43.512Z" -> "10-15T03:12"
    ts.and_then(|t| t.get(5..16)).unwrap_or("").to_string()
}

pub(super) fn parse_ts(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn render_message(seq: u64, kind: &str, ts: Option<&str>, text: &str) -> Option<String> {
    let (label, body) = match kind {
        "user" => ("user", head(text, CAP_USER)),
        "assistant" => (
            "assistant",
            head_tail(text, CAP_ASSISTANT_HEAD, CAP_ASSISTANT_TAIL),
        ),
        "tool_use" => ("tool", head(&text.replace('\n', " "), CAP_TOOL_USE)),
        "tool_error" => ("tool-error", head(text, CAP_TOOL_ERROR)),
        _ => return None,
    };
    Some(format!("[#{seq} {label} {}] {body}", short_ts(ts)))
}

/// Messages of the session after `after_seq` and not later than `cutoff`.
fn load_items(
    conn: &Connection,
    info: &SessionInfo,
    after_seq: u64,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Item>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT seq, kind, ts, text FROM messages \
             WHERE file_id=?1 AND seq>?2 AND kind IN ('user','assistant','tool_use','tool_error') \
             ORDER BY seq",
        )
        .map_err(|e| format!("prepare messages: {e}"))?;
    let rows = stmt
        .query_map(
            params![info.file_id, i64::try_from(after_seq).unwrap_or(i64::MAX)],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(|e| format!("read messages: {e}"))?;
    let mut items = Vec::new();
    for row in rows {
        let (seq, kind, ts, text) = row.map_err(|e| format!("read messages: {e}"))?;
        // Messages are chronological: the first one past the cut-off ends the round.
        if ts.as_deref().and_then(parse_ts).is_some_and(|t| t > cutoff) {
            break;
        }
        let seq = u64::try_from(seq).unwrap_or(0);
        if let Some(line) = render_message(seq, &kind, ts.as_deref(), &text) {
            items.push(Item::Message {
                seq,
                user_turn_start: kind == "user",
                ts,
                text: line,
            });
        }
    }
    Ok(items)
}

/// One block per sub-agent session of this session whose work falls after
/// `after_ts` and before the cut-off.
fn load_sub_agents(
    conn: &Connection,
    info: &SessionInfo,
    after_ts: Option<DateTime<Utc>>,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Item>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT file_id, title FROM sessions \
             WHERE source=?1 AND parent_session_id=?2 AND is_subagent=1 ORDER BY started_at",
        )
        .map_err(|e| format!("prepare sub-agents: {e}"))?;
    let subs: Vec<(i64, Option<String>)> = stmt
        .query_map(params![info.source, info.session_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| format!("read sub-agents: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("read sub-agents: {e}"))?;
    let mut out = Vec::new();
    for (file_id, title) in subs {
        let first: Option<(Option<String>, String)> = conn
            .query_row(
                "SELECT ts, text FROM messages WHERE file_id=?1 AND kind='user' ORDER BY seq LIMIT 1",
                [file_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("read sub-agent task: {e}"))?;
        let last: Option<String> = conn
            .query_row(
                "SELECT text FROM messages WHERE file_id=?1 AND kind='assistant' ORDER BY seq DESC LIMIT 1",
                [file_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("read sub-agent result: {e}"))?;
        let (Some((ts, task)), Some(result)) = (first, last) else {
            continue;
        };
        let at = ts.as_deref().and_then(parse_ts);
        if at.is_some_and(|t| t > cutoff) || matches!((at, after_ts), (Some(t), Some(a)) if t <= a)
        {
            continue;
        }
        let name = title
            .as_deref()
            .map(|t| head(&t.split_whitespace().collect::<Vec<_>>().join(" "), 60))
            .unwrap_or_default();
        out.push(Item::SubAgent {
            text: format!(
                "[sub-agent {} {}]\n  task: {}\n  result: {}",
                name,
                short_ts(ts.as_deref()),
                head(&task, CAP_SUB_TASK),
                head(&result, CAP_SUB_RESULT)
            ),
            ts,
        });
    }
    Ok(out)
}

/// Sub-agent blocks go before the first message that is not older than them.
fn interleave(messages: Vec<Item>, subs: Vec<Item>) -> Vec<Item> {
    let mut out = Vec::with_capacity(messages.len() + subs.len());
    let mut subs = subs.into_iter().peekable();
    for message in messages {
        let message_ts = match &message {
            Item::Message { ts, .. } => ts.as_deref().and_then(parse_ts),
            Item::SubAgent { .. } => None,
        };
        while let Some(sub) = subs.peek() {
            let sub_ts = match sub {
                Item::SubAgent { ts, .. } => ts.as_deref().and_then(parse_ts),
                Item::Message { .. } => None,
            };
            if matches!((sub_ts, message_ts), (Some(s), Some(m)) if s <= m) {
                out.extend(subs.next());
            } else {
                break;
            }
        }
        out.push(message);
    }
    out.extend(subs);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The complete stdin text.
    pub text: String,
    pub from_seq: u64,
    pub to_seq: u64,
    /// Characters of the messages part (what the size limit counts).
    pub chars: usize,
    /// Messages remain after this chunk.
    pub more: bool,
}

/// First index of an item to leave for the next chunk: the last user-turn
/// start inside the chunk, so a turn is never split; or the hard limit when
/// one turn alone is larger than the chunk.
fn cut_point(items: &[Item], limit: usize) -> usize {
    let mut used = 0;
    let mut last_turn_start = None;
    for (i, item) in items.iter().enumerate() {
        let size = item.chars();
        if used > 0 && used + size > limit {
            return last_turn_start.filter(|&s| s > 0).unwrap_or(i);
        }
        used += size;
        if matches!(
            item,
            Item::Message {
                user_turn_start: true,
                ..
            }
        ) {
            last_turn_start = Some(i);
        }
    }
    items.len()
}

/// The next piece to send for a session, or `None` when nothing new is left.
/// `after_seq` is how far it has been digested; `notes` are its notes so far.
pub fn next_chunk(
    conn: &Connection,
    info: &SessionInfo,
    after_seq: u64,
    cutoff: DateTime<Utc>,
    notes: Option<&NotesFile>,
    limit: usize,
) -> Result<Option<Chunk>, String> {
    let messages = load_items(conn, info, after_seq, cutoff)?;
    if messages.is_empty() {
        return Ok(None);
    }
    let digested_ts: Option<DateTime<Utc>> = conn
        .query_row(
            "SELECT ts FROM messages WHERE file_id=?1 AND seq<=?2 AND ts IS NOT NULL \
             ORDER BY seq DESC LIMIT 1",
            params![info.file_id, i64::try_from(after_seq).unwrap_or(i64::MAX)],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("read digested time: {e}"))?
        .as_deref()
        .and_then(parse_ts);
    let subs = load_sub_agents(conn, info, digested_ts, cutoff)?;
    let items = interleave(messages, subs);
    let cut = cut_point(&items, limit);
    let (now, rest) = items.split_at(cut);
    let more = rest.iter().any(|i| matches!(i, Item::Message { .. }));

    let mut seqs = now.iter().filter_map(|i| match i {
        Item::Message { seq, .. } => Some(*seq),
        Item::SubAgent { .. } => None,
    });
    let Some(from_seq) = seqs.next() else {
        return Ok(None);
    };
    let to_seq = seqs.next_back().unwrap_or(from_seq);
    let first_ts = now.iter().find_map(|i| match i {
        Item::Message { ts, .. } => ts.clone(),
        Item::SubAgent { .. } => None,
    });
    let last_ts = now.iter().rev().find_map(|i| match i {
        Item::Message { ts, .. } => ts.clone(),
        Item::SubAgent { .. } => None,
    });
    let body: Vec<&str> = now
        .iter()
        .map(|i| match i {
            Item::Message { text, .. } | Item::SubAgent { text, .. } => text.as_str(),
        })
        .collect();
    let body = body.join("\n");

    let mut text = format!(
        "<session>\nsource: {}\ntitle: {}\nproject: {}\ncwd: {}\nactive: {} .. {}\n</session>\n",
        info.source,
        head(
            &info
                .title
                .as_deref()
                .unwrap_or("(untitled)")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            CAP_TITLE
        ),
        info.project.as_deref().unwrap_or("-"),
        info.cwd.as_deref().unwrap_or("-"),
        first_ts.as_deref().unwrap_or("?"),
        last_ts.as_deref().unwrap_or("?"),
    );
    if let Some(notes) = notes.filter(|n| !n.entries.is_empty()) {
        let previous: Vec<serde_json::Value> = notes
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id, "title": e.title, "problem": e.problem, "cause": e.cause,
                    "solution": e.solution, "conclusion": e.conclusion, "keywords": e.keywords,
                    "seq_from": e.evidence.seq_from, "seq_to": e.evidence.seq_to,
                })
            })
            .collect();
        text.push_str(&format!(
            "<previous_notes>\n{}\n</previous_notes>\n",
            serde_json::to_string(&previous).map_err(|e| e.to_string())?
        ));
    }
    text.push_str(&format!(
        "<messages from=#{from_seq} to=#{to_seq}>\n{body}\n</messages>\n"
    ));
    Ok(Some(Chunk {
        // A second pass: nothing leaves the machine without being redacted
        // again, whatever the index holds.
        text: redact(&text).into_owned(),
        from_seq,
        to_seq,
        chars: body.chars().count(),
        more,
    }))
}

/// Size of everything a session still has to send, for the cost estimate:
/// the number of calls and the input tokens they will need, assuming at most
/// `max_chunks` calls.
pub fn estimate_session(
    conn: &Connection,
    info: &SessionInfo,
    after_seq: u64,
    cutoff: DateTime<Utc>,
    max_chunks: usize,
) -> Result<(u64, u64), String> {
    let mut calls = 0u64;
    let mut tokens = 0u64;
    let mut from = after_seq;
    for _ in 0..max_chunks {
        let Some(chunk) = next_chunk(conn, info, from, cutoff, None, CHUNK_CHARS)? else {
            break;
        };
        calls += 1;
        tokens += CALL_OVERHEAD_TOKENS + (chunk.chars as f64 * TOKENS_PER_CHAR) as u64;
        from = chunk.to_seq;
        if !chunk.more {
            break;
        }
    }
    Ok((calls, tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::digest::testutil::TestKb;

    fn cutoff() -> DateTime<Utc> {
        "2026-10-16T03:00:00Z".parse().unwrap()
    }

    fn info(kb: &TestKb, id: &str) -> SessionInfo {
        session_info(&kb.conn, "claude", id).unwrap().unwrap()
    }

    #[test]
    fn the_input_has_a_header_numbered_messages_and_no_tool_output() {
        let kb = TestKb::new();
        let file = kb.session("claude", "s1", "/p/app", None, "2026-10-15T10:00:00Z");
        kb.messages(
            file,
            1,
            &[
                ("user", "2026-10-15T09:00:00Z", "why does it deadlock?"),
                ("tool_use", "2026-10-15T09:00:05Z", "Bash cargo test"),
                ("tool_result", "2026-10-15T09:00:09Z", "HUGE OUTPUT"),
                ("tool_error", "2026-10-15T09:00:10Z", "error: lock poisoned"),
                (
                    "assistant",
                    "2026-10-15T09:01:00Z",
                    "the two locks are taken in opposite order",
                ),
            ],
        );
        let chunk = next_chunk(&kb.conn, &info(&kb, "s1"), 0, cutoff(), None, CHUNK_CHARS)
            .unwrap()
            .unwrap();
        assert_eq!((chunk.from_seq, chunk.to_seq, chunk.more), (1, 5, false));
        assert!(chunk.text.contains("<messages from=#1 to=#5>"));
        assert!(chunk
            .text
            .contains("[#1 user 10-15T09:00] why does it deadlock?"));
        assert!(chunk.text.contains("[#2 tool 10-15T09:00] Bash cargo test"));
        assert!(chunk
            .text
            .contains("[#4 tool-error 10-15T09:00] error: lock poisoned"));
        assert!(chunk.text.contains("[#5 assistant 10-15T09:01]"));
        assert!(
            !chunk.text.contains("HUGE OUTPUT"),
            "tool results are not sent"
        );
        assert!(chunk.text.contains("cwd: /p/app"));
        assert!(!chunk.text.contains("<previous_notes>"));
    }

    #[test]
    fn only_messages_after_the_digested_seq_and_before_the_cutoff_are_sent() {
        let kb = TestKb::new();
        let file = kb.session("claude", "s1", "/p", None, "2026-10-16T05:00:00Z");
        kb.messages(
            file,
            1,
            &[
                ("user", "2026-10-15T09:00:00Z", "old question"),
                ("assistant", "2026-10-15T09:01:00Z", "old answer"),
                ("user", "2026-10-16T02:00:00Z", "new question"),
                ("assistant", "2026-10-16T02:01:00Z", "new answer"),
                ("user", "2026-10-16T04:00:00Z", "after the cut-off"),
            ],
        );
        let chunk = next_chunk(&kb.conn, &info(&kb, "s1"), 2, cutoff(), None, CHUNK_CHARS)
            .unwrap()
            .unwrap();
        assert_eq!((chunk.from_seq, chunk.to_seq), (3, 4));
        assert!(chunk.text.contains("new question"));
        assert!(!chunk.text.contains("old question"));
        assert!(!chunk.text.contains("after the cut-off"));
        assert!(
            next_chunk(&kb.conn, &info(&kb, "s1"), 4, cutoff(), None, CHUNK_CHARS)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn previous_notes_are_included_when_the_session_continues() {
        let kb = TestKb::new();
        let file = kb.session("claude", "s1", "/p", None, "2026-10-16T01:00:00Z");
        kb.messages(
            file,
            1,
            &[("user", "2026-10-16T01:00:00Z", "and one more thing")],
        );
        let notes = crate::kb::notes::testing::sample("claude", "s1", 1);
        let chunk = next_chunk(
            &kb.conn,
            &info(&kb, "s1"),
            0,
            cutoff(),
            Some(&notes),
            CHUNK_CHARS,
        )
        .unwrap()
        .unwrap();
        assert!(chunk.text.contains("<previous_notes>"));
        assert!(chunk.text.contains("\"id\":\"n1\""));
        assert!(chunk.text.contains("死锁排查 1"));
    }

    #[test]
    fn long_messages_are_cut_to_the_documented_caps() {
        let kb = TestKb::new();
        let file = kb.session("claude", "s1", "/p", None, "2026-10-16T01:00:00Z");
        let user = "u".repeat(5000);
        let assistant = format!("{}{}", "a".repeat(3000), "Z".repeat(400));
        kb.messages(
            file,
            1,
            &[
                ("user", "2026-10-16T01:00:00Z", &user),
                ("assistant", "2026-10-16T01:01:00Z", &assistant),
            ],
        );
        let chunk = next_chunk(&kb.conn, &info(&kb, "s1"), 0, cutoff(), None, CHUNK_CHARS)
            .unwrap()
            .unwrap();
        assert!(chunk.chars < 1200 + 1100 + 400 + 200, "{}", chunk.chars);
        assert!(chunk.text.contains("[…]"));
        assert!(
            chunk.text.contains(&"Z".repeat(400)),
            "the end of an answer is kept"
        );
    }

    #[test]
    fn a_long_session_is_cut_at_the_start_of_a_user_turn_and_continues_from_there() {
        let kb = TestKb::new();
        let file = kb.session("claude", "s1", "/p", None, "2026-10-16T01:00:00Z");
        let mut rows = Vec::new();
        for turn in 0..10 {
            rows.push((
                "user",
                "2026-10-16T01:00:00Z",
                format!("question {turn} {}", "q".repeat(300)),
            ));
            rows.push((
                "assistant",
                "2026-10-16T01:00:30Z",
                format!("answer {turn} {}", "a".repeat(300)),
            ));
        }
        let borrowed: Vec<(&str, &str, &str)> =
            rows.iter().map(|(k, t, x)| (*k, *t, x.as_str())).collect();
        kb.messages(file, 1, &borrowed);

        let first = next_chunk(&kb.conn, &info(&kb, "s1"), 0, cutoff(), None, 2000)
            .unwrap()
            .unwrap();
        assert!(first.more);
        assert!(first.chars <= 2000);
        assert_eq!(
            first.to_seq % 2,
            0,
            "the cut falls before a user turn, after an answer"
        );
        assert!(first.text.contains("[#1 user"));

        let second = next_chunk(
            &kb.conn,
            &info(&kb, "s1"),
            first.to_seq,
            cutoff(),
            None,
            2000,
        )
        .unwrap()
        .unwrap();
        assert_eq!(second.from_seq, first.to_seq + 1);
        assert_eq!(
            second.from_seq % 2,
            1,
            "the next piece starts with a user message"
        );

        // Walking the pieces covers everything exactly once.
        let mut seen = 0;
        let mut from = 0;
        while let Some(chunk) =
            next_chunk(&kb.conn, &info(&kb, "s1"), from, cutoff(), None, 2000).unwrap()
        {
            assert_eq!(chunk.from_seq, from + 1);
            seen += chunk.to_seq - chunk.from_seq + 1;
            from = chunk.to_seq;
            if !chunk.more {
                break;
            }
        }
        assert_eq!(seen, 20);
    }

    #[test]
    fn a_single_oversized_turn_is_still_cut_so_progress_is_made() {
        let kb = TestKb::new();
        let file = kb.session("claude", "s1", "/p", None, "2026-10-16T01:00:00Z");
        let mut rows = vec![("user", "2026-10-16T01:00:00Z", "start".to_string())];
        for i in 0..30 {
            rows.push((
                "tool_use",
                "2026-10-16T01:00:01Z",
                format!("Bash step {i} {}", "x".repeat(100)),
            ));
        }
        let borrowed: Vec<(&str, &str, &str)> =
            rows.iter().map(|(k, t, x)| (*k, *t, x.as_str())).collect();
        kb.messages(file, 1, &borrowed);
        let chunk = next_chunk(&kb.conn, &info(&kb, "s1"), 0, cutoff(), None, 800)
            .unwrap()
            .unwrap();
        assert!(chunk.more);
        assert!(chunk.to_seq > 1 && chunk.to_seq < 31);
    }

    #[test]
    fn sub_agents_are_folded_into_the_parent_as_task_and_result() {
        let kb = TestKb::new();
        let parent = kb.session("claude", "s1", "/p", None, "2026-10-16T01:00:00Z");
        kb.messages(
            parent,
            1,
            &[
                ("user", "2026-10-16T00:00:00Z", "find the bug"),
                ("assistant", "2026-10-16T00:30:00Z", "found it"),
            ],
        );
        let sub = kb.session(
            "claude",
            "agent-a1",
            "/p",
            Some("s1"),
            "2026-10-16T00:10:00Z",
        );
        kb.messages(
            sub,
            1,
            &[
                ("user", "2026-10-16T00:05:00Z", "search the repo for locks"),
                ("tool_use", "2026-10-16T00:06:00Z", "Grep lock"),
                (
                    "assistant",
                    "2026-10-16T00:09:00Z",
                    "lock order differs in a.rs and b.rs",
                ),
            ],
        );
        let chunk = next_chunk(&kb.conn, &info(&kb, "s1"), 0, cutoff(), None, CHUNK_CHARS)
            .unwrap()
            .unwrap();
        assert!(chunk.text.contains("[sub-agent"));
        assert!(chunk.text.contains("task: search the repo for locks"));
        assert!(chunk
            .text
            .contains("result: lock order differs in a.rs and b.rs"));
        assert!(
            !chunk.text.contains("Grep lock"),
            "only the task and the final answer"
        );
        let at = |needle: &str| chunk.text.find(needle).unwrap();
        assert!(at("find the bug") < at("[sub-agent") && at("[sub-agent") < at("found it"));
        assert_eq!(
            (chunk.from_seq, chunk.to_seq),
            (1, 2),
            "sub-agents have no seq of their own"
        );
    }

    #[test]
    fn secrets_that_reached_the_index_are_still_removed_before_sending() {
        let kb = TestKb::new();
        let file = kb.session("claude", "s1", "/p", None, "2026-10-16T01:00:00Z");
        kb.messages(
            file,
            1,
            &[(
                "user",
                "2026-10-16T01:00:00Z",
                "my password=hunter2hunter2 and key sk-abcdefghijklmnopqrstuvwxyz0123",
            )],
        );
        let chunk = next_chunk(&kb.conn, &info(&kb, "s1"), 0, cutoff(), None, CHUNK_CHARS)
            .unwrap()
            .unwrap();
        assert!(!chunk.text.contains("hunter2"));
        assert!(!chunk.text.contains("sk-abcdefghijkl"));
    }

    #[test]
    fn the_estimate_counts_calls_and_tokens_and_respects_the_chunk_cap() {
        let kb = TestKb::new();
        let file = kb.session("claude", "s1", "/p", None, "2026-10-16T01:00:00Z");
        let mut rows = Vec::new();
        for turn in 0..40 {
            rows.push((
                "user",
                "2026-10-16T01:00:00Z",
                format!("question {turn} {}", "q".repeat(500)),
            ));
            rows.push((
                "assistant",
                "2026-10-16T01:00:30Z",
                format!("answer {turn} {}", "a".repeat(500)),
            ));
        }
        let borrowed: Vec<(&str, &str, &str)> =
            rows.iter().map(|(k, t, x)| (*k, *t, x.as_str())).collect();
        kb.messages(file, 1, &borrowed);
        let (calls, tokens) = estimate_session(&kb.conn, &info(&kb, "s1"), 0, cutoff(), 3).unwrap();
        assert_eq!(calls, 1, "40 turns of ~1.1k characters fit one 100k chunk");
        assert!(tokens > CALL_OVERHEAD_TOKENS && tokens < 60_000, "{tokens}");
    }
}
