//! What should have a vector: read from `kb.db`, never written.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::chunk::{self, Chunk, NoteText};

/// Open `kb.db` for reading. `mode=ro` only: the `immutable` fallback the
/// search uses cannot see what is still in the write-ahead log, and a build
/// that misses the newest messages would believe they do not exist.
pub fn open_ro(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("cannot open kb.db read-only: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("busy_timeout: {e}"))?;
    Ok(conn)
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

/// Every piece that should be embedded, in the order it is worth building:
/// notes, then titles, then messages of the most recently active sessions
/// first. Sessions left out of search (by working directory) are left out.
pub fn desired(conn: &Connection) -> Result<Vec<Chunk>, String> {
    let q = |what: &str, e: rusqlite::Error| format!("{what}: {e}");
    let mut out: Vec<Chunk> = Vec::new();

    let mut sessions = conn
        .prepare(
            "SELECT file_id, source, session_id, title FROM sessions
             WHERE cwd_excluded = 0 ORDER BY last_activity_at DESC, file_id",
        )
        .map_err(|e| q("sessions", e))?;
    let rows: Vec<(i64, String, String, Option<String>)> = sessions
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| q("sessions", e))?
        .collect::<Result<_, _>>()
        .map_err(|e| q("sessions", e))?;
    let included: HashSet<(String, String)> = rows
        .iter()
        .map(|(_, s, id, _)| (s.clone(), id.clone()))
        .collect();

    if has_table(conn, "notes") {
        let mut stmt = conn
            .prepare(
                "SELECT source, session_id, entry_id, title, problem, cause, solution, conclusion, keywords
                 FROM notes ORDER BY digested_at DESC, id",
            )
            .map_err(|e| q("notes", e))?;
        let mut cursor = stmt.query([]).map_err(|e| q("notes", e))?;
        while let Some(r) = cursor.next().map_err(|e| q("notes", e))? {
            let (source, session): (String, String) = (
                r.get(0).map_err(|e| q("notes", e))?,
                r.get(1).map_err(|e| q("notes", e))?,
            );
            if !included.contains(&(source.clone(), session.clone())) {
                continue;
            }
            let field = |i: usize| -> Result<String, String> {
                r.get::<_, String>(i).map_err(|e| q("notes", e))
            };
            let (entry, title, problem, cause, solution, conclusion, keywords) = (
                field(2)?,
                field(3)?,
                field(4)?,
                field(5)?,
                field(6)?,
                field(7)?,
                field(8)?,
            );
            out.push(chunk::note_chunk(
                &source,
                &session,
                &NoteText {
                    entry_id: &entry,
                    title: &title,
                    problem: &problem,
                    cause: &cause,
                    solution: &solution,
                    conclusion: &conclusion,
                    keywords: &keywords,
                },
            ));
        }
    }

    for (_, source, session, title) in &rows {
        if let Some(t) = title {
            let redacted = crate::kb::redact::redact(t);
            out.extend(chunk::title_chunk(source, session, &redacted));
        }
    }

    let mut messages = conn
        .prepare("SELECT seq, kind, text FROM messages WHERE file_id = ?1 AND kind IN ('user','assistant') ORDER BY seq")
        .map_err(|e| q("messages", e))?;
    for (file_id, source, session, _) in &rows {
        let mut cursor = messages.query([file_id]).map_err(|e| q("messages", e))?;
        while let Some(r) = cursor.next().map_err(|e| q("messages", e))? {
            let seq: i64 = r.get(0).map_err(|e| q("messages", e))?;
            let kind: String = r.get(1).map_err(|e| q("messages", e))?;
            let text: String = r.get(2).map_err(|e| q("messages", e))?;
            let kind = if kind == "user" { "user" } else { "assistant" };
            out.extend(chunk::message_chunks(source, session, seq, kind, &text));
        }
    }
    Ok(out)
}
