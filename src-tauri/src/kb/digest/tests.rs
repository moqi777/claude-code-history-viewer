//! Whole-run tests with the fake AI command (`tests/fake_ai/fake-ai.sh`) and
//! a clock the test moves by hand. Nothing here calls a real AI.
//!
//! `FAKE_AI_DIR` is process-global, like `HOME` in the settings tests, so
//! these run single-threaded (`cargo test -- --test-threads=1`).

use rusqlite::params;
use std::sync::Mutex;

use super::testutil::{FakeAi, TestKb};
use super::*;

/// 03:00 in UTC+8 is 19:00 UTC; the round named 2026-10-16 ends there.
const NIGHT: &str = "2026-10-15T19:00:00Z";

fn utc(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

struct Rig {
    kb: TestKb,
    ai: FakeAi,
    clock: Arc<Mutex<DateTime<Utc>>>,
    cfg: DigestConfig,
}

impl Rig {
    fn new() -> Self {
        let kb = TestKb::new();
        // `KbConfig::default()` wants a sandboxed home directory.
        std::env::set_var("CCHV_TEST_HOME", kb.dir.path());
        let ai = FakeAi::new(kb.dir.path());
        let cfg = DigestConfig {
            enabled: true,
            cli_path: Some(ai.script.clone()),
            ..DigestConfig::default()
        };
        Self {
            kb,
            ai,
            clock: Arc::new(Mutex::new(utc("2026-10-16T00:00:00Z"))),
            cfg,
        }
    }

    fn at(&self, time: &str) {
        *self.clock.lock().unwrap() = utc(time);
    }

    fn env(&self) -> DigestEnv {
        let clock = Arc::clone(&self.clock);
        DigestEnv {
            data: self.kb.data.clone(),
            config: self.cfg.clone(),
            now: Arc::new(move || *clock.lock().unwrap()),
            tz: FixedOffset::east_opt(8 * 3600).unwrap(),
            cancel: Arc::new(AtomicBool::new(false)),
            call_timeout: Duration::from_secs(20),
            retry_after: Duration::from_secs(600),
            pause_after: Duration::from_secs(3600),
        }
    }

    fn run(&self) -> RunSummary {
        run(&self.env(), Trigger::Auto, &Scope::Auto).unwrap()
    }

    /// A finished conversation: `messages` messages ending at `last`.
    fn session(&self, id: &str, messages: usize, last: &str) -> i64 {
        let file = self.kb.session("claude", id, "/work/app", None, last);
        let start = utc(last) - chrono::Duration::minutes(i64::try_from(messages).unwrap());
        self.kb.chat(file, messages, &start.to_rfc3339());
        file
    }

    fn item_states(&self, round: &str) -> Vec<(String, String, u32)> {
        store::items(&self.kb.conn, round)
            .unwrap()
            .into_iter()
            .map(|i| (i.session_id, i.state, i.attempts))
            .collect()
    }

    fn note(&self, id: &str) -> Option<NotesFile> {
        notes::read(&self.kb.data, "claude", id).unwrap()
    }
}

#[test]
fn oversized_answer_is_stored_as_eight_notes_with_a_discard_log() {
    let rig = Rig::new();
    rig.session("s", 6, "2026-10-15T14:00:00Z");
    let entries: Vec<_> = (0..9).map(|i| serde_json::json!({
        "id": null, "title": format!("topic {i}"), "problem": "p", "cause": "c",
        "solution": "s", "conclusion": "answer", "keywords": ["k"], "seq_from": 1, "seq_to": 6,
    })).collect();
    std::fs::write(
        rig.ai.dir.join("answer.json"),
        serde_json::json!({"entries": entries}).to_string(),
    )
    .unwrap();
    rig.ai.mode("custom");
    let summary = rig.run();
    assert_eq!((summary.done, summary.failed, summary.retried), (1, 0, 0));
    assert_eq!(rig.ai.calls(), 1);
    let notes = rig.note("s").unwrap();
    assert_eq!(notes.entries.len(), 8);
    for (i, note) in notes.entries.iter().enumerate() {
        assert_eq!(note.title, format!("topic {i}"));
        assert_eq!(note.solution, "s");
    }
    let count: i64 = rig
        .kb
        .conn
        .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 8);
    let log: String = std::fs::read_dir(rig.kb.data.logs())
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(log.matches("discarded 1 entries beyond 8").count(), 1);
    assert!(log.contains("ai reply claude:s tokens=115/20"));
}

#[test]
fn one_attempt_scope_never_resumes_failures_or_other_pending_sessions() {
    for mode in ["ok", "fail", "quota", "auth"] {
        let rig = Rig::new();
        rig.session("selected", 6, "2026-10-15T14:00:00Z");
        rig.session("unselected", 6, "2026-10-15T14:00:00Z");
        rig.ai.mode(mode);
        let summary = run(
            &rig.env(),
            Trigger::Cli,
            &Scope::SessionOnce {
                source: "claude".into(),
                session_id: "selected".into(),
            },
        )
        .unwrap();
        assert_eq!(rig.ai.calls(), 1, "{mode}");
        assert_eq!(summary.remaining, 0);
        assert_eq!(summary.retried, 0);
        assert_eq!(summary.done, usize::from(mode == "ok"));
        assert_eq!(summary.failed, usize::from(mode != "ok"));
        let items = store::items(&rig.kb.conn, &summary.rounds[0]).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].session_id, "selected");
        assert_eq!(items[0].attempts, 1);
        assert!(!store::has_due_item(&rig.kb.conn, "2099-01-01T00:00:00Z").unwrap());
        assert!(rig.note("unselected").is_none());
    }
}

#[test]
fn interrupted_one_attempt_round_is_closed_without_another_call() {
    let rig = Rig::new();
    rig.at("2026-10-16T00:00:00.123456789Z");
    rig.session("selected", 6, "2026-10-15T14:00:00Z");
    let env = rig.env();
    let round = plan_manual(
        &env,
        &rig.kb.conn,
        &Scope::SessionOnce {
            source: "claude".into(),
            session_id: "selected".into(),
        },
        Trigger::Cli,
    )
    .unwrap()
    .unwrap();
    assert_eq!(round.state, "superseded");
    assert_eq!(round.cutoff_at, "2026-10-16T00:00:00.123456789Z");
    assert!(!store::has_due_item(&rig.kb.conn, "2099-01-01T00:00:00Z").unwrap());
    let item = store::items(&rig.kb.conn, &round.round_id)
        .unwrap()
        .remove(0);
    store::mark_running(&rig.kb.conn, &item).unwrap();
    recover_running(
        &env,
        &rig.kb.conn,
        &SyncLog::open_digest(&rig.kb.data.logs()),
    )
    .unwrap();
    let item = store::items(&rig.kb.conn, &round.round_id)
        .unwrap()
        .remove(0);
    assert_eq!(item.state, "failed");
    assert_eq!(item.attempts, 1);
    assert_eq!(rig.ai.calls(), 0);
}

#[test]
fn a_night_off_is_made_up_at_the_first_check_and_the_round_completes() {
    let rig = Rig::new();
    // Chats from the evening before; the machine was off at 03:00.
    for id in ["a", "b", "c"] {
        rig.session(id, 6, "2026-10-15T14:00:00Z");
    }
    rig.at("2026-10-16T00:30:00Z"); // 08:30 local, first check after boot
    assert!(due_check(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap());

    let summary = rig.run();
    assert_eq!(
        (summary.status.as_str(), summary.done, summary.remaining),
        ("ok", 3, 0)
    );
    assert_eq!(rig.ai.calls(), 3);
    let round = store::get_round(&rig.kb.conn, "2026-10-16")
        .unwrap()
        .unwrap();
    assert_eq!(round.state, "done");
    assert!(round.completed_at.is_some());
    assert_eq!(round.cutoff_at, NIGHT);
    for id in ["a", "b", "c"] {
        let note = rig.note(id).unwrap();
        assert_eq!(note.status, "notes");
        assert_eq!(note.digested_through_seq, 6);
        assert_eq!(note.entries[0].round, "2026-10-16");
    }
    // The database copy matches, and the next check has nothing to do.
    assert_eq!(
        notes::digested_seq(&rig.kb.conn, "claude", "a").unwrap(),
        Some(6)
    );
    assert!(!due_check(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap());
    assert_eq!(rig.run().status, "idle");
    assert_eq!(rig.ai.calls(), 3, "nothing is digested twice");

    // The coverage of every item is recorded, for judging the digest later.
    let (messages, covered): (i64, i64) = rig
        .kb
        .conn
        .query_row(
            "SELECT SUM(messages), SUM(covered) FROM digest_items WHERE state='done'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        messages, 18,
        "six user/assistant messages in each of three sessions"
    );
    assert!(covered > 0 && covered <= messages, "{covered}/{messages}");
}

fn rig_config(rig: &Rig) -> KbConfig {
    KbConfig {
        data_dir: rig.kb.data.root().to_path_buf(),
        digest: rig.cfg.clone(),
        ..KbConfig::default()
    }
}

impl Rig {
    fn clock_now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }
}

#[test]
fn when_the_quota_runs_out_nothing_is_called_for_an_hour_and_only_the_rest_is_done() {
    let rig = Rig::new();
    for id in ["a", "b", "c", "d"] {
        rig.session(id, 6, "2026-10-15T14:00:00Z");
    }
    // Sessions are tried newest first; make the order explicit.
    rig.kb
        .conn
        .execute(
            "UPDATE sessions SET last_activity_at='2026-10-15T14:04:00Z' WHERE session_id='a'",
            [],
        )
        .unwrap();
    rig.ai.mode("ok");
    rig.ai.mode_for_call(3, "quota");
    rig.at("2026-10-16T00:30:00Z");

    let first = rig.run();
    assert_eq!(first.status, "paused");
    assert_eq!(first.pause_reason.as_deref(), Some("quota"));
    assert_eq!(first.done, 2);
    assert_eq!(rig.ai.calls(), 3);
    let states = rig.item_states("2026-10-16");
    assert_eq!(states.iter().filter(|s| s.1 == "done").count(), 2);
    let waiting: Vec<_> = states.iter().filter(|s| s.1 == "pending").collect();
    assert_eq!(waiting.len(), 2);
    assert!(
        waiting.iter().all(|s| s.2 <= 1),
        "the quota error cost no attempt: {states:?}"
    );
    assert_eq!(
        store::get_round(&rig.kb.conn, "2026-10-16")
            .unwrap()
            .unwrap()
            .state,
        "open"
    );

    // 20 and 50 minutes later: still paused, no call, and no process is started.
    for later in ["2026-10-16T00:50:00Z", "2026-10-16T01:20:00Z"] {
        rig.at(later);
        assert!(!due_check(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap());
        assert_eq!(rig.run().status, "paused");
        assert_eq!(rig.ai.calls(), 3);
    }

    // After the hour: only the two that were not done.
    rig.at("2026-10-16T01:35:00Z");
    assert!(due_check(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap());
    let second = rig.run();
    assert_eq!(
        (second.status.as_str(), second.done, second.remaining),
        ("ok", 2, 0)
    );
    assert_eq!(
        rig.ai.calls(),
        5,
        "exactly two more calls; the finished two are not repeated"
    );
    assert_eq!(
        store::get_round(&rig.kb.conn, "2026-10-16")
            .unwrap()
            .unwrap()
            .state,
        "done"
    );
    assert!(
        store::blocked(&rig.kb.conn).unwrap().is_none(),
        "a success lifts the pause"
    );
}

#[test]
fn codex_quota_and_login_problems_pause_the_same_way() {
    let mut rig = Rig::new();
    rig.cfg.engine = "codex".into();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    rig.ai.mode("quota");
    let summary = rig.run();
    assert_eq!(summary.pause_reason.as_deref(), Some("quota"));
    let paused = store::blocked(&rig.kb.conn).unwrap().unwrap();
    assert_eq!(paused.1, "quota");

    rig.at("2026-10-16T02:00:00Z");
    rig.ai.mode("auth");
    assert_eq!(rig.run().pause_reason.as_deref(), Some("auth"));
    assert_eq!(
        rig.item_states("2026-10-16")[0].2,
        0,
        "neither cost an attempt"
    );

    rig.at("2026-10-16T04:00:00Z");
    rig.ai.mode("ok");
    assert_eq!(rig.run().done, 1);
    assert!(
        rig.ai.args(3).contains(&"exec".to_string()),
        "it really ran the codex shape"
    );
}

#[test]
fn a_session_that_keeps_failing_is_given_up_after_three_attempts() {
    let rig = Rig::new();
    rig.session("bad", 6, "2026-10-15T14:00:00Z");
    rig.session("good", 6, "2026-10-15T13:00:00Z");
    rig.ai.mode("ok");
    // The newest session is "bad" (calls 1, 3, 4); "good" is call 2.
    for call in [1, 3, 4] {
        rig.ai.mode_for_call(call, "bad_json");
    }
    rig.at("2026-10-16T00:30:00Z");

    let first = rig.run();
    assert_eq!(first.done, 1, "the other session is not held up");
    let states = rig.item_states("2026-10-16");
    assert!(
        states.contains(&("bad".into(), "pending".into(), 1)),
        "{states:?}"
    );

    // Retries wait ten minutes.
    assert_eq!(rig.run().done, 0);
    assert_eq!(rig.ai.calls(), 2);
    rig.at("2026-10-16T00:45:00Z");
    assert_eq!(rig.run().status, "ok");
    rig.at("2026-10-16T01:00:00Z");
    let last = rig.run();
    assert_eq!(last.failed, 1);
    assert!(rig
        .item_states("2026-10-16")
        .contains(&("bad".into(), "failed".into(), 3)));
    assert_eq!(rig.ai.calls(), 4);

    // The round is complete, with a failure on record; it is not retried.
    assert_eq!(
        store::get_round(&rig.kb.conn, "2026-10-16")
            .unwrap()
            .unwrap()
            .state,
        "done"
    );
    rig.at("2026-10-16T09:00:00Z");
    assert_eq!(rig.run().status, "idle");
    assert_eq!(rig.ai.calls(), 4);
    let failures = store::failures(&rig.kb.conn).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(failures[0]
        .last_error
        .as_deref()
        .unwrap()
        .contains("invalid answer"));
    let report = status::status(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap();
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].attempts, 3);
    assert!(rig.note("bad").is_none(), "a failed session leaves no note");
}

#[test]
fn a_timeout_counts_as_a_failure_and_leaves_no_process_behind() {
    let mut rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.ai.mode("slow");
    rig.at("2026-10-16T00:30:00Z");
    let mut env = rig.env();
    env.call_timeout = Duration::from_millis(600);
    let started = std::time::Instant::now();
    let summary = run(&env, Trigger::Auto, &Scope::Auto).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the 30 s sleep was cut short"
    );
    assert_eq!(summary.failed + summary.done, 0);
    assert_eq!(
        rig.item_states("2026-10-16")[0],
        ("a".into(), "pending".into(), 1)
    );
    let error = store::items(&rig.kb.conn, "2026-10-16")
        .unwrap()
        .remove(0)
        .last_error
        .unwrap();
    assert!(error.contains("in time"), "{error}");
    rig.cfg.enabled = true;
}

#[test]
fn a_second_run_while_one_is_working_is_refused_without_calling_the_ai() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    rig.ai.mode("slow");
    let mut slow = rig.env();
    slow.call_timeout = Duration::from_secs(3);
    let background = std::thread::spawn(move || run(&slow, Trigger::Auto, &Scope::Auto).unwrap());
    // Wait until the fake has started.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while rig.ai.calls() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(rig.ai.calls(), 1);

    let manual = run(&rig.env(), Trigger::Gui, &Scope::All).unwrap();
    assert_eq!(manual.status, "busy");
    let one = run(
        &rig.env(),
        Trigger::Gui,
        &Scope::Session {
            source: "claude".into(),
            session_id: "a".into(),
        },
    )
    .unwrap();
    assert_eq!(one.status, "busy");
    assert!(
        !due_check(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap(),
        "no second process is started"
    );
    assert_eq!(rig.ai.calls(), 1, "no duplicate call");
    let seen = status::status(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap();
    assert!(seen.running.is_some());
    background.join().unwrap();
    assert!(
        status::status(&rig_config(&rig), rig.clock_now(), rig.env().tz)
            .unwrap()
            .running
            .is_none()
    );
}

#[test]
fn sessions_that_must_not_be_digested_alone_are_left_out() {
    let rig = Rig::new();
    let work = rig.kb.data.digest_work().to_string_lossy().into_owned();
    rig.session("ok", 6, "2026-10-15T14:00:00Z");
    rig.session("short", 2, "2026-10-15T14:00:00Z");
    rig.session("busy", 6, "2026-10-16T00:10:00Z"); // active 20 minutes before the run
    let own = rig
        .kb
        .session("claude", "own", &work, None, "2026-10-15T14:00:00Z");
    rig.kb.chat(own, 6, "2026-10-15T13:00:00Z");
    let excluded = rig.session("excluded", 6, "2026-10-15T14:00:00Z");
    rig.kb
        .conn
        .execute(
            "UPDATE sessions SET cwd_excluded=1 WHERE file_id=?1",
            [excluded],
        )
        .unwrap();
    let parent = rig.session("parent", 6, "2026-10-15T14:00:00Z");
    let sub = rig.kb.session(
        "claude",
        "agent-1",
        "/work/app",
        Some("parent"),
        "2026-10-15T14:00:00Z",
    );
    rig.kb.chat(sub, 8, "2026-10-15T13:00:00Z");
    let _ = parent;
    rig.at("2026-10-16T00:30:00Z");

    let summary = rig.run();
    assert_eq!(summary.done, 2, "{summary:?}");
    let digested: Vec<String> = rig
        .item_states("2026-10-16")
        .into_iter()
        .map(|i| i.0)
        .collect();
    assert_eq!(digested.len(), 2);
    assert!(digested.contains(&"ok".to_string()) && digested.contains(&"parent".to_string()));
    for id in ["short", "busy", "own", "excluded", "agent-1"] {
        assert!(rig.note(id).is_none(), "{id}");
    }

    // The busy session goes in the next round, once it has been quiet.
    rig.at("2026-10-16T19:30:00Z");
    assert_eq!(rig.run().done, 1);
    assert!(rig.note("busy").is_some());
}

#[test]
fn a_session_that_goes_on_is_continued_from_its_notes_with_only_the_new_messages() {
    let rig = Rig::new();
    let file = rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    rig.run();
    assert_eq!(rig.ai.calls(), 1);
    let first = rig.note("a").unwrap();
    assert_eq!(first.entries.len(), 1);

    // The next afternoon the conversation continues; round 2 starts at 03:00 the day after.
    rig.kb.messages(
        file,
        7,
        &[
            ("user", "2026-10-16T05:00:00Z", "a brand new question"),
            ("assistant", "2026-10-16T05:01:00Z", "a brand new answer"),
            ("user", "2026-10-17T05:00:00Z", "after the next cut-off"),
            ("assistant", "2026-10-17T05:01:00Z", "also after it"),
        ],
    );
    rig.kb
        .conn
        .execute(
            "UPDATE sessions SET last_activity_at='2026-10-17T05:01:00Z' WHERE session_id='a'",
            [],
        )
        .unwrap();
    rig.at("2026-10-17T06:00:00Z"); // 14:00 local on the 17th, round 2026-10-17 is due
    let summary = rig.run();
    assert_eq!(summary.done, 1);
    assert_eq!(rig.ai.calls(), 2);

    let sent = rig.ai.input(2);
    assert!(sent.contains("a brand new question"));
    assert!(
        !sent.contains("message 0 about locks"),
        "the old messages are not sent again"
    );
    assert!(
        !sent.contains("after the next cut-off"),
        "later messages wait for the next round"
    );
    assert!(
        sent.contains("<previous_notes>") && sent.contains("Fake note 1 seq 1-6"),
        "the previous note goes along"
    );
    assert!(sent.contains("<messages from=#7 to=#8>"));
    let second = rig.note("a").unwrap();
    assert_eq!(second.digested_through_seq, 8);
    assert_eq!(second.history.len(), 2);
    assert_eq!(
        second.entries.len(),
        2,
        "the old entry stays, the new one is added"
    );
    assert_eq!(second.entries[0].round, "2026-10-16");
    assert_eq!(second.entries[1].round, "2026-10-17");
}

#[test]
fn a_long_session_is_sent_in_pieces_and_continues_next_round_when_the_cap_is_reached() {
    let mut rig = Rig::new();
    rig.cfg.max_chunks_per_session = 2;
    let file = rig
        .kb
        .session("claude", "big", "/work/app", None, "2026-10-15T14:00:00Z");
    let mut rows = Vec::new();
    for turn in 0..120 {
        rows.push((
            "user",
            "2026-10-15T10:00:00Z",
            format!("question {turn} {}", "q".repeat(3000)),
        ));
        rows.push((
            "assistant",
            "2026-10-15T10:01:00Z",
            format!("answer {turn} {}", "a".repeat(1000)),
        ));
    }
    let borrowed: Vec<(&str, &str, &str)> =
        rows.iter().map(|(k, t, x)| (*k, *t, x.as_str())).collect();
    rig.kb.messages(file, 1, &borrowed);
    rig.at("2026-10-16T00:30:00Z");

    // Each 50k-character piece holds about 22 turns; two pieces leave the rest.
    let summary = rig.run();
    assert_eq!(summary.calls, 2);
    let note = rig.note("big").unwrap();
    assert_eq!(note.status, "partial");
    assert!(note.digested_through_seq > 0 && note.digested_through_seq < 240);
    assert_eq!(
        rig.ai.input(2).matches("<previous_notes>").count(),
        1,
        "the second piece carries the first note"
    );

    // Day by day, with no new activity, the unfinished session carries on until it is done.
    let mut rounds = 1;
    while rig.note("big").unwrap().status == "partial" && rounds < 8 {
        rounds += 1;
        rig.at(&format!("2026-10-{:02}T00:30:00Z", 15 + rounds));
        let next = rig.run();
        assert_eq!(
            next.done, 1,
            "an unfinished session continues without new activity"
        );
        assert!(
            next.calls <= 2,
            "each round stays within the per-session cap"
        );
    }
    let note = rig.note("big").unwrap();
    assert!(rounds >= 2, "it did not fit in one round");
    assert_eq!(note.digested_through_seq, 240);
    assert_eq!(note.status, "notes");
}

#[test]
fn the_text_sent_to_the_ai_has_no_secrets() {
    let rig = Rig::new();
    let file = rig
        .kb
        .session("claude", "s", "/work/app", None, "2026-10-15T14:00:00Z");
    rig.kb.messages(
        file,
        1,
        &[
            (
                "user",
                "2026-10-15T10:00:00Z",
                "deploy with password=hunter2hunter2 please",
            ),
            (
                "assistant",
                "2026-10-15T10:01:00Z",
                "using sk-abcdefghijklmnopqrstuvwxyz012345 now",
            ),
            ("user", "2026-10-15T10:02:00Z", "thanks"),
            ("assistant", "2026-10-15T10:03:00Z", "done"),
        ],
    );
    rig.at("2026-10-16T00:30:00Z");
    rig.run();
    let sent = rig.ai.input(1);
    assert!(sent.contains("deploy with"));
    assert!(!sent.contains("hunter2hunter2"));
    assert!(!sent.contains("sk-abcdefghijkl"));
}

#[test]
fn a_secret_in_the_answer_never_reaches_the_note_file() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.ai.mode("leaky");
    rig.at("2026-10-16T00:30:00Z");
    rig.run();
    let raw = std::fs::read_to_string(rig.kb.data.notes_file("claude", "a").unwrap()).unwrap();
    assert!(!raw.contains("hunter2hunter2"), "{raw}");
    assert!(raw.contains("[REDACTED]"));
}

#[test]
fn an_answer_that_names_messages_outside_the_session_is_a_failed_attempt() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.ai.mode("bad_range");
    rig.at("2026-10-16T00:30:00Z");
    rig.run();
    assert!(rig.note("a").is_none());
    let item = store::items(&rig.kb.conn, "2026-10-16").unwrap().remove(0);
    assert_eq!(item.state, "pending");
    assert!(item.last_error.unwrap().contains("outside"));
}

#[test]
fn nothing_worth_keeping_still_counts_as_digested() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.ai.mode("nothing");
    rig.at("2026-10-16T00:30:00Z");
    let summary = rig.run();
    assert_eq!(summary.done, 1);
    let note = rig.note("a").unwrap();
    assert_eq!((note.status.as_str(), note.entries.len()), ("nothing", 0));
    assert_eq!(
        notes::digested_seq(&rig.kb.conn, "claude", "a").unwrap(),
        Some(6)
    );
    rig.at("2026-10-17T00:30:00Z");
    assert_eq!(rig.run().status, "idle");
    assert_eq!(rig.ai.calls(), 1);
}

#[test]
fn the_per_round_cap_defers_the_rest_without_failing_them() {
    let mut rig = Rig::new();
    rig.cfg.max_sessions_per_round = 2;
    for (n, id) in ["a", "b", "c", "d"].iter().enumerate() {
        rig.session(id, 6, &format!("2026-10-15T1{n}:00:00Z"));
    }
    rig.at("2026-10-16T00:30:00Z");
    let first = rig.run();
    assert_eq!((first.done, first.failed), (2, 0));
    assert_eq!(
        store::get_round(&rig.kb.conn, "2026-10-16")
            .unwrap()
            .unwrap()
            .state,
        "done"
    );
    rig.at("2026-10-17T00:30:00Z");
    assert_eq!(rig.run().done, 2, "the next round takes the other two");
}

#[test]
fn manual_runs_cover_old_sessions_and_a_single_session_ignores_the_quiet_rule() {
    let rig = Rig::new();
    rig.session("old", 6, "2026-08-01T14:00:00Z");
    rig.session("recent", 6, "2026-10-15T14:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    let all = run(&rig.env(), Trigger::Gui, &Scope::All).unwrap();
    assert_eq!(all.done, 2);
    assert!(all.rounds[0].starts_with("manual-"));
    assert_eq!(rig.ai.calls(), 2);

    // A conversation still going on: "digest this one now".
    let live = rig
        .kb
        .session("claude", "live", "/work/app", None, "2026-10-16T00:29:00Z");
    rig.kb.chat(live, 5, "2026-10-16T00:20:00Z");
    assert_eq!(
        run(&rig.env(), Trigger::Gui, &Scope::All).unwrap().status,
        "idle"
    );
    let one = run(
        &rig.env(),
        Trigger::Gui,
        &Scope::Session {
            source: "claude".into(),
            session_id: "live".into(),
        },
    )
    .unwrap();
    assert_eq!(one.done, 1);
    let nothing_new = run(
        &rig.env(),
        Trigger::Gui,
        &Scope::Session {
            source: "claude".into(),
            session_id: "live".into(),
        },
    );
    assert!(
        nothing_new.is_err(),
        "a session with nothing new is refused"
    );
}

#[test]
fn the_ai_is_started_in_the_work_directory_with_input_on_stdin_and_never_in_argv() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    rig.run();
    let work = std::fs::canonicalize(rig.kb.data.digest_work()).unwrap();
    assert_eq!(std::fs::canonicalize(rig.ai.cwd(1)).unwrap(), work);
    let args = rig.ai.args(1).join("\n");
    assert!(args.contains("--safe-mode") && args.contains("--no-session-persistence"));
    assert!(
        !args.contains("message 0 about locks"),
        "the conversation is not in argv"
    );
    assert!(rig.ai.input(1).contains("message 0 about locks"));
    let leftovers: Vec<_> = std::fs::read_dir(&work).unwrap().flatten().collect();
    assert!(
        leftovers.is_empty(),
        "no temp file is left in the work directory"
    );
}

#[test]
fn a_cancelled_run_puts_the_session_back_without_a_failure() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.ai.mode("slow");
    rig.at("2026-10-16T00:30:00Z");
    let env = rig.env();
    let cancel = Arc::clone(&env.cancel);
    let worker = std::thread::spawn(move || run(&env, Trigger::Gui, &Scope::All).unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while rig.ai.calls() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));
    }
    cancel.store(true, Ordering::SeqCst);
    let summary = worker.join().unwrap();
    assert_eq!(summary.status, "cancelled");
    let round = &summary.rounds[0];
    assert_eq!(rig.item_states(round)[0], ("a".into(), "pending".into(), 0));
    assert_eq!(
        store::get_round(&rig.kb.conn, round)
            .unwrap()
            .unwrap()
            .state,
        "superseded"
    );
    assert!(rig.note("a").is_none());
}

#[test]
fn an_interrupted_item_is_recovered_as_one_failed_attempt() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    rig.run(); // plans and digests
    let conn = &rig.kb.conn;
    // Pretend the process died during a later call.
    conn.execute(
        "UPDATE digest_items SET state='running', attempts=1 WHERE session_id='a'",
        [],
    )
    .unwrap();
    store::set_round_state(conn, "2026-10-16", "open", None).unwrap();
    notes::reconcile(conn, &rig.kb.data).unwrap();
    assert!(due_check(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap());
    rig.ai.mode("ok");
    let summary = rig.run();
    assert_eq!(summary.status, "ok");
}

#[test]
fn the_digest_does_nothing_while_it_is_switched_off_or_the_cli_is_missing() {
    let mut rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    rig.cfg.enabled = false;
    assert_eq!(rig.run().status, "disabled");
    assert!(!due_check(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap());
    assert_eq!(rig.ai.calls(), 0);

    rig.cfg.enabled = true;
    rig.cfg.cli_path = Some(rig.kb.dir.path().join("no-such-cli"));
    let summary = rig.run();
    assert_eq!(summary.status, "paused");
    assert_eq!(summary.pause_reason.as_deref(), Some("config"));
    assert_eq!(rig.item_states("2026-10-16")[0].2, 0);
}

#[test]
fn the_test_button_reports_success_and_each_kind_of_failure() {
    let rig = Rig::new();
    let env = rig.env();
    rig.ai.mode("nothing");
    let ok = status::test_call(&env);
    assert!(ok.ok, "{ok:?}");
    assert_eq!((ok.input_tokens, ok.output_tokens), (115, 20));
    rig.ai.mode("quota");
    assert_eq!(status::test_call(&env).kind.as_deref(), Some("quota"));
    rig.ai.mode("auth");
    assert_eq!(status::test_call(&env).kind.as_deref(), Some("auth"));
    rig.ai.mode("bad_json");
    assert_eq!(
        status::test_call(&env).kind.as_deref(),
        Some("invalid_answer")
    );
    let mut missing = rig.env();
    missing.config.cli_path = Some("/nope/claude".into());
    assert_eq!(status::test_call(&missing).kind.as_deref(), Some("config"));
}

#[test]
fn the_status_report_describes_the_last_round_and_what_is_waiting() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.session("old", 6, "2026-08-01T14:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    let before = status::status(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap();
    assert!(before.last_round.is_none());
    assert_eq!((before.pending_auto, before.pending_older), (1, 1));
    assert!(
        before.first_round_tokens.unwrap() > 0,
        "the first round is announced with its cost"
    );

    rig.run();
    let after = status::status(&rig_config(&rig), rig.clock_now(), rig.env().tz).unwrap();
    let last = after.last_round.unwrap();
    assert_eq!(
        (last.round_id.as_str(), last.state.as_str(), last.done),
        ("2026-10-16", "done", 1)
    );
    assert_eq!((last.input_tokens, last.output_tokens), (115, 20));
    assert_eq!((after.pending_auto, after.pending_older), (0, 1));
    assert!(after.first_round_tokens.is_none());
    assert!(after.next_run_at.is_some());
    let estimate = status::estimate_all(&rig_config(&rig), rig.clock_now()).unwrap();
    assert_eq!(estimate.sessions, 1);
    assert!(estimate.input_tokens > 0);
}

#[test]
fn rebuilding_the_index_brings_every_note_back_and_search_finds_them() {
    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.session("b", 6, "2026-10-15T13:00:00Z");
    rig.at("2026-10-16T00:30:00Z");
    rig.run();
    let before: i64 = rig
        .kb
        .conn
        .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, 2);
    rig.kb
        .conn
        .execute_batch("DROP TABLE notes_fts; DROP TABLE notes; DROP TABLE digest_sessions;")
        .unwrap();
    notes::rebuild_all(&rig.kb.conn, &rig.kb.data).unwrap();
    let after: i64 = rig
        .kb
        .conn
        .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, 2);
    assert_eq!(
        notes::digested_seq(&rig.kb.conn, "claude", "b").unwrap(),
        Some(6)
    );
    // Digesting resumes where it left off: nothing is redone.
    rig.at("2026-10-17T00:30:00Z");
    assert_eq!(rig.run().status, "idle");
}

#[test]
fn a_collection_is_not_held_up_by_a_digest_that_is_working() {
    use crate::kb::config::SourceRoots;
    use crate::kb::progress::ProgressSink;
    use crate::kb::sync::{run_sync, KbEnv};

    let rig = Rig::new();
    rig.session("a", 6, "2026-10-15T14:00:00Z");
    rig.ai.mode("slow");
    rig.at("2026-10-16T00:30:00Z");

    // A real source file for the collection to pick up.
    let claude = rig.kb.dir.path().join(".claude");
    let session = claude.join("projects/-tmp-p/11111111-2222-3333-4444-555555555555.jsonl");
    std::fs::create_dir_all(session.parent().unwrap()).unwrap();
    std::fs::write(
        &session,
        r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","cwd":"/tmp/p","message":{"role":"user","content":"hello there"}}
"#,
    )
    .unwrap();
    // The hand-made sessions have no mirror files; mark the index as current
    // so the collection does not try to rebuild them.
    db::set_meta(
        &rig.kb.conn,
        "indexer_version",
        crate::kb::sync::INDEXER_VERSION,
    )
    .unwrap();
    db::set_meta(&rig.kb.conn, "exclude_cwd_applied", "").unwrap();
    let sync_env = KbEnv {
        data: rig.kb.data.clone(),
        roots: SourceRoots {
            claude: Some(claude),
            codex: None,
        },
        exclude_prefixes: vec![],
        exclude_cwd_prefixes: vec![],
        mirror: None,
        progress: ProgressSink::default(),
        force_reindex: false,
    };

    let mut slow = rig.env();
    slow.call_timeout = Duration::from_secs(4);
    let background = std::thread::spawn(move || run(&slow, Trigger::Auto, &Scope::Auto).unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while rig.ai.calls() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(rig.ai.calls(), 1, "the digest is in the middle of a call");

    let started = std::time::Instant::now();
    let summary = run_sync(&sync_env, "test").unwrap();
    let took = started.elapsed();
    assert_eq!(summary.status, "ok", "{:?}", summary.errors);
    assert_eq!(summary.new_sessions, 1);
    assert!(
        took < Duration::from_secs(2),
        "the collection took {took:?} while the digest waited for the AI"
    );
    background.join().unwrap();
}

#[test]
fn a_note_remembers_where_its_evidence_starts_even_without_a_message_key() {
    let rig = Rig::new();
    rig.session("with-key", 6, "2026-10-15T14:00:00Z");
    let codex = rig.kb.session(
        "codex",
        "codex-1",
        "/work/app",
        None,
        "2026-10-15T14:00:00Z",
    );
    rig.kb.chat(codex, 6, "2026-10-15T13:00:00Z");
    rig.kb
        .conn
        .execute("UPDATE messages SET msg_key=NULL WHERE file_id=?1", [codex])
        .unwrap();
    rig.at("2026-10-16T00:30:00Z");
    rig.run();

    let keyed = &rig.note("with-key").unwrap().entries[0].evidence;
    assert_eq!(keyed.msg_key.as_deref(), Some("key-1"));
    assert_eq!(keyed.kind.as_deref(), Some("user"));
    assert!(keyed.at.is_some());

    let plain = &notes::read(&rig.kb.data, "codex", "codex-1")
        .unwrap()
        .unwrap()
        .entries[0]
        .evidence;
    assert_eq!(plain.msg_key, None, "Codex has no message keys");
    assert_eq!(plain.kind.as_deref(), Some("user"));
    assert!(plain.at.as_deref().unwrap().starts_with("2026-10-15T13:00"));
}

// ---- P7.5: a failure is gone once the readable messages in its range are digested

impl Rig {
    /// A session of `inputs` readable messages followed by `tails` tool results,
    /// with a failed item (old round) whose range is every message, like the
    /// failure records of before 0.7.5.
    fn failed_with_tail(&self, id: &str, inputs: usize, tails: usize) -> i64 {
        let file = self.session(id, inputs, "2026-10-15T14:00:00Z");
        let rows: Vec<(&str, &str, &str)> = (0..tails)
            .map(|_| ("tool_result", "2026-10-15T14:01:00Z", "output"))
            .collect();
        self.kb
            .messages(file, i64::try_from(inputs).unwrap() + 1, &rows);
        let conn = &self.kb.conn;
        conn.execute(
            "INSERT OR IGNORE INTO digest_rounds VALUES ('old','manual','2026-10-15T00:00:00Z', \
             '2026-10-15T00:00:00Z','2026-10-15T01:00:00Z','done','gui','codex','m')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO digest_items (round_id, source, session_id, state, attempts, last_error, \
             seq_to, finished_at) VALUES ('old','claude',?1,'failed',3,'boom',?2,'2026-10-15T00:30:00Z')",
            params![id, i64::try_from(inputs + tails).unwrap()],
        )
        .unwrap();
        file
    }

    fn digested_to(&self, id: &str, seq: usize) {
        self.kb
            .conn
            .execute(
                "INSERT OR REPLACE INTO digest_sessions VALUES ('claude',?1,'notes',?2,'t','h')",
                params![id, i64::try_from(seq).unwrap()],
            )
            .unwrap();
    }

    fn failing(&self) -> usize {
        store::failures(&self.kb.conn).unwrap().len()
    }

    fn readable_left(&self, id: &str, digested: usize) -> bool {
        let info = input::session_info(&self.kb.conn, "claude", id)
            .unwrap()
            .unwrap();
        input::next_chunk(
            &self.kb.conn,
            &info,
            digested as u64,
            utc("2026-10-20T00:00:00Z"),
            None,
            input::CHUNK_CHARS,
        )
        .unwrap()
        .is_some()
    }
}

#[test]
fn a_failure_whose_readable_messages_are_digested_is_resolved_whatever_trails_it() {
    let rig = Rig::new();
    // The boundaries of the three Windows sessions: 66/67, 150/151, 188/191.
    for (id, inputs, tails) in [("a", 66, 1), ("b", 150, 1), ("c", 188, 3), ("d", 6, 1)] {
        rig.failed_with_tail(id, inputs, tails);
        assert_eq!(rig.failing(), 1, "{id}: not digested yet, still failing");
        rig.kb
            .conn
            .execute("DELETE FROM digest_items WHERE session_id<>?1", [id])
            .ok();
        rig.digested_to(id, inputs);
        assert_eq!(rig.failing(), 0, "{id}: resolved");
        assert!(
            !rig.readable_left(id, inputs),
            "{id}: no model call would start"
        );
        // The history row stays.
        let kept: i64 = rig
            .kb
            .conn
            .query_row(
                "SELECT COUNT(*) FROM digest_items WHERE session_id=?1 AND state='failed'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1);
        rig.kb.conn.execute("DELETE FROM digest_items", []).unwrap();
        rig.kb
            .conn
            .execute("DELETE FROM digest_sessions", [])
            .unwrap();
    }
}

#[test]
fn a_failure_with_readable_messages_left_in_its_range_stays_and_can_be_retried() {
    let rig = Rig::new();
    rig.failed_with_tail("part", 20, 2);
    rig.digested_to("part", 8); // only the first piece went through
    assert_eq!(rig.failing(), 1);
    assert!(rig.readable_left("part", 8));
    // A failure that never had a note is no different.
    rig.failed_with_tail("never", 6, 0);
    assert_eq!(rig.failing(), 2);
}

#[test]
fn a_session_that_only_gained_tool_results_is_neither_failing_nor_digested_again() {
    let rig = Rig::new();
    let file = rig.failed_with_tail("grow", 6, 1);
    rig.digested_to("grow", 6);
    assert_eq!(rig.failing(), 0);
    rig.kb.messages(
        file,
        8,
        &[("tool_result", "2026-10-15T14:05:00Z", "more output")],
    );
    assert_eq!(rig.failing(), 0);
    rig.at("2026-10-16T00:30:00Z");
    rig.ai.mode("ok");
    let summary = rig.run();
    assert_eq!(
        rig.ai.calls(),
        0,
        "{summary:?}: tool results alone start no call"
    );

    // A real new message past the failure's range ends the old failure and is
    // picked up by the normal incremental rules.
    rig.kb.chat_more(file, 9);
    assert_eq!(rig.failing(), 0);
    assert!(rig.readable_left("grow", 6));
}

#[test]
fn a_held_session_stays_held_until_a_readable_message_arrives() {
    let rig = Rig::new();
    let file = rig.failed_with_tail("held", 6, 1);
    let newest = |rig: &Rig| store::newest_input_seq(&rig.kb.conn, "claude", "held").unwrap();
    assert_eq!(newest(&rig), 6);
    assert!(store::is_held(&rig.kb.conn, "claude", "held", newest(&rig)).unwrap());
    rig.kb
        .messages(file, 8, &[("tool_result", "2026-10-15T14:05:00Z", "x")]);
    assert!(store::is_held(&rig.kb.conn, "claude", "held", newest(&rig)).unwrap());
    rig.kb.chat_more(file, 9);
    assert!(!store::is_held(&rig.kb.conn, "claude", "held", newest(&rig)).unwrap());
}

#[test]
fn retrying_a_finished_session_says_why_with_a_code() {
    let rig = Rig::new();
    rig.failed_with_tail("done", 6, 1);
    rig.digested_to("done", 6);
    let config = rig_config(&rig);
    let blocker = super::commands::session_blocker(&config, "claude", "done").unwrap();
    assert_eq!(blocker.map(|b| b.0), Some("up_to_date"));
    let missing = super::commands::session_blocker(&config, "claude", "nope").unwrap();
    assert_eq!(missing.map(|b| b.0), Some("not_indexed"));
}
