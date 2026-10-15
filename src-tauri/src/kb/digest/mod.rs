//! The daily digest: turning sessions into notes with the user's own
//! `claude -p` / `codex exec`.
//!
//! Layout: [`plan`] picks the sessions, [`input`] builds what is sent,
//! [`engine`] runs the CLI, [`validate`] checks the answer, [`store`] keeps
//! the run state. This file drives a run: rounds, retries, the quota pause,
//! and the lock that keeps two runs (automatic or manual) from overlapping.
//!
//! Rules worth knowing before reading the code (all from the design,
//! `docs/agent-kb/P4-design.md`):
//! * A round ends at a fixed time of day (the "cut-off"); only messages up to
//!   it are digested, newer ones belong to the next round.
//! * A session counts as digested only after its note file is written and
//!   indexed. A round is "complete" when none of its sessions is pending.
//! * Quota / login / missing-CLI errors pause the whole digest for an hour
//!   without costing the session an attempt; any other failure costs one of
//!   three attempts, ten minutes apart.
//! * The AI is never called while a database transaction is open.

pub mod commands;
pub mod engine;
pub mod input;
pub mod plan;
pub mod status;
pub mod store;
pub mod validate;

#[cfg(test)]
pub mod testutil;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use rusqlite::Connection;
use serde::Serialize;

use self::engine::{Call, Engine, Failure};
use self::store::{Done, Item, Round};
use super::config::{DigestConfig, KbConfig};
use super::db;
use super::lock::SyncLock;
use super::log::SyncLog;
use super::notes::{self, EngineInfo, NotesFile};
use super::paths::DataPaths;
use super::redact::redact;

/// Failures a session may have before it is left alone.
pub const MAX_ATTEMPTS: u32 = 3;
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
const PAUSE_AFTER: Duration = Duration::from_secs(60 * 60);
const ERROR_CHARS: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Started by `--kb-sync` when a round is due.
    Auto,
    /// A button in the app.
    Gui,
    /// `--kb-digest` typed by hand.
    Cli,
}

impl Trigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gui => "gui",
            Self::Cli => "cli",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The daily round, and anything unfinished.
    Auto,
    /// Every undigested session, however old ("整理全部未整理的").
    All,
    /// One session, now ("立即整理这个").
    Session { source: String, session_id: String },
}

type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct DigestEnv {
    pub data: DataPaths,
    pub config: DigestConfig,
    /// A clock, so tests can run "overnight" in a moment.
    pub now: Clock,
    pub tz: FixedOffset,
    pub cancel: Arc<AtomicBool>,
    pub call_timeout: Duration,
    pub retry_after: Duration,
    pub pause_after: Duration,
}

impl DigestEnv {
    pub fn from_config(config: &KbConfig) -> Self {
        Self {
            data: DataPaths::new(config.data_dir.clone()),
            config: config.digest.clone(),
            now: Arc::new(Utc::now),
            tz: *chrono::Local::now().offset(),
            cancel: Arc::new(AtomicBool::new(false)),
            call_timeout: engine::CALL_TIMEOUT,
            retry_after: RETRY_AFTER,
            pause_after: PAUSE_AFTER,
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.now)()
    }

    fn engine(&self) -> Result<Engine, String> {
        Engine::parse(&self.config.engine)
            .ok_or_else(|| format!("unknown digest engine {:?}", self.config.engine))
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct RunSummary {
    /// `ok`, `busy` (another run holds the lock), `disabled`, `paused`
    /// (quota/login/CLI problem, or still inside the pause), `cancelled`,
    /// `idle` (nothing to do) or `error`.
    pub status: String,
    pub rounds: Vec<String>,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Sessions whose attempt failed and will be tried again.
    pub retried: usize,
    pub remaining: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub calls: usize,
    pub pause_reason: Option<String>,
    pub message: Option<String>,
}

impl RunSummary {
    fn with_status(status: &str) -> Self {
        Self {
            status: status.into(),
            ..Self::default()
        }
    }
}

fn clip_error(text: &str) -> String {
    let clean = redact(text);
    let flat: String = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(ERROR_CHARS).collect()
}

// ---------------------------------------------------------------------------
// Rounds
// ---------------------------------------------------------------------------

fn new_round(
    env: &DigestEnv,
    id: &str,
    kind: &str,
    cutoff: DateTime<Utc>,
    trigger: Trigger,
) -> Round {
    Round {
        round_id: id.to_string(),
        kind: kind.to_string(),
        cutoff_at: plan::rfc3339(cutoff),
        planned_at: plan::rfc3339(env.now()),
        completed_at: None,
        state: "open".into(),
        trigger: trigger.as_str().into(),
        engine: env.config.engine.clone(),
        model: env.config.model.clone(),
    }
}

fn finish_if_complete(env: &DigestEnv, conn: &Connection, round_id: &str) -> Result<bool, String> {
    let counts = store::counts(conn, round_id)?;
    if counts.pending + counts.running > 0 {
        return Ok(false);
    }
    store::set_round_state(conn, round_id, "done", Some(&plan::rfc3339(env.now())))?;
    Ok(true)
}

/// Plan the automatic round of the latest cut-off, unless it already exists.
/// Returns the round to work on (new or still open), `None` when it is
/// already finished.
fn ensure_daily_round(env: &DigestEnv, conn: &Connection) -> Result<Option<Round>, String> {
    let time = env
        .config
        .time_of_day()
        .ok_or("the digest time is not HH:MM")?;
    let now = env.now();
    let (cutoff, date) = plan::latest_cutoff(now, time, env.tz);
    if let Some(existing) = store::get_round(conn, &date)? {
        return Ok((existing.state == "open").then_some(existing));
    }
    // The old open round stops; what it left undone is planned again below
    // because those sessions still have undigested messages.
    store::supersede_open(conn, "daily", &date)?;
    let round = new_round(env, &date, "daily", cutoff, Trigger::Auto);
    let rules = plan::Rules {
        cfg: &env.config,
        data: &env.data,
        cutoff,
        now,
        recent_only: true,
        quiet: true,
        honour_failures: true,
    };
    let mut found = plan::candidates(conn, &rules)?;
    found.truncate(env.config.max_sessions_per_round);
    store::insert_round(conn, &round)?;
    for candidate in &found {
        store::add_item(
            conn,
            &round.round_id,
            &candidate.info.source,
            &candidate.info.session_id,
        )?;
    }
    db::set_meta(conn, store::FIRST_ROUND_SEEN, "1")?;
    // Nothing to digest is still a finished round: the card says "done, 0".
    finish_if_complete(env, conn, &round.round_id)?;
    store::get_round(conn, &date)
}

fn manual_round_id(now: DateTime<Utc>) -> String {
    format!("manual-{}", now.format("%Y%m%dT%H%M%SZ"))
}

fn plan_manual(
    env: &DigestEnv,
    conn: &Connection,
    scope: &Scope,
    trigger: Trigger,
) -> Result<Option<Round>, String> {
    let now = env.now();
    let base = manual_round_id(now);
    let mut id = base.clone();
    let mut n = 1;
    while store::get_round(conn, &id)?.is_some() {
        n += 1;
        id = format!("{base}-{n}");
    }
    let round = new_round(env, &id, "manual", now, trigger);
    let found = if let Scope::Session { source, session_id } = scope {
        let info = input::session_info(conn, source, session_id)?
            .ok_or_else(|| format!("{source}:{session_id} is not in the index"))?;
        let digested = notes::digested_seq(conn, source, session_id)?.unwrap_or(0);
        let has_new =
            input::next_chunk(conn, &info, digested, now, None, input::CHUNK_CHARS)?.is_some();
        if !has_new {
            return Err("this session has nothing new to digest".to_string());
        }
        vec![(info.source, info.session_id)]
    } else {
        store::supersede_open(conn, "manual", &id)?;
        let rules = plan::Rules {
            cfg: &env.config,
            data: &env.data,
            cutoff: now,
            now,
            recent_only: false,
            quiet: true,
            honour_failures: true,
        };
        plan::candidates(conn, &rules)?
            .into_iter()
            .map(|c| (c.info.source, c.info.session_id))
            .collect()
    };
    store::insert_round(conn, &round)?;
    for (source, session_id) in &found {
        store::add_item(conn, &id, source, session_id)?;
    }
    finish_if_complete(env, conn, &id)?;
    store::get_round(conn, &id)
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

/// Run once: take the lock, plan if needed, digest what is due, return.
pub fn run(env: &DigestEnv, trigger: Trigger, scope: &Scope) -> Result<RunSummary, String> {
    if trigger == Trigger::Auto && !env.config.enabled {
        return Ok(RunSummary::with_status("disabled"));
    }
    let Some(_lock) = SyncLock::acquire(&env.data.digest_lock(), Duration::ZERO)? else {
        return Ok(RunSummary::with_status("busy"));
    };
    let log = SyncLog::open_digest(&env.data.logs());
    log.line(&format!(
        "digest start trigger={} scope={scope:?}",
        trigger.as_str()
    ));
    // The AI's working directory exists before anything can look at it.
    std::fs::create_dir_all(env.data.digest_work())
        .map_err(|e| format!("cannot create {}: {e}", env.data.digest_work().display()))?;
    let conn = db::open(&env.data.db())?;
    notes::reconcile(&conn, &env.data)?;
    recover_running(env, &conn, &log)?;

    let result = run_locked(env, &conn, trigger, scope, &log);
    match &result {
        Ok(summary) => log.line(&format!(
            "digest end status={} done={} failed={} remaining={} calls={} tokens={}/{}",
            summary.status,
            summary.done,
            summary.failed,
            summary.remaining,
            summary.calls,
            summary.input_tokens,
            summary.output_tokens
        )),
        Err(e) => log.line(&format!("digest error: {}", clip_error(e))),
    }
    store::prune_rounds(&conn)?;
    result
}

fn run_locked(
    env: &DigestEnv,
    conn: &Connection,
    trigger: Trigger,
    scope: &Scope,
    log: &SyncLog,
) -> Result<RunSummary, String> {
    let now = env.now();
    if trigger == Trigger::Auto {
        if let Some((until, reason, message)) = store::blocked(conn)? {
            if DateTime::parse_from_rfc3339(&until).is_ok_and(|t| t.with_timezone(&Utc) > now) {
                return Ok(RunSummary {
                    status: "paused".into(),
                    pause_reason: Some(reason),
                    message: Some(message),
                    ..RunSummary::default()
                });
            }
        }
    }

    let mut rounds: Vec<Round> = Vec::new();
    match scope {
        Scope::Auto => {
            if let Some(round) = ensure_daily_round(env, conn)? {
                rounds.push(round);
            }
            // Unfinished manual rounds (quota ran out mid-way) go on too.
            for open in store::open_rounds(conn)? {
                if open.kind == "manual" && !rounds.iter().any(|r| r.round_id == open.round_id) {
                    rounds.push(open);
                }
            }
        }
        _ => {
            rounds.extend(plan_manual(env, conn, scope, trigger)?);
        }
    }
    if rounds.is_empty() {
        return Ok(RunSummary::with_status("idle"));
    }

    let mut summary = RunSummary::with_status("ok");
    let mut ctx = Context::new(env)?;
    let respect_quiet = !matches!(scope, Scope::Session { .. });
    'rounds: for round in rounds {
        summary.rounds.push(round.round_id.clone());
        loop {
            if env.cancelled() {
                // The user stopped it: what is left is planned again later.
                store::set_round_state(conn, &round.round_id, "superseded", None)?;
                summary.status = "cancelled".into();
                break 'rounds;
            }
            let Some(item) =
                store::next_due_item(conn, &round.round_id, &plan::rfc3339(env.now()))?
            else {
                break;
            };
            match process_item(env, conn, &round, &item, &mut ctx, respect_quiet, log)? {
                Outcome::Done(stats) => {
                    summary.done += 1;
                    summary.calls += stats.calls;
                    summary.input_tokens += stats.input_tokens;
                    summary.output_tokens += stats.output_tokens;
                }
                Outcome::Skipped => summary.skipped += 1,
                Outcome::Retry => summary.retried += 1,
                Outcome::Failed => summary.failed += 1,
                Outcome::Pause(failure) => {
                    summary.status = "paused".into();
                    summary.pause_reason = Some(failure.kind().to_string());
                    summary.message = Some(clip_error(&failure.message()));
                    break 'rounds;
                }
                Outcome::Cancelled => {
                    store::set_round_state(conn, &round.round_id, "superseded", None)?;
                    summary.status = "cancelled".into();
                    break 'rounds;
                }
            }
        }
        finish_if_complete(env, conn, &round.round_id)?;
    }
    for id in &summary.rounds {
        summary.remaining += store::counts(conn, id).map_or(0, |c| c.pending + c.running);
    }
    if summary.status == "ok"
        && summary.done + summary.failed + summary.skipped + summary.retried == 0
    {
        summary.status = "idle".into();
    }
    Ok(summary)
}

/// Items left `running` by a process that died count as one failed attempt
/// (the attempt was counted when the call began).
fn recover_running(env: &DigestEnv, conn: &Connection, log: &SyncLog) -> Result<(), String> {
    for item in store::running_items(conn)? {
        log.line(&format!(
            "recovering an interrupted item {}:{}",
            item.source, item.session_id
        ));
        let error = "the digest was interrupted";
        if item.attempts >= MAX_ATTEMPTS {
            let newest = newest_seq(conn, &item.source, &item.session_id)?;
            store::mark_failed(conn, &item, error, newest, &plan::rfc3339(env.now()))?;
        } else {
            store::mark_pending(conn, &item, false, None, Some(error))?;
        }
        finish_if_complete(env, conn, &item.round_id)?;
    }
    Ok(())
}

fn newest_seq(conn: &Connection, source: &str, session_id: &str) -> Result<u64, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(m.seq),0) FROM messages m JOIN sessions s ON s.file_id=m.file_id \
         WHERE s.source=?1 AND s.session_id=?2",
        [source, session_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| u64::try_from(n).unwrap_or(0))
    .map_err(|e| format!("read newest message: {e}"))
}

/// What a run learns once and reuses.
struct Context {
    engine: Engine,
    cli: Result<PathBuf, Failure>,
    version: Option<String>,
}

impl Context {
    fn new(env: &DigestEnv) -> Result<Self, String> {
        let engine = env.engine()?;
        let cli = engine::resolve_cli(engine, env.config.cli_path.as_deref());
        Ok(Self {
            engine,
            cli,
            version: None,
        })
    }

    fn version(&mut self, cli: &std::path::Path) -> String {
        self.version
            .get_or_insert_with(|| engine::cli_version(cli))
            .clone()
    }
}

struct Stats {
    calls: usize,
    input_tokens: u64,
    output_tokens: u64,
}

enum Outcome {
    Done(Stats),
    Skipped,
    /// Back to pending for a later retry.
    Retry,
    Failed,
    /// Quota, login or CLI problem: stop everything for a while.
    Pause(Failure),
    Cancelled,
}

fn process_item(
    env: &DigestEnv,
    conn: &Connection,
    round: &Round,
    item: &Item,
    ctx: &mut Context,
    respect_quiet: bool,
    log: &SyncLog,
) -> Result<Outcome, String> {
    let now = env.now();
    let now_text = plan::rfc3339(now);
    let Some(info) = input::session_info(conn, &item.source, &item.session_id)? else {
        store::mark_skipped(
            conn,
            item,
            "the session is no longer in the index",
            &now_text,
        )?;
        return Ok(Outcome::Skipped);
    };
    // A session that became active after planning waits for the next round.
    if respect_quiet && is_active(conn, env, &info, now)? {
        log.line(&format!(
            "skip {}:{} (still active)",
            item.source, item.session_id
        ));
        store::mark_skipped(conn, item, "still active", &now_text)?;
        return Ok(Outcome::Skipped);
    }
    let cli = match &ctx.cli {
        Ok(path) => path.clone(),
        Err(failure) => {
            pause(env, conn, failure)?;
            return Ok(Outcome::Pause(failure.clone()));
        }
    };

    store::mark_running(conn, item)?;
    let cutoff = DateTime::parse_from_rfc3339(&round.cutoff_at)
        .map_err(|e| format!("bad cutoff: {e}"))?
        .with_timezone(&Utc);
    match digest_session(env, conn, round, &info, ctx, &cli, cutoff, log) {
        Ok(result) => {
            store::mark_done(
                conn,
                item,
                &Done {
                    seq_from: result.seq_from,
                    seq_to: result.seq_to,
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    duration_ms: result.duration_ms,
                    messages: result.messages,
                    covered: result.covered,
                },
                &plan::rfc3339(env.now()),
            )?;
            store::clear_blocked(conn)?;
            log.line(&format!(
                "digested {}:{} seq {}-{} calls={} tokens={}/{} turns={} list_cost_usd={:.3} took={}s coverage={}/{}",
                item.source,
                item.session_id,
                result.seq_from,
                result.seq_to,
                result.calls,
                result.input_tokens,
                result.output_tokens,
                result.turns,
                result.cost_usd,
                result.duration_ms / 1000,
                result.covered,
                result.messages
            ));
            Ok(Outcome::Done(Stats {
                calls: result.calls,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
            }))
        }
        Err(failure) => {
            log.line(&format!(
                "{} {}:{}: {}",
                failure.kind(),
                item.source,
                item.session_id,
                clip_error(&failure.message())
            ));
            match failure {
                Failure::Cancelled => {
                    store::mark_pending(conn, item, true, None, None)?;
                    Ok(Outcome::Cancelled)
                }
                Failure::Quota(_) | Failure::Auth(_) | Failure::Config(_) => {
                    store::mark_pending(conn, item, true, None, None)?;
                    pause(env, conn, &failure)?;
                    Ok(Outcome::Pause(failure))
                }
                other => {
                    let error = clip_error(&other.message());
                    let attempts = item.attempts + 1;
                    if attempts >= MAX_ATTEMPTS {
                        let newest = newest_seq(conn, &item.source, &item.session_id)?;
                        store::mark_failed(conn, item, &error, newest, &plan::rfc3339(env.now()))?;
                        Ok(Outcome::Failed)
                    } else {
                        let retry_at = env.now()
                            + chrono::Duration::from_std(env.retry_after).unwrap_or_default();
                        store::mark_pending(
                            conn,
                            item,
                            false,
                            Some(&plan::rfc3339(retry_at)),
                            Some(&error),
                        )?;
                        Ok(Outcome::Retry)
                    }
                }
            }
        }
    }
}

fn pause(env: &DigestEnv, conn: &Connection, failure: &Failure) -> Result<(), String> {
    let until = env.now() + chrono::Duration::from_std(env.pause_after).unwrap_or_default();
    store::set_blocked(
        conn,
        &plan::rfc3339(until),
        failure.kind(),
        &clip_error(&failure.message()),
    )
}

fn is_active(
    conn: &Connection,
    env: &DigestEnv,
    info: &input::SessionInfo,
    now: DateTime<Utc>,
) -> Result<bool, String> {
    let latest: Option<String> = conn
        .query_row(
            "SELECT MAX(x) FROM (SELECT last_activity_at AS x FROM sessions WHERE file_id=?1 \
             UNION ALL SELECT last_activity_at FROM sessions WHERE source=?2 \
               AND parent_session_id=?3 AND is_subagent=1)",
            rusqlite::params![info.file_id, info.source, info.session_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("read activity: {e}"))?;
    let quiet = chrono::Duration::minutes(i64::from(env.config.quiet_minutes));
    Ok(latest
        .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
        .is_some_and(|t| now - t.with_timezone(&Utc) < quiet))
}

struct Digested {
    seq_from: u64,
    seq_to: u64,
    calls: usize,
    input_tokens: u64,
    output_tokens: u64,
    duration_ms: u64,
    cost_usd: f64,
    turns: u64,
    /// User/assistant messages sent, and how many the notes point at.
    messages: u64,
    covered: u64,
}

/// How many user/assistant messages of one piece the notes point at, and how
/// many there were (a message is pointed at when it lies inside the range of
/// any entry). Only recorded, never acted on: the first weeks of real use
/// show whether low coverage is common.
fn coverage(
    conn: &Connection,
    file_id: i64,
    file: &NotesFile,
    from_seq: u64,
    to_seq: u64,
) -> (u64, u64) {
    let Ok(mut stmt) = conn.prepare(
        "SELECT seq FROM messages WHERE file_id=?1 AND seq BETWEEN ?2 AND ?3 \
         AND kind IN ('user','assistant')",
    ) else {
        return (0, 0);
    };
    let Ok(rows) = stmt.query_map(
        rusqlite::params![
            file_id,
            i64::try_from(from_seq).unwrap_or(i64::MAX),
            i64::try_from(to_seq).unwrap_or(i64::MAX)
        ],
        |r| r.get::<_, i64>(0),
    ) else {
        return (0, 0);
    };
    let (mut total, mut covered) = (0, 0);
    for seq in rows.filter_map(Result::ok) {
        let seq = u64::try_from(seq).unwrap_or(0);
        total += 1;
        if file
            .entries
            .iter()
            .any(|e| e.evidence.seq_from <= seq && seq <= e.evidence.seq_to)
        {
            covered += 1;
        }
    }
    (total, covered)
}

/// Up to `max_chunks_per_session` calls for one session. Each call's result
/// is on disk (and indexed) before the next call is made, so a failure in
/// the middle keeps the progress.
#[allow(clippy::too_many_arguments)]
fn digest_session(
    env: &DigestEnv,
    conn: &Connection,
    round: &Round,
    info: &input::SessionInfo,
    ctx: &mut Context,
    cli: &std::path::Path,
    cutoff: DateTime<Utc>,
    log: &SyncLog,
) -> Result<Digested, Failure> {
    let other = |e: String| Failure::Other(e);
    let mut file = notes::read(&env.data, &info.source, &info.session_id)
        .map_err(other)?
        .unwrap_or_else(|| NotesFile::empty(&info.source, &info.session_id));
    let mut result = Digested {
        seq_from: file.digested_through_seq + 1,
        seq_to: file.digested_through_seq,
        calls: 0,
        input_tokens: 0,
        output_tokens: 0,
        duration_ms: 0,
        cost_usd: 0.0,
        turns: 0,
        messages: 0,
        covered: 0,
    };
    let version = ctx.version(cli);
    for _ in 0..env.config.max_chunks_per_session {
        if env.cancelled() {
            return Err(Failure::Cancelled);
        }
        let Some(chunk) = input::next_chunk(
            conn,
            info,
            file.digested_through_seq,
            cutoff,
            Some(&file),
            input::CHUNK_CHARS,
        )
        .map_err(other)?
        else {
            break;
        };
        let reply = engine::run(&Call {
            engine: ctx.engine,
            cli,
            model: &env.config.model,
            reasoning: &env.config.codex_reasoning,
            work_dir: &env.data.digest_work(),
            system_prompt: engine::SYSTEM_PROMPT,
            input: &chunk.text,
            timeout: env.call_timeout,
            cancel: &env.cancel,
        })?;
        result.calls += 1;
        result.cost_usd += reply.cost_usd.unwrap_or(0.0);
        result.turns += reply.turns.unwrap_or(0);
        result.input_tokens += reply.input_tokens;
        result.output_tokens += reply.output_tokens;
        result.duration_ms += reply.duration_ms;

        let seq_min = file
            .entries
            .iter()
            .map(|e| e.evidence.seq_from)
            .min()
            .map_or(chunk.from_seq, |m| m.min(chunk.from_seq));
        let ids: Vec<&str> = file.entries.iter().map(|e| e.id.as_str()).collect();
        let checked = validate::check(
            &reply.answer,
            &validate::Bounds {
                seq_min,
                seq_max: chunk.to_seq,
                previous_ids: ids,
            },
        )
        .map_err(|e| Failure::Other(format!("invalid answer: {e}")))?;

        let at = plan::rfc3339(env.now());
        let file_id = info.file_id;
        let lookup = |seq: u64| -> Option<validate::Anchor> {
            conn.query_row(
                "SELECT msg_key, ts, kind FROM messages WHERE file_id=?1 AND seq>=?2 \
                 ORDER BY seq LIMIT 1",
                rusqlite::params![file_id, i64::try_from(seq).unwrap_or(i64::MAX)],
                |r| {
                    Ok(validate::Anchor {
                        msg_key: r.get(0)?,
                        at: r.get(1)?,
                        kind: r.get(2)?,
                    })
                },
            )
            .ok()
        };
        validate::merge(
            &mut file,
            checked,
            &validate::Authorship {
                round: &round.round_id,
                at: &at,
                engine: EngineInfo {
                    cli: ctx.engine.as_str().to_string(),
                    cli_version: version.clone(),
                    model: reply
                        .model
                        .clone()
                        .or_else(|| {
                            (!env.config.model.is_empty()).then(|| env.config.model.clone())
                        })
                        .unwrap_or_else(|| "default".to_string()),
                },
                anchor_of: &lookup,
            },
        );
        file.session_title = info.title.as_deref().map(|t| redact(t).into_owned());
        file.cwd.clone_from(&info.cwd);
        file.digested_through_seq = chunk.to_seq;
        file.digested_at.clone_from(&at);
        file.status = if chunk.more {
            "partial"
        } else if file.entries.is_empty() {
            "nothing"
        } else {
            "notes"
        }
        .into();
        file.history.push(notes::HistoryItem {
            round: round.round_id.clone(),
            at,
            seq_from: chunk.from_seq,
            seq_to: chunk.to_seq,
            input_tokens: reply.input_tokens,
            output_tokens: reply.output_tokens,
        });
        // File first, then the index: a crash in between is repaired by the
        // hash check at the start of the next run.
        let hash = notes::write(&env.data, &file).map_err(other)?;
        notes::index_file(conn, &file, &hash).map_err(other)?;
        let (messages, covered) = coverage(conn, info.file_id, &file, chunk.from_seq, chunk.to_seq);
        log.line(&format!(
            "piece {}:{} seq {}-{} coverage={}/{} tokens={}/{}",
            info.source,
            info.session_id,
            chunk.from_seq,
            chunk.to_seq,
            covered,
            messages,
            reply.input_tokens,
            reply.output_tokens
        ));
        result.messages += messages;
        result.covered += covered;
        result.seq_to = chunk.to_seq;
        if !chunk.more {
            break;
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Deciding whether a run is worth starting (called after every `--kb-sync`)
// ---------------------------------------------------------------------------

/// Whether a digest process is running now (its pid, from the lock file).
pub fn running_pid(data: &DataPaths) -> Option<u32> {
    super::lock::held_by_live_process(&data.digest_lock()).map(|i| i.pid)
}

/// Ask a running digest to stop (the app's "stop" button): SIGTERM to the
/// pid in the lock file. True when a signal was sent.
#[allow(unsafe_code)] // `kill(2)` on the pid the lock file names
pub fn request_stop(data: &DataPaths) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = running_pid(data).and_then(|p| i32::try_from(p).ok()) else {
            return false;
        };
        // SAFETY: plain syscall; the pid belongs to a live process that holds
        // our lock (checked just above).
        unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = data;
        false
    }
}

/// True when `--kb-digest --auto` has something to do: digest on, not paused,
/// nobody running, and either the latest round was never planned or an open
/// round has an item whose time has come.
pub fn due_check(config: &KbConfig, now: DateTime<Utc>, tz: FixedOffset) -> Result<bool, String> {
    let cfg = &config.digest;
    if !cfg.enabled {
        return Ok(false);
    }
    let data = DataPaths::new(config.data_dir.clone());
    if !data.db().is_file() {
        return Ok(false);
    }
    let conn = db::open(&data.db())?;
    if let Some((until, _, _)) = store::blocked(&conn)? {
        if DateTime::parse_from_rfc3339(&until).is_ok_and(|t| t.with_timezone(&Utc) > now) {
            return Ok(false);
        }
    }
    // A live holder means a run is already working. (Asking, not locking:
    // taking the lock even briefly could make a run starting right now find it busy.)
    if super::lock::held_by_live_process(&data.digest_lock()).is_some() {
        return Ok(false);
    }
    if store::has_due_item(&conn, &plan::rfc3339(now))? {
        return Ok(true);
    }
    let time = cfg.time_of_day().ok_or("the digest time is not HH:MM")?;
    let (_, round_id) = plan::latest_cutoff(now, time, tz);
    Ok(store::get_round(&conn, &round_id)?.is_none())
}

static TERMINATED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_sigterm(_signal: libc::c_int) {
    TERMINATED.store(true, Ordering::SeqCst);
}

/// Make SIGTERM (the app's "stop" button) set `cancel`: the run kills the AI
/// process, puts the session back as pending, and leaves cleanly.
#[allow(unsafe_code)] // installing a signal handler that only stores to an atomic
pub fn stop_on_sigterm(cancel: &Arc<AtomicBool>) {
    #[cfg(unix)]
    {
        // SAFETY: the handler is async-signal-safe (one atomic store).
        unsafe {
            libc::signal(
                libc::SIGTERM,
                on_sigterm as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
        let cancel = Arc::clone(cancel);
        std::thread::spawn(move || loop {
            if TERMINATED.load(Ordering::SeqCst) {
                cancel.store(true, Ordering::SeqCst);
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        });
    }
    #[cfg(not(unix))]
    let _ = cancel;
}

/// Start `--kb-digest --auto` as a separate, detached process and return at
/// once, so the collection that called this never waits for the AI.
#[allow(unsafe_code)] // `setsid(2)` between fork and exec
pub fn spawn_background(
    exe: &std::path::Path,
    data: &DataPaths,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<(), String> {
    let logs = data.logs();
    std::fs::create_dir_all(&logs).map_err(|e| format!("cannot create {}: {e}", logs.display()))?;
    let log_path = logs.join(format!(
        "digest-{}.log",
        chrono::Local::now().format("%Y-%m-%d")
    ));
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("cannot open {}: {e}", log_path.display()))
    };
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .stdout(open()?)
        .stderr(open()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: only async-signal-safe `setsid` runs between fork and exec.
        // A new session keeps launchd (which reaps the sync job's process
        // group when the job exits) from taking the digest down with it.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    command
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot start the digest: {e}"))
}

#[cfg(test)]
mod tests;
