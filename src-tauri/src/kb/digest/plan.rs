//! Which sessions a round digests, and what that will cost.
//!
//! A session qualifies when it is a main session (not a sub-agent), not in an
//! excluded working directory, not one of the digest's own, has enough new
//! conversation, and (for automatic rounds) was active recently. The same
//! rules answer "what is pending" for the home page and "how many tokens"
//! for the confirmation dialog, so they live in one place.

use std::collections::HashMap;

use chrono::{DateTime, Duration, FixedOffset, TimeZone, Utc};
use rusqlite::{params, Connection};

use super::input::{self, SessionInfo};
use crate::kb::config::DigestConfig;
use crate::kb::db;
use crate::kb::paths::DataPaths;

/// New user/assistant messages a session that already has notes needs before
/// it is worth another call.
pub const MIN_NEW_MESSAGES_UPDATE: usize = 2;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub info: SessionInfo,
    pub digested_seq: u64,
    pub partial: bool,
    /// The later of the session's and its sub-agents' last activity.
    pub activity: DateTime<Utc>,
    pub new_messages: usize,
    pub newest_seq: u64,
}

pub struct Rules<'a> {
    pub cfg: &'a DigestConfig,
    pub data: &'a DataPaths,
    pub cutoff: DateTime<Utc>,
    pub now: DateTime<Utc>,
    /// Automatic rounds only look `recent_days` back (unfinished long
    /// sessions excepted).
    pub recent_only: bool,
    /// Leave out sessions with a message newer than `quiet_minutes`.
    pub quiet: bool,
    /// Leave out sessions that failed all attempts and have nothing new.
    pub honour_failures: bool,
}

fn parse(ts: Option<&str>) -> Option<DateTime<Utc>> {
    ts.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

pub fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Qualifying sessions, most recently active first.
pub fn candidates(conn: &Connection, rules: &Rules<'_>) -> Result<Vec<Candidate>, String> {
    let mut digested: HashMap<(String, String), (u64, bool)> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT source, session_id, digested_seq, status FROM digest_sessions")
            .map_err(|e| format!("read digest state: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                    (
                        u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                        r.get::<_, String>(3)? == "partial",
                    ),
                ))
            })
            .map_err(|e| format!("read digest state: {e}"))?;
        for row in rows {
            let (key, value) = row.map_err(|e| format!("read digest state: {e}"))?;
            digested.insert(key, value);
        }
    }
    let work = rules.data.digest_work().to_string_lossy().into_owned();
    let work_rule = [work];
    let cutoff_text = rfc3339(rules.cutoff);
    let recent_from = rules.now - Duration::days(i64::from(rules.cfg.recent_days));
    let quiet_from = rules.now - Duration::minutes(i64::from(rules.cfg.quiet_minutes));

    let mut stmt = conn
        .prepare(
            "SELECT s.file_id, s.source, s.session_id, s.title, s.project_name, s.cwd, \
                    s.last_activity_at, \
                    (SELECT MAX(c.last_activity_at) FROM sessions c WHERE c.source=s.source \
                       AND c.parent_session_id=s.session_id AND c.is_subagent=1), \
                    COALESCE((SELECT MAX(seq) FROM messages m WHERE m.file_id=s.file_id), 0) \
             FROM sessions s WHERE s.is_subagent=0 AND s.cwd_excluded=0",
        )
        .map_err(|e| format!("prepare sessions: {e}"))?;
    let mut count = conn
        .prepare(
            "SELECT COUNT(*) FROM messages WHERE file_id=?1 AND seq>?2 \
             AND kind IN ('user','assistant') AND (ts IS NULL OR ts<=?3)",
        )
        .map_err(|e| format!("prepare count: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                SessionInfo {
                    file_id: r.get(0)?,
                    source: r.get(1)?,
                    session_id: r.get(2)?,
                    title: r.get(3)?,
                    project: r.get(4)?,
                    cwd: r.get(5)?,
                    last_activity_at: r.get(6)?,
                },
                r.get::<_, Option<String>>(7)?,
                u64::try_from(r.get::<_, i64>(8)?).unwrap_or(0),
            ))
        })
        .map_err(|e| format!("read sessions: {e}"))?;

    let mut out = Vec::new();
    for row in rows {
        let (info, child_activity, newest_seq) = row.map_err(|e| format!("read sessions: {e}"))?;
        // The digest's own working directory is never digested, whatever the
        // user's exclusion list says.
        if db::is_cwd_excluded(info.cwd.as_deref(), &work_rule) {
            continue;
        }
        let activity = [
            parse(info.last_activity_at.as_deref()),
            parse(child_activity.as_deref()),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(DateTime::UNIX_EPOCH);
        let (digested_seq, partial) = digested
            .get(&(info.source.clone(), info.session_id.clone()))
            .copied()
            .unwrap_or((0, false));
        let has_notes = digested.contains_key(&(info.source.clone(), info.session_id.clone()));
        if rules.recent_only && activity < recent_from && !partial {
            continue;
        }
        if rules.quiet && activity > quiet_from {
            continue;
        }
        let new_messages: usize = count
            .query_row(
                params![
                    info.file_id,
                    i64::try_from(digested_seq).unwrap_or(i64::MAX),
                    cutoff_text
                ],
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| format!("count messages: {e}"))
            .map(|n| usize::try_from(n).unwrap_or(0))?;
        let needed = if has_notes {
            MIN_NEW_MESSAGES_UPDATE
        } else {
            rules.cfg.min_messages
        };
        if new_messages < needed.min(if partial { 1 } else { usize::MAX }) {
            continue;
        }
        if rules.honour_failures
            && super::store::is_held(conn, &info.source, &info.session_id, newest_seq)?
        {
            continue;
        }
        out.push(Candidate {
            info,
            digested_seq,
            partial,
            activity,
            new_messages,
            newest_seq,
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.activity));
    Ok(out)
}

// ---------------------------------------------------------------------------
// The round boundary
// ---------------------------------------------------------------------------

/// The latest digest time that is not in the future, as a UTC instant, and
/// the local date that names its round.
pub fn latest_cutoff(
    now: DateTime<Utc>,
    time_of_day: (u32, u32),
    tz: FixedOffset,
) -> (DateTime<Utc>, String) {
    let local = now.with_timezone(&tz);
    let at = |days_back: i64| {
        let date = local.date_naive() - Duration::days(days_back);
        let naive = date
            .and_hms_opt(time_of_day.0, time_of_day.1, 0)
            .unwrap_or_default();
        tz.from_local_datetime(&naive)
            .single()
            .map(|t| (t.with_timezone(&Utc), date.format("%Y-%m-%d").to_string()))
    };
    let today = at(0);
    match today {
        Some((cutoff, date)) if cutoff <= now => (cutoff, date),
        _ => at(1).unwrap_or((now, local.format("%Y-%m-%d").to_string())),
    }
}

/// The next time a round becomes due, for the home page.
pub fn next_cutoff(now: DateTime<Utc>, time_of_day: (u32, u32), tz: FixedOffset) -> DateTime<Utc> {
    let (latest, _) = latest_cutoff(now, time_of_day, tz);
    latest + Duration::days(1)
}

// ---------------------------------------------------------------------------
// Estimates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct Estimate {
    pub sessions: usize,
    pub calls: u64,
    pub input_tokens: u64,
    /// Rough wall-clock minutes at the speed measured for each CLI.
    pub minutes: u64,
}

/// Measured seconds per call (design, §6): Claude ~11 s, Codex ~95 s.
pub fn seconds_per_call(engine: &str) -> u64 {
    if engine == "codex" {
        95
    } else {
        11
    }
}

pub fn estimate(
    conn: &Connection,
    cfg: &DigestConfig,
    candidates: &[Candidate],
    cutoff: DateTime<Utc>,
) -> Result<Estimate, String> {
    let mut total = Estimate {
        sessions: candidates.len(),
        ..Estimate::default()
    };
    for candidate in candidates {
        let (calls, tokens) = input::estimate_session(
            conn,
            &candidate.info,
            candidate.digested_seq,
            cutoff,
            cfg.max_chunks_per_session,
        )?;
        total.calls += calls;
        total.input_tokens += tokens;
    }
    total.minutes = (total.calls * seconds_per_call(&cfg.engine)).div_ceil(60);
    Ok(total)
}

/// Average input tokens of a day's digest, estimated from the sessions active
/// in the last `recent_days` days as if none had been digested: the design's
/// §6 method. `None` when nothing was active.
pub fn daily_estimate(
    conn: &Connection,
    cfg: &DigestConfig,
    data: &DataPaths,
    now: DateTime<Utc>,
    tz: FixedOffset,
) -> Result<Option<u64>, String> {
    let rules = Rules {
        cfg,
        data,
        cutoff: now,
        now,
        recent_only: true,
        quiet: false,
        honour_failures: false,
    };
    let mut per_day: HashMap<chrono::NaiveDate, u64> = HashMap::new();
    for candidate in candidates(conn, &rules)? {
        let (_, tokens) =
            input::estimate_session(conn, &candidate.info, 0, now, cfg.max_chunks_per_session)?;
        let day = candidate.activity.with_timezone(&tz).date_naive();
        *per_day.entry(day).or_default() += tokens;
    }
    if per_day.is_empty() {
        return Ok(None);
    }
    Ok(Some(per_day.values().sum::<u64>() / per_day.len() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::digest::testutil::TestKb;

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn cn() -> FixedOffset {
        FixedOffset::east_opt(8 * 3600).unwrap()
    }

    #[test]
    fn the_cutoff_is_the_latest_digest_time_not_in_the_future() {
        // 03:00 in UTC+8 is 19:00 UTC of the day before.
        let (cut, id) = latest_cutoff(utc("2026-10-16T08:30:00Z"), (3, 0), cn());
        assert_eq!(
            (cut, id.as_str()),
            (utc("2026-10-15T19:00:00Z"), "2026-10-16")
        );
        // 01:00 local on the 16th: today's 03:00 has not come, so it is the 15th's.
        let (cut, id) = latest_cutoff(utc("2026-10-15T17:00:00Z"), (3, 0), cn());
        assert_eq!(
            (cut, id.as_str()),
            (utc("2026-10-14T19:00:00Z"), "2026-10-15")
        );
        // Exactly at the digest time counts as due.
        let (cut, _) = latest_cutoff(utc("2026-10-15T19:00:00Z"), (3, 0), cn());
        assert_eq!(cut, utc("2026-10-15T19:00:00Z"));
        assert_eq!(
            next_cutoff(utc("2026-10-16T08:30:00Z"), (3, 0), cn()),
            utc("2026-10-16T19:00:00Z")
        );
    }

    fn rules<'a>(
        cfg: &'a DigestConfig,
        data: &'a DataPaths,
        now: &str,
        recent_only: bool,
    ) -> Rules<'a> {
        Rules {
            cfg,
            data,
            cutoff: utc(now),
            now: utc(now),
            recent_only,
            quiet: true,
            honour_failures: true,
        }
    }

    #[test]
    fn the_skip_rules_apply() {
        let kb = TestKb::new();
        let cfg = DigestConfig::default();
        let now = "2026-10-16T04:00:00Z";
        let work = kb.data.digest_work().to_string_lossy().into_owned();

        let good = kb.session("claude", "good", "/p", None, "2026-10-16T01:00:00Z");
        kb.chat(good, 6, "2026-10-16T00:00:00Z");
        let few = kb.session("claude", "few", "/p", None, "2026-10-16T01:00:00Z");
        kb.chat(few, 2, "2026-10-16T00:00:00Z");
        let busy = kb.session("claude", "busy", "/p", None, "2026-10-16T03:50:00Z");
        kb.chat(busy, 6, "2026-10-16T03:40:00Z");
        let old = kb.session("claude", "old", "/p", None, "2026-09-01T01:00:00Z");
        kb.chat(old, 6, "2026-09-01T00:00:00Z");
        let sub = kb.session(
            "claude",
            "agent-x",
            "/p",
            Some("good"),
            "2026-10-16T01:00:00Z",
        );
        kb.chat(sub, 6, "2026-10-16T00:00:00Z");
        let own = kb.session("claude", "own", &work, None, "2026-10-16T01:00:00Z");
        kb.chat(own, 6, "2026-10-16T00:00:00Z");
        let excluded = kb.session("claude", "excluded", "/p", None, "2026-10-16T01:00:00Z");
        kb.chat(excluded, 6, "2026-10-16T00:00:00Z");
        kb.conn
            .execute(
                "UPDATE sessions SET cwd_excluded=1 WHERE session_id='excluded'",
                [],
            )
            .unwrap();

        let ids = |recent_only: bool| -> Vec<String> {
            candidates(&kb.conn, &rules(&cfg, &kb.data, now, recent_only))
                .unwrap()
                .into_iter()
                .map(|c| c.info.session_id)
                .collect()
        };
        assert_eq!(
            ids(true),
            ["good"],
            "auto: sub-agent, own, excluded, few, busy, old all skipped"
        );
        assert_eq!(
            ids(false),
            ["good", "old"],
            "manual: old sessions count, quiet ones still wait"
        );
    }

    #[test]
    fn a_digested_session_needs_new_messages_and_a_failed_one_waits_for_them() {
        let kb = TestKb::new();
        let cfg = DigestConfig::default();
        let now = "2026-10-16T04:00:00Z";
        let file = kb.session("claude", "s", "/p", None, "2026-10-16T01:00:00Z");
        kb.chat(file, 6, "2026-10-16T00:00:00Z");
        let mut notes = crate::kb::notes::testing::sample("claude", "s", 1);
        notes.digested_through_seq = 6;
        let hash = crate::kb::notes::write(&kb.data, &notes).unwrap();
        crate::kb::notes::index_file(&kb.conn, &notes, &hash).unwrap();
        let count = |kb: &TestKb| {
            candidates(&kb.conn, &rules(&cfg, &kb.data, now, true))
                .unwrap()
                .len()
        };
        assert_eq!(count(&kb), 0, "everything is digested");

        kb.messages(file, 7, &[("user", "2026-10-16T01:10:00Z", "one more")]);
        assert_eq!(count(&kb), 0, "one new message is too few for an update");
        kb.messages(file, 8, &[("assistant", "2026-10-16T01:11:00Z", "answer")]);
        assert_eq!(count(&kb), 1);

        // Three failed attempts: held until something newer arrives.
        kb.conn
            .execute(
                "INSERT INTO digest_rounds VALUES ('r1','daily','x','x',NULL,'open','auto','claude','')",
                [],
            )
            .unwrap();
        kb.conn
            .execute(
                "INSERT INTO digest_items (round_id, source, session_id, state, attempts, seq_to) \
                 VALUES ('r1','claude','s','failed',3,8)",
                [],
            )
            .unwrap();
        assert_eq!(count(&kb), 0, "held after three failures");
        kb.chat_more(file, 9);
        assert_eq!(count(&kb), 1, "new messages lift the hold");
    }

    #[test]
    fn estimates_add_up_the_calls_and_tokens_of_the_candidates() {
        let kb = TestKb::new();
        let cfg = DigestConfig::default();
        for id in ["a", "b"] {
            let file = kb.session("claude", id, "/p", None, "2026-10-16T01:00:00Z");
            kb.chat(file, 6, "2026-10-16T00:00:00Z");
        }
        let now = utc("2026-10-16T04:00:00Z");
        let found = candidates(
            &kb.conn,
            &rules(&cfg, &kb.data, "2026-10-16T04:00:00Z", true),
        )
        .unwrap();
        let total = estimate(&kb.conn, &cfg, &found, now).unwrap();
        assert_eq!((total.sessions, total.calls), (2, 2));
        assert!(total.input_tokens > 2 * input::CALL_OVERHEAD_TOKENS);
        assert_eq!(total.minutes, 1);
        let daily = daily_estimate(&kb.conn, &cfg, &kb.data, now, cn())
            .unwrap()
            .unwrap();
        assert_eq!(
            daily, total.input_tokens,
            "both sessions were active on the same day"
        );
    }
}
