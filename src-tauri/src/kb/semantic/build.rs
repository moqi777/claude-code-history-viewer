//! Building the vectors a little at a time.
//!
//! One run (`--kb-embed`, started after a collection) compares what should
//! have a vector with what has one, drops what is stale, and computes the
//! missing ones for at most [`AUTO_BUDGET`] seconds. It reads `kb.db` and never
//! writes it, holds only `embed.lock`, and runs at the lowest priority. A run
//! that the machine is too short of memory for is put off to the next one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::chunk::Chunk;
use super::embed::Embedder;
use super::state::{self, EmbedState, RunReport};
use super::store::{self, Key, Store};
use super::{corpus, model, sys};
use crate::kb::lock::SyncLock;
use crate::kb::log::SyncLog;
use crate::kb::paths::DataPaths;

/// Computing time of one automatic run (loading the model comes on top).
pub const AUTO_BUDGET: Duration = Duration::from_secs(50);
/// A run starts only with this much memory to spare (see `sys::available_memory`).
pub const MIN_AVAILABLE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const AUTO_THREADS: usize = 3;
pub const BATCH: usize = 8;
/// Chunks taken in priority order, then sorted by length so a batch pads little.
const WINDOW: usize = 64;

pub fn lock_path(data: &DataPaths) -> PathBuf {
    store::dir(data).join("embed.lock")
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Stop computing after this long; `None` goes on until done or cancelled.
    pub budget: Option<Duration>,
    pub threads: usize,
    /// Lowest priority, and skip the run when memory is short.
    pub polite: bool,
}

impl Options {
    pub fn automatic() -> Self {
        // The variables are for measuring and testing only.
        let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
        Self {
            budget: Some(
                number("AGENTKB_EMBED_BUDGET_SECS").map_or(AUTO_BUDGET, Duration::from_secs),
            ),
            threads: number("AGENTKB_EMBED_THREADS")
                .and_then(|n| usize::try_from(n).ok())
                .map_or(AUTO_THREADS, |n| n.max(1)),
            polite: true,
        }
    }
    /// "Build everything now": normal priority, all cores but a few, until done.
    pub fn everything() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
        Self {
            budget: None,
            threads: cores.saturating_sub(2).clamp(2, 8),
            polite: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ran,
    /// Nothing to compute.
    UpToDate,
    /// Another run holds the lock.
    Busy,
    /// Not enough free memory; try again next time.
    Postponed,
    /// No (valid) model.
    NoModel(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub removed: Vec<Key>,
}

/// Split `desired` into what must be computed (in the given order) and the
/// rest; `stored` keys no longer wanted are `removed`.
#[allow(clippy::implicit_hasher)]
pub fn diff<'a>(
    desired: &'a [Chunk],
    stored: &std::collections::HashMap<Key, String>,
) -> (Vec<&'a Chunk>, Diff) {
    let mut d = Diff::default();
    let mut todo = Vec::new();
    let mut wanted: HashSet<Key> = HashSet::with_capacity(desired.len());
    for c in desired {
        let key = (c.source.clone(), c.session_id.clone(), c.key.clone());
        match stored.get(&key) {
            None => {
                d.new += 1;
                todo.push(c);
            }
            Some(sha) if *sha != c.sha => {
                d.changed += 1;
                todo.push(c);
            }
            Some(_) => d.unchanged += 1,
        }
        wanted.insert(key);
    }
    d.removed = stored
        .keys()
        .filter(|k| !wanted.contains(*k))
        .cloned()
        .collect();
    (todo, d)
}

/// What `run` needs from outside, so tests can stand in for the model and the
/// machine's memory.
pub struct Env<'a> {
    pub data: &'a DataPaths,
    pub model_spec: &'a model::ModelSpec,
    pub available_memory: &'a dyn Fn() -> Option<u64>,
    pub make_embedder: &'a dyn Fn(usize) -> Result<Box<dyn Embedder>, String>,
    pub lower_priority: bool,
}

fn write_state(data: &DataPaths, f: impl FnOnce(&mut EmbedState)) {
    let mut s = state::read(data);
    f(&mut s);
    s.updated_at = state::now();
    state::write(data, &s);
}

pub fn run(env: &Env<'_>, opts: &Options, cancel: &AtomicBool) -> Result<Outcome, String> {
    let data = env.data;
    let log = SyncLog::open_embed(&data.logs());
    let Some(_lock) = SyncLock::acquire(&lock_path(data), Duration::ZERO)? else {
        return Ok(Outcome::Busy);
    };
    let started = Instant::now();

    match model::check(data, env.model_spec) {
        model::ModelStatus::Ready => {}
        model::ModelStatus::Missing => {
            write_state(data, |s| {
                s.status = "paused_model".into();
                s.message = "model not downloaded".into();
                s.pid = None;
            });
            return Ok(Outcome::NoModel("missing".into()));
        }
        model::ModelStatus::Corrupt { file, reason } => {
            let msg = format!("{file}: {reason}");
            log.line(&format!("embed: model is damaged ({msg})"));
            write_state(data, |s| {
                s.status = "paused_model".into();
                s.message = format!("model damaged: {msg}");
                s.pid = None;
            });
            return Ok(Outcome::NoModel(msg));
        }
    }

    let db = data.db();
    if !db.is_file() {
        return Ok(Outcome::UpToDate);
    }
    let conn = corpus::open_ro(&db)?;
    let desired = corpus::desired(&conn)?;
    drop(conn);
    let vectors_path = store::path(data);
    let mut vectors = match Store::open(&vectors_path, model::model_sha()) {
        Ok(v) => v,
        Err(e) => {
            // Derived data: a damaged file is thrown away and made again.
            log.line(&format!(
                "embed: the vector file cannot be used ({e}); starting it again"
            ));
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", vectors_path.display()));
            }
            Store::open(&vectors_path, model::model_sha())?
        }
    };
    let stored = vectors.keys()?;
    let (todo, d) = diff(&desired, &stored);
    log.line(&format!(
        "embed: wanted {} new {} changed {} unchanged {} removed {}",
        desired.len(),
        d.new,
        d.changed,
        d.unchanged,
        d.removed.len()
    ));
    if !d.removed.is_empty() {
        vectors.remove(&d.removed)?;
    }
    let total = desired.len();
    if todo.is_empty() {
        write_state(data, |s| {
            s.status = "idle".into();
            s.total = total;
            s.done = total;
            s.message = String::new();
            s.pid = None;
            s.postponed_rounds = 0;
            s.last_run = Some(RunReport {
                new: 0,
                changed: 0,
                unchanged: d.unchanged,
                removed: d.removed.len(),
                embedded: 0,
                seconds: started.elapsed().as_secs_f64(),
                finished_at: state::now(),
            });
        });
        return Ok(Outcome::UpToDate);
    }

    if opts.polite {
        if let Some(free) = (env.available_memory)() {
            if free < MIN_AVAILABLE_BYTES {
                log.line(&format!(
                    "embed: postponed, {} MB available, {} MB needed",
                    free / 1_048_576,
                    MIN_AVAILABLE_BYTES / 1_048_576
                ));
                write_state(data, |s| {
                    s.status = "postponed_memory".into();
                    s.total = total;
                    s.done = total - todo.len();
                    s.message = format!("{} MB available", free / 1_048_576);
                    s.postponed_rounds += 1;
                    s.pid = None;
                });
                return Ok(Outcome::Postponed);
            }
        }
    }
    if env.lower_priority {
        sys::lower_priority();
    }
    write_state(data, |s| {
        s.status = "running".into();
        s.total = total;
        s.done = total - todo.len();
        s.message = String::new();
        s.pid = Some(std::process::id());
    });
    let mut embedder = (env.make_embedder)(opts.threads)?;
    log.line(&format!(
        "embed: model loaded in {:.1}s",
        started.elapsed().as_secs_f64()
    ));
    let compute_from = Instant::now();
    let mut embedded = 0usize;
    let out_of_time = || {
        cancel.load(Ordering::SeqCst) || opts.budget.is_some_and(|b| compute_from.elapsed() >= b)
    };
    for window in todo.chunks(WINDOW) {
        if out_of_time() {
            break;
        }
        let mut order: Vec<&Chunk> = window.to_vec();
        order.sort_by_key(|c| c.text.chars().count());
        let mut rows: Vec<(&Chunk, Vec<f32>)> = Vec::with_capacity(order.len());
        for batch in order.chunks(BATCH) {
            if out_of_time() {
                break;
            }
            let texts: Vec<String> = batch
                .iter()
                .map(|c| crate::kb::redact::redact(&c.text).into_owned())
                .collect();
            let vecs = embedder.embed_passages(&texts)?;
            rows.extend(batch.iter().copied().zip(vecs));
        }
        vectors.put(&rows)?;
        embedded += rows.len();
        let stopped_inside = rows.len() < window.len();
        let done = total - todo.len() + embedded;
        write_state(data, |s| {
            s.done = done;
            s.pid = Some(std::process::id());
        });
        if stopped_inside {
            break; // what was computed is saved
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let done = total - todo.len() + embedded;
    log.line(&format!(
        "embed: computed {embedded} in {secs:.1}s ({done} of {total} have vectors)"
    ));
    write_state(data, |s| {
        s.status = "idle".into();
        s.total = total;
        s.done = done;
        s.pid = None;
        s.message = String::new();
        s.postponed_rounds = 0;
        s.last_run = Some(RunReport {
            new: d.new,
            changed: d.changed,
            unchanged: d.unchanged,
            removed: d.removed.len(),
            embedded,
            seconds: secs,
            finished_at: state::now(),
        });
    });
    Ok(Outcome::Ran)
}

/// Start `--kb-embed --auto` as a separate, detached process and return at
/// once (the collection that calls this never waits for it).
#[allow(unsafe_code)] // `setsid(2)` between fork and exec
pub fn spawn_background(exe: &Path, data: &DataPaths, args: &[&str]) -> Result<(), String> {
    let logs = data.logs();
    std::fs::create_dir_all(&logs).map_err(|e| format!("cannot create {}: {e}", logs.display()))?;
    let log_path = logs.join(format!(
        "embed-{}.log",
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
        .stdin(std::process::Stdio::null())
        .stdout(open()?)
        .stderr(open()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: only async-signal-safe `setsid` runs between fork and exec.
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
        .map_err(|e| format!("cannot start the vector builder: {e}"))
}

/// Whether a builder is running right now.
pub fn is_running(data: &DataPaths) -> bool {
    crate::kb::lock::held_by_live_process(&lock_path(data)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::semantic::embed::fake::FakeEmbedder;
    use rusqlite::Connection;
    use std::cell::Cell;
    use std::rc::Rc;

    const TEST_SPEC_BODY: &[u8] = b"fake model";

    fn test_spec() -> &'static model::ModelSpec {
        use sha2::{Digest, Sha256};
        let sha = Sha256::digest(TEST_SPEC_BODY)
            .iter()
            .fold(String::new(), |mut s, b| {
                use std::fmt::Write;
                let _ = write!(s, "{b:02x}");
                s
            });
        let sha: &'static str = Box::leak(sha.into_boxed_str());
        let files: &'static [model::FileSpec] = Box::leak(Box::new([model::FileSpec {
            name: "model.onnx",
            size: TEST_SPEC_BODY.len() as u64,
            sha256: sha,
            remote: "m",
            local_names: &["model.onnx"],
        }]));
        Box::leak(Box::new(model::ModelSpec {
            files,
            hf_repo: "x",
            ms_repo: "x",
        }))
    }

    /// A data dir with a `kb.db` holding two sessions, one note, and a model.
    fn setup() -> (tempfile::TempDir, DataPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataPaths::new(tmp.path());
        std::fs::create_dir_all(model::dir(&data)).unwrap();
        std::fs::write(model::dir(&data).join("model.onnx"), TEST_SPEC_BODY).unwrap();
        let conn = Connection::open(data.db()).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE sessions (file_id INTEGER PRIMARY KEY, source TEXT, session_id TEXT, title TEXT, last_activity_at TEXT, cwd_excluded INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE messages (file_id INTEGER, seq INTEGER, kind TEXT, text TEXT);
             CREATE TABLE notes (id INTEGER PRIMARY KEY, source TEXT, session_id TEXT, entry_id TEXT, title TEXT, problem TEXT, cause TEXT, solution TEXT, conclusion TEXT, keywords TEXT, digested_at TEXT);
             INSERT INTO sessions VALUES (1,'claude','s1','电池掉电快问题排查','2026-10-02',0),(2,'codex','s2','Mac 充电功率查看','2026-10-01',0),(3,'codex','s3','被排除的会话','2026-10-03',1);
             INSERT INTO messages VALUES (1,1,'user','我的笔记本电池掉电特别快，帮我看看是哪个进程在耗电'),
               (1,2,'assistant','先看 pmset -g log 和活动监视器的能耗列，找出耗电大户。'),
               (1,3,'tool_result','这一条是工具输出，不应该被建向量，哪怕它很长很长很长很长'),
               (2,1,'user','mac 的查看本机充电功率的命令是什么，我想知道现在充了多少瓦'),
               (3,1,'user','这个会话被工作目录规则排除了，不应出现在语义索引里面');
             INSERT INTO notes VALUES (1,'claude','s1','n1','电池掉电','问题','原因','解决','结论','电池 耗电','2026-10-03');",
        )
        .unwrap();
        (tmp, data)
    }

    fn env<'a>(
        data: &'a DataPaths,
        mem: &'a dyn Fn() -> Option<u64>,
        made: &'a dyn Fn(usize) -> Result<Box<dyn Embedder>, String>,
    ) -> Env<'a> {
        Env {
            data,
            model_spec: test_spec(),
            available_memory: mem,
            make_embedder: made,
            lower_priority: false,
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn plenty() -> Option<u64> {
        Some(64 * 1024 * 1024 * 1024)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn fake(_: usize) -> Result<Box<dyn Embedder>, String> {
        Ok(Box::new(FakeEmbedder::default()))
    }

    fn stored(data: &DataPaths) -> usize {
        Store::open_ro(&store::path(data), model::model_sha())
            .ok()
            .flatten()
            .map_or(0, |s| s.count())
    }

    // The store records the real model's SHA; tests use the fake spec's, so go through run()
    // which uses `model::model_sha()` for both. Keep them equal by reading it back.
    fn count(data: &DataPaths) -> usize {
        stored(data)
    }

    #[test]
    fn first_run_builds_everything_wanted_and_nothing_else() {
        let (_t, data) = setup();
        let cancel = AtomicBool::new(false);
        let out = run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        assert_eq!(out, Outcome::Ran);
        // 1 note + 2 titles (the excluded session has none) + 3 user/assistant messages
        assert_eq!(count(&data), 1 + 2 + 3);
        let s = state::read(&data);
        assert_eq!((s.status.as_str(), s.total, s.done), ("idle", 6, 6));
        assert_eq!(s.last_run.unwrap().new, 6);
    }

    #[test]
    fn two_files_with_the_same_session_id_do_not_make_every_run_compute_again() {
        let (_t, data) = setup();
        let conn = Connection::open(data.db()).unwrap();
        conn.execute_batch(
            "INSERT INTO sessions VALUES (4,'claude','s1','电池掉电快问题排查','2026-09-01',0);
             INSERT INTO messages VALUES (4,1,'user','同一个会话号的另一份文件里，第一条消息说的是完全不同的内容');",
        )
        .unwrap();
        drop(conn);
        let cancel = AtomicBool::new(false);
        run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        let out = run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        assert_eq!(out, Outcome::UpToDate, "the second run has nothing left");
    }

    #[test]
    fn a_damaged_vector_file_is_thrown_away_and_built_again() {
        let (_t, data) = setup();
        std::fs::create_dir_all(store::dir(&data)).unwrap();
        std::fs::write(
            store::path(&data),
            b"this is not a sqlite file at all, just some text of a decent length...",
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let out = run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        assert_eq!(out, Outcome::Ran);
        assert_eq!(count(&data), 6);
    }

    #[test]
    fn second_run_computes_nothing() {
        let (_t, data) = setup();
        let cancel = AtomicBool::new(false);
        run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        let calls = Rc::new(Cell::new(0));
        let c2 = calls.clone();
        let counting = move |_: usize| -> Result<Box<dyn Embedder>, String> {
            c2.set(c2.get() + 1);
            Ok(Box::new(FakeEmbedder::default()))
        };
        let out = run(
            &env(&data, &plenty, &counting),
            &Options::automatic(),
            &cancel,
        )
        .unwrap();
        assert_eq!(out, Outcome::UpToDate);
        assert_eq!(calls.get(), 0, "the model must not even be loaded");
        assert_eq!(state::read(&data).last_run.unwrap().unchanged, 6);
    }

    #[test]
    fn new_changed_and_deleted_rows_are_picked_up() {
        let (_t, data) = setup();
        let cancel = AtomicBool::new(false);
        run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        let conn = Connection::open(data.db()).unwrap();
        conn.execute_batch(
            "INSERT INTO messages VALUES (2,2,'assistant','用 system_profiler SPPowerDataType 可以看到充电器的功率信息');
             UPDATE messages SET text='先看 pmset -g log，再看活动监视器的能耗列，找出耗电大户。' WHERE file_id=1 AND seq=2;
             DELETE FROM messages WHERE file_id=1 AND seq=1;",
        )
        .unwrap();
        drop(conn);
        run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        let r = state::read(&data).last_run.unwrap();
        assert_eq!(
            (r.new, r.changed, r.removed, r.unchanged),
            (1, 1, 1, 4),
            "{r:?}"
        );
        assert_eq!(count(&data), 6);
    }

    #[test]
    fn a_stopped_run_continues_where_it_was() {
        let (_t, data) = setup();
        let cancel = AtomicBool::new(true); // stops before the first window
        run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        assert_eq!(count(&data), 0);
        assert_eq!(state::read(&data).done, 0);
        let go = AtomicBool::new(false);
        run(&env(&data, &plenty, &fake), &Options::automatic(), &go).unwrap();
        assert_eq!(count(&data), 6);
        // a zero budget does one thing at most: nothing, then the rest next time
        let (_t2, data2) = setup();
        let zero = Options {
            budget: Some(Duration::ZERO),
            ..Options::automatic()
        };
        run(&env(&data2, &plenty, &fake), &zero, &go).unwrap();
        assert!(count(&data2) < 6);
        run(&env(&data2, &plenty, &fake), &Options::automatic(), &go).unwrap();
        assert_eq!(count(&data2), 6);
    }

    #[test]
    fn short_of_memory_puts_the_run_off_and_counts_it() {
        let (_t, data) = setup();
        let cancel = AtomicBool::new(false);
        let low = || Some(1024 * 1024 * 1024);
        for n in 1..=2 {
            let out = run(&env(&data, &low, &fake), &Options::automatic(), &cancel).unwrap();
            assert_eq!(out, Outcome::Postponed);
            let s = state::read(&data);
            assert_eq!(
                (s.status.as_str(), s.postponed_rounds),
                ("postponed_memory", n)
            );
        }
        assert_eq!(count(&data), 0);
        run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        assert_eq!(state::read(&data).postponed_rounds, 0);
        // the "build everything now" button does not wait for memory
        let (_t2, data2) = setup();
        let out = run(&env(&data2, &low, &fake), &Options::everything(), &cancel).unwrap();
        assert_eq!(out, Outcome::Ran);
    }

    #[test]
    fn missing_or_damaged_model_pauses_without_loading_it() {
        let (_t, data) = setup();
        let cancel = AtomicBool::new(false);
        std::fs::write(model::dir(&data).join("model.onnx"), b"damaged!!!").unwrap();
        let out = run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        assert!(matches!(out, Outcome::NoModel(_)));
        assert_eq!(state::read(&data).status, "paused_model");
        model::delete(&data).unwrap();
        let out = run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        assert_eq!(out, Outcome::NoModel("missing".into()));
    }

    #[test]
    fn second_builder_finds_the_lock_taken() {
        let (_t, data) = setup();
        let held = SyncLock::acquire(&lock_path(&data), Duration::ZERO)
            .unwrap()
            .unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap(),
            Outcome::Busy
        );
        drop(held);
    }

    #[test]
    fn messages_still_in_the_write_ahead_log_are_seen() {
        let (_t, data) = setup();
        // keep a writer open so nothing is checkpointed into the main file
        let writer = Connection::open(data.db()).unwrap();
        writer
            .execute_batch("PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        writer
            .execute("INSERT INTO messages VALUES (2, 9, 'assistant', '刚刚写进日志文件、还没合并进主文件的一条新消息')", [])
            .unwrap();
        let wal = data.db().with_extension("db-wal");
        assert!(
            std::fs::metadata(&wal).map_or(0, |m| m.len()) > 0,
            "the new row should be in the WAL"
        );
        let cancel = AtomicBool::new(false);
        run(&env(&data, &plenty, &fake), &Options::automatic(), &cancel).unwrap();
        assert_eq!(
            state::read(&data).total,
            7,
            "the row only the WAL has must be counted"
        );
        drop(writer);
    }
}

// ---------------------------------------------------------------------------
// The real thing (command line and app)
// ---------------------------------------------------------------------------

/// SIGTERM sets `cancel`: a "stop" from the app ends the run after the batch
/// in hand, with everything computed so far saved.
pub fn run_real(
    data: &DataPaths,
    opts: &Options,
    cancel: &std::sync::Arc<AtomicBool>,
) -> Result<Outcome, String> {
    crate::kb::digest::stop_on_sigterm(cancel);
    // `AGENTKB_EMBED_AVAILABLE_MB` pretends the machine has that much memory to
    // spare: for trying the "short of memory" path, not for everyday use.
    let available = || {
        std::env::var("AGENTKB_EMBED_AVAILABLE_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|mb| mb * 1_048_576)
            .or_else(sys::available_memory)
    };
    let make = |threads: usize| -> Result<Box<dyn Embedder>, String> {
        let dir = model::dir(data);
        super::embed::OrtEmbedder::load(&dir, threads).map(|e| Box::new(e) as Box<dyn Embedder>)
    };
    run(
        &Env {
            data,
            model_spec: &model::SPEC,
            available_memory: &available,
            make_embedder: &make,
            lower_priority: opts.polite,
        },
        opts,
        cancel,
    )
}

/// Ask the running builder to stop. True when one was running.
pub fn request_stop(data: &DataPaths) -> bool {
    #[cfg(unix)]
    {
        let Some(info) = crate::kb::lock::held_by_live_process(&lock_path(data)) else {
            return false;
        };
        let Ok(pid) = i32::try_from(info.pid) else {
            return false;
        };
        // SAFETY: plain syscall; the pid belongs to a live process that holds our lock.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(pid, libc::SIGTERM) == 0
        }
    }
    #[cfg(not(unix))]
    {
        let _ = data;
        false
    }
}

/// A one-sentence test: load the model, embed, report the time.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TestReport {
    pub ok: bool,
    pub seconds: f64,
    pub dim: usize,
    pub message: String,
}

pub fn test_model(data: &DataPaths) -> TestReport {
    let started = Instant::now();
    match model::check(data, &model::SPEC) {
        model::ModelStatus::Ready => {}
        model::ModelStatus::Missing => {
            return TestReport {
                ok: false,
                seconds: 0.0,
                dim: 0,
                message: "model not downloaded".into(),
            }
        }
        model::ModelStatus::Corrupt { file, reason } => {
            return TestReport {
                ok: false,
                seconds: 0.0,
                dim: 0,
                message: format!("{file}: {reason}"),
            }
        }
    }
    let result = super::embed::OrtEmbedder::load(&model::dir(data), 4).and_then(|mut e| {
        let a = e.embed_query("续航变短，电池掉电快")?;
        let b = e.embed_query("battery drains quickly")?;
        let c = e.embed_query("今天晚饭吃什么")?;
        let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>();
        if a.len() != super::DIM || (dot(&a, &a) - 1.0).abs() > 0.01 {
            return Err("the model returned an unexpected vector".to_string());
        }
        if dot(&a, &b) <= dot(&a, &c) {
            return Err(
                "the model does not tell related sentences from unrelated ones".to_string(),
            );
        }
        Ok(a.len())
    });
    let seconds = started.elapsed().as_secs_f64();
    match result {
        Ok(dim) => TestReport {
            ok: true,
            seconds,
            dim,
            message: String::new(),
        },
        Err(message) => TestReport {
            ok: false,
            seconds,
            dim: 0,
            message,
        },
    }
}
