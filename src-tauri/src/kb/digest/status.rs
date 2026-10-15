//! What the home page and the settings page show about the digest, and the
//! "test" button.

use chrono::{DateTime, FixedOffset, Utc};
use serde::Serialize;

use super::engine::{self, Call, Engine};
use super::store::{self, Counts};
use super::{plan, validate, DigestEnv};
use crate::kb::config::KbConfig;
use crate::kb::db;
use crate::kb::lock;
use crate::kb::paths::DataPaths;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RoundReport {
    pub round_id: String,
    pub kind: String,
    pub state: String,
    pub planned_at: String,
    pub completed_at: Option<String>,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub pending: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
}

fn report_of(round: &store::Round, counts: &Counts) -> RoundReport {
    RoundReport {
        round_id: round.round_id.clone(),
        kind: round.kind.clone(),
        state: round.state.clone(),
        planned_at: round.planned_at.clone(),
        completed_at: round.completed_at.clone(),
        done: counts.done,
        failed: counts.failed,
        skipped: counts.skipped,
        pending: counts.pending + counts.running,
        input_tokens: counts.input_tokens,
        output_tokens: counts.output_tokens,
        duration_ms: counts.duration_ms,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Paused {
    /// `quota`, `auth` or `config`.
    pub reason: String,
    pub until: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FailureReport {
    pub source: String,
    pub session_id: String,
    pub title: Option<String>,
    pub attempts: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Running {
    pub pid: u32,
    pub started_at: String,
    pub round_id: Option<String>,
    pub total: usize,
    pub finished: usize,
    pub current_title: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub enabled: bool,
    pub engine: String,
    pub model: String,
    pub time: String,
    pub running: Option<Running>,
    pub paused: Option<Paused>,
    pub next_run_at: Option<String>,
    pub last_round: Option<RoundReport>,
    pub failures: Vec<FailureReport>,
    /// Sessions the next automatic round would digest.
    pub pending_auto: usize,
    /// Undigested sessions outside the automatic window (manual only).
    pub pending_older: usize,
    /// Set until the first round has been planned: what it will cost.
    pub first_round_tokens: Option<u64>,
}

/// Everything on the home page's digest card. Read only: never migrates and
/// never takes the digest lock.
pub fn status(
    config: &KbConfig,
    now: DateTime<Utc>,
    tz: FixedOffset,
) -> Result<StatusReport, String> {
    let cfg = &config.digest;
    let data = DataPaths::new(config.data_dir.clone());
    let mut report = StatusReport {
        enabled: cfg.enabled,
        engine: cfg.engine.clone(),
        model: cfg.model.clone(),
        time: cfg.time.clone(),
        running: None,
        paused: None,
        next_run_at: cfg
            .time_of_day()
            .map(|t| plan::rfc3339(plan::next_cutoff(now, t, tz))),
        last_round: None,
        failures: Vec::new(),
        pending_auto: 0,
        pending_older: 0,
        first_round_tokens: None,
    };
    let Some(conn) = db::open_readonly(&data.db())? else {
        return Ok(report);
    };
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| format!("user_version: {e}"))?;
    if version < 4 {
        return Ok(report);
    }

    if let Some(info) = lock::held_by_live_process(&data.digest_lock()) {
        let open = store::open_rounds(&conn)?;
        let (round_id, counts) = match open.last() {
            Some(round) => (
                Some(round.round_id.clone()),
                store::counts(&conn, &round.round_id)?,
            ),
            None => (None, Counts::default()),
        };
        let current_title = match &round_id {
            Some(id) => store::items(&conn, id)?
                .into_iter()
                .find(|i| i.state == "running")
                .and_then(|i| {
                    conn.query_row(
                        "SELECT title FROM sessions WHERE source=?1 AND session_id=?2",
                        [i.source, i.session_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten()
                }),
            None => None,
        };
        report.running = Some(Running {
            pid: info.pid,
            started_at: info.started_at,
            round_id,
            total: counts.total(),
            finished: counts.done + counts.failed + counts.skipped,
            current_title,
        });
    }
    if let Some((until, reason, message)) = store::blocked(&conn)? {
        if DateTime::parse_from_rfc3339(&until).is_ok_and(|t| t.with_timezone(&Utc) > now) {
            report.paused = Some(Paused {
                reason,
                until,
                message,
            });
        }
    }
    if let Some(round) = store::last_done_round(&conn)? {
        report.last_round = Some(report_of(&round, &store::counts(&conn, &round.round_id)?));
    }
    for item in store::failures(&conn)? {
        let title = conn
            .query_row(
                "SELECT title FROM sessions WHERE source=?1 AND session_id=?2",
                [&item.source, &item.session_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        report.failures.push(FailureReport {
            source: item.source,
            session_id: item.session_id,
            title,
            attempts: item.attempts,
            error: item.last_error,
        });
    }
    let rules = |recent_only: bool| plan::Rules {
        cfg,
        data: &data,
        cutoff: now,
        now,
        recent_only,
        quiet: false,
        honour_failures: true,
    };
    let auto = plan::candidates(&conn, &rules(true))?;
    let all = plan::candidates(&conn, &rules(false))?;
    report.pending_auto = auto.len();
    report.pending_older = all.len().saturating_sub(auto.len());
    if cfg.enabled && !store::any_round(&conn)? {
        let capped = &auto[..auto.len().min(cfg.max_sessions_per_round)];
        report.first_round_tokens = Some(plan::estimate(&conn, cfg, capped, now)?.input_tokens);
    }
    Ok(report)
}

/// The confirmation dialog of "整理全部未整理的".
pub fn estimate_all(config: &KbConfig, now: DateTime<Utc>) -> Result<plan::Estimate, String> {
    let data = DataPaths::new(config.data_dir.clone());
    let Some(conn) = db::open_readonly(&data.db())? else {
        return Ok(plan::Estimate::default());
    };
    let rules = plan::Rules {
        cfg: &config.digest,
        data: &data,
        cutoff: now,
        now,
        recent_only: false,
        quiet: true,
        honour_failures: true,
    };
    let found = plan::candidates(&conn, &rules)?;
    plan::estimate(&conn, &config.digest, &found, now)
}

/// The settings page's "about X tokens a day" line.
pub fn daily_tokens(
    config: &KbConfig,
    now: DateTime<Utc>,
    tz: FixedOffset,
) -> Result<Option<u64>, String> {
    let data = DataPaths::new(config.data_dir.clone());
    let Some(conn) = db::open_readonly(&data.db())? else {
        return Ok(None);
    };
    plan::daily_estimate(&conn, &config.digest, &data, now, tz)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TestReport {
    pub ok: bool,
    /// `quota`, `auth`, `config`, `timeout`, `failed` or `invalid_answer` when not ok.
    pub kind: Option<String>,
    pub message: Option<String>,
    pub cli: Option<String>,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: Option<String>,
}

const TEST_INPUT: &str = "<session>\nsource: claude\ntitle: connection test\nproject: -\ncwd: -\nactive: ? .. ?\n</session>\n<messages from=#1 to=#2>\n[#1 user 01-01T00:00] Hello, this is only a connection test.\n[#2 assistant 01-01T00:00] Hello! The connection works.\n</messages>\n";

/// One real, tiny call with the same isolation as a real digest. Costs a few
/// thousand tokens. The settings page uses it to show that the CLI is found,
/// logged in and answers in the expected shape.
pub fn test_call(env: &DigestEnv) -> TestReport {
    let fail = |kind: &str, message: String, cli: Option<String>| TestReport {
        ok: false,
        kind: Some(kind.into()),
        message: Some(message),
        cli,
        duration_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
        model: None,
    };
    let Some(kind) = Engine::parse(&env.config.engine) else {
        return fail("config", "unknown AI".into(), None);
    };
    let cli = match engine::resolve_cli(kind, env.config.cli_path.as_deref()) {
        Ok(cli) => cli,
        Err(f) => return fail(f.kind(), f.message(), None),
    };
    let shown = Some(cli.display().to_string());
    let work = env.data.digest_work();
    match engine::run(&Call {
        engine: kind,
        cli: &cli,
        model: &env.config.model,
        reasoning: &env.config.codex_reasoning,
        work_dir: &work,
        system_prompt: engine::SYSTEM_PROMPT,
        input: TEST_INPUT,
        timeout: env.call_timeout,
        cancel: &env.cancel,
    }) {
        Err(f) => fail(f.kind(), f.message(), shown),
        Ok(reply) => {
            let valid = validate::check(
                &reply.answer,
                &validate::Bounds {
                    seq_min: 1,
                    seq_max: 2,
                    previous_ids: Vec::new(),
                },
            );
            TestReport {
                ok: valid.is_ok(),
                kind: valid.is_err().then(|| "invalid_answer".to_string()),
                message: valid.err(),
                cli: shown,
                duration_ms: reply.duration_ms,
                input_tokens: reply.input_tokens,
                output_tokens: reply.output_tokens,
                model: reply.model,
            }
        }
    }
}
