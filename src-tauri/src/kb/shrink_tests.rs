//! Old backups stored compressed, end to end on throw-away libraries.

// Helpers are methods for readability at the call sites.
#![allow(clippy::unused_self, clippy::case_sensitive_file_extension_comparisons)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use super::config::{PackConfig, SourceRoots};
use super::db::{self, FileRow};
use super::fsutil;
use super::paths::DataPaths;
use super::progress::ProgressSink;
use super::search::{self, SearchParams, ShowParams};
use super::shrink;
use super::sync::{run_sync, KbEnv, Summary};

const S1: &str = "bbbbbbbb-0000-0000-0000-000000000001";
const S2: &str = "bbbbbbbb-0000-0000-0000-000000000002";
const S3: &str = "bbbbbbbb-0000-0000-0000-000000000003";
const CODEX_ID: &str = "019e0000-bbbb-7000-8000-000000000001";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Puts every test hook back, also when a test fails halfway.
struct Hooks;

impl Hooks {
    fn new() -> Self {
        Self::clear();
        Self
    }

    fn clear() {
        use std::sync::PoisonError;
        *shrink::DIE_AT
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        *shrink::ON_STEP
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        fsutil::REFUSE_REMOVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        *search::BEFORE_READ
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn die_at(&self, step: Option<&'static str>) {
        *shrink::DIE_AT.lock().unwrap() = step;
    }

    fn refuse_remove(&self, names: &[&str]) {
        let mut list = fsutil::REFUSE_REMOVE.lock().unwrap();
        list.clear();
        list.extend(names.iter().map(|n| (*n).to_string()));
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        Self::clear();
    }
}

struct Sb {
    dir: tempfile::TempDir,
    env: KbEnv,
    claude: PathBuf,
    codex: PathBuf,
}

fn sandbox(pack: bool) -> Sb {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join(".claude");
    let codex = dir.path().join(".codex");
    fs::create_dir_all(claude.join("projects")).unwrap();
    fs::create_dir_all(codex.join("sessions")).unwrap();
    let env = KbEnv {
        data: DataPaths::new(dir.path().join("AgentKB")),
        roots: SourceRoots {
            claude: Some(claude.clone()),
            codex: Some(codex.clone()),
        },
        exclude_prefixes: vec![],
        exclude_cwd_prefixes: vec![],
        mirror: None,
        progress: ProgressSink::default(),
        force_reindex: false,
        pack: PackConfig {
            enabled: pack,
            after_days: 30,
        },
    };
    Sb {
        dir,
        env,
        claude,
        codex,
    }
}

/// A Claude session from January: some short turns and one user message
/// longer than the index keeps, so showing it in full reads the backup.
fn claude_text(id: &str, turns: usize, tag: &str) -> String {
    let mut out = String::new();
    for i in 0..turns {
        out.push_str(&format!(
            r#"{{"type":"user","uuid":"{id}-u{i}","timestamp":"2026-01-01T00:{i:02}:01Z","cwd":"/Users/me/proj","message":{{"role":"user","content":"{tag} question {i} about the deadlock"}}}}
{{"type":"assistant","uuid":"{id}-a{i}","timestamp":"2026-01-01T00:{i:02}:02Z","message":{{"role":"assistant","content":[{{"type":"text","text":"{tag} answer {i}"}}]}}}}
"#
        ));
    }
    let long = "lock ".repeat(5000);
    out.push_str(&format!(
        r#"{{"type":"user","uuid":"{id}-long","timestamp":"2026-01-01T01:00:00Z","cwd":"/Users/me/proj","message":{{"role":"user","content":"{tag} {long}the end"}}}}
"#
    ));
    out
}

fn line(id: &str, n: u32, tag: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"{id}-n{n}","timestamp":"2026-11-20T00:00:{n:02}Z","cwd":"/Users/me/proj","message":{{"role":"user","content":"{tag} new message {n}"}}}}
"#
    )
}

fn set_age(path: &Path, days: u32) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - DAY * days).unwrap();
}

impl Sb {
    fn claude_file(&self, id: &str) -> PathBuf {
        self.claude
            .join("projects/-Users-me-proj")
            .join(format!("{id}.jsonl"))
    }

    fn codex_file(&self) -> PathBuf {
        self.codex.join(format!(
            "sessions/2026/01/02/rollout-2026-01-02T10-00-00-{CODEX_ID}.jsonl"
        ))
    }

    /// Write a session that was last touched `days` ago.
    fn write(&self, path: &Path, text: &str, days: u32) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        set_age(path, days);
    }

    fn append(&self, path: &Path, text: &str) {
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
        drop(f);
        set_age(path, 0);
    }

    fn sync(&self) -> Summary {
        run_sync(&self.env, "test").unwrap()
    }

    fn conn(&self) -> Connection {
        db::open(&self.env.data.db()).unwrap()
    }

    fn row(&self, source_path: &Path) -> FileRow {
        let want = source_path.to_string_lossy();
        db::load_files(&self.conn())
            .unwrap()
            .into_iter()
            .find(|r| r.source_path == want || r.source_path == format!("{want}.zst"))
            .unwrap()
    }

    fn raw_files(&self) -> Vec<String> {
        let mut out: Vec<String> = fsutil::walk_files(&self.env.data.raw())
            .into_iter()
            .map(|(rel, _)| rel)
            .collect();
        out.sort();
        out
    }

    /// The plain text the backup of `source_path` holds.
    fn backup_text(&self, source_path: &Path) -> Vec<u8> {
        let row = self.row(source_path);
        let path = self.env.data.mirror_path(&row.mirror_rel).unwrap();
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut shrink::open_plain(&path).unwrap(), &mut out).unwrap();
        out.truncate(usize::try_from(row.mirror_offset).unwrap());
        out
    }

    fn show(&self, id: &str) -> Vec<String> {
        search::show_in(
            &self.env.data,
            &ShowParams {
                session: id.into(),
                full: Some(true),
                head: Some(100),
                max_chars: Some(100_000),
                ..Default::default()
            },
        )
        .unwrap()
        .messages
        .into_iter()
        .map(|m| m.text)
        .collect()
    }

    fn search(&self, query: &str) -> serde_json::Value {
        let response = search::search_in(
            &self.env.data,
            &SearchParams {
                query: query.into(),
                ..SearchParams::default()
            },
        )
        .unwrap();
        let mut results = serde_json::to_value(response).unwrap()["results"].clone();
        // Where the backup is stored is the one thing allowed to change
        // (`.jsonl` -> `.jsonl.packed.zst`); checked separately.
        for result in results.as_array_mut().unwrap() {
            result["paths"]["mirror"] = serde_json::Value::Null;
        }
        results
    }

    fn messages(&self) -> Vec<(i64, i64, String, i64)> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT file_id, seq, kind, byte_off FROM messages ORDER BY file_id, seq")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn pass(&self) -> shrink::PassSummary {
        let log = super::log::SyncLog::open(&self.env.data.logs());
        shrink::run_pass(
            &self.conn(),
            &self.env.data,
            &self.env.pack,
            &log,
            None,
            &ProgressSink::default(),
        )
        .unwrap()
    }
}

fn ends_in_full(texts: &[String]) -> bool {
    texts
        .iter()
        .any(|t| t.ends_with("the end") && t.len() > 20_000)
}

/// S1 is old, S2 is old by its messages but its file was touched lately,
/// S3 is old but too small.
fn three_sessions(sb: &Sb) {
    sb.write(&sb.claude_file(S1), &claude_text(S1, 20, "alpha"), 60);
    sb.write(&sb.claude_file(S2), &claude_text(S2, 20, "beta"), 1);
    sb.write(&sb.claude_file(S3), &line(S3, 1, "gamma"), 60);
}

#[test]
fn old_backups_are_compressed_and_everything_reads_the_same() {
    let _hooks = Hooks::new();
    let mut sb = sandbox(false);
    three_sessions(&sb);
    assert_eq!(sb.sync().status, "ok");
    let (search_before, show_before, messages_before) =
        (sb.search("deadlock"), sb.show(S1), sb.messages());
    assert!(ends_in_full(&show_before));
    let original = fs::read(sb.claude_file(S1)).unwrap();

    sb.env.pack.enabled = true;
    let summary = sb.sync();
    assert_eq!(summary.status, "ok", "{:?}", summary.errors);
    let pass = summary.shrink.unwrap();
    assert_eq!((pass.packed_files, pass.failed_files), (1, 0));
    assert!(pass.packed_bytes * 2 < pass.plain_bytes, "{pass:?}");

    let row = sb.row(&sb.claude_file(S1));
    assert!(row.packed && row.mirror_rel.ends_with(".jsonl.packed.zst"));
    assert!(!sb.row(&sb.claude_file(S2)).packed, "file touched lately");
    assert!(!sb.row(&sb.claude_file(S3)).packed, "too small");
    assert_eq!(
        sb.raw_files()
            .iter()
            .filter(|r| r.contains(S1))
            .collect::<Vec<_>>(),
        [&row.mirror_rel],
        "the plain backup is gone"
    );
    assert_eq!(
        sb.backup_text(&sb.claude_file(S1)),
        original,
        "byte for byte"
    );
    assert_eq!(sb.search("deadlock"), search_before);
    assert_eq!(sb.show(S1), show_before);
    assert_eq!(sb.messages(), messages_before);

    // Rebuilding the index from the compressed backup gives the same rows.
    sb.env.force_reindex = true;
    assert_eq!(sb.sync().status, "ok");
    sb.env.force_reindex = false;
    assert_eq!(sb.messages(), messages_before);
    assert_eq!(sb.show(S1), show_before);

    // Nothing more to do the next time.
    assert_eq!(sb.sync().shrink.unwrap().packed_files, 0);
    let stats = shrink::stats(&sb.conn(), &sb.env.data).unwrap();
    assert_eq!(stats.files, 1);
    assert_eq!(stats.plain_bytes, original.len() as u64);
}

#[test]
fn every_step_of_compressing_can_be_cut_short() {
    for step in ["pack:after_tmp", "pack:after_rename", "pack:after_db"] {
        let hooks = Hooks::new();
        let sb = sandbox(false);
        three_sessions(&sb);
        sb.sync();
        let original = fs::read(sb.claude_file(S1)).unwrap();
        let show_before = sb.show(S1);

        hooks.die_at(Some(step));
        let mut sb = sb;
        sb.env.pack.enabled = true;
        let cut = sb.pass();
        assert_eq!(cut.failed_files, 1, "{step}");
        // Whatever the row points to is complete.
        assert_eq!(sb.backup_text(&sb.claude_file(S1)), original, "{step}");
        assert_eq!(sb.show(S1), show_before, "{step}");

        hooks.die_at(None);
        let summary = sb.sync();
        assert_eq!(summary.status, "ok", "{step}: {:?}", summary.errors);
        let row = sb.row(&sb.claude_file(S1));
        assert!(row.packed, "{step}");
        assert_eq!(sb.backup_text(&sb.claude_file(S1)), original, "{step}");
        let left: Vec<String> = sb
            .raw_files()
            .into_iter()
            .filter(|r| r.contains(S1))
            .collect();
        assert_eq!(
            left,
            std::slice::from_ref(&row.mirror_rel),
            "{step}: nothing left over"
        );
        assert_eq!(sb.show(S1), show_before, "{step}");
    }
}

#[test]
fn new_messages_turn_the_backup_back_into_plain_text_then_append() {
    let _hooks = Hooks::new();
    let sb = sandbox(true);
    three_sessions(&sb);
    sb.sync();
    assert!(sb.row(&sb.claude_file(S1)).packed);

    sb.append(&sb.claude_file(S1), &line(S1, 1, "resumed"));
    let summary = sb.sync();
    assert_eq!(summary.status, "ok", "{:?}", summary.errors);
    assert_eq!(summary.unpacked_files, 1);
    assert_eq!(summary.rewritten_files, 0, "appended, not rewritten");
    let row = sb.row(&sb.claude_file(S1));
    assert!(!row.packed && row.mirror_rel.ends_with(".jsonl"));
    assert_eq!(
        sb.backup_text(&sb.claude_file(S1)),
        fs::read(sb.claude_file(S1)).unwrap()
    );
    let left: Vec<String> = sb
        .raw_files()
        .into_iter()
        .filter(|r| r.contains(S1))
        .collect();
    assert_eq!(left, [row.mirror_rel], "no .packed.zst, no .prev-*");
    let shown = sb.show(S1);
    assert!(ends_in_full(&shown), "old content");
    assert!(
        shown.iter().any(|t| t.contains("resumed new message 1")),
        "new content"
    );
}

#[test]
fn every_step_of_unpacking_can_be_cut_short() {
    for step in ["unpack:after_tmp", "unpack:after_rename", "unpack:after_db"] {
        let hooks = Hooks::new();
        let sb = sandbox(true);
        three_sessions(&sb);
        sb.sync();
        let old = fs::read(sb.claude_file(S1)).unwrap();
        sb.append(&sb.claude_file(S1), &line(S1, 1, "resumed"));

        hooks.die_at(Some(step));
        let cut = sb.sync();
        assert_eq!(cut.status, "partial", "{step}");
        assert_eq!(
            sb.backup_text(&sb.claude_file(S1)),
            old,
            "{step}: still the old content"
        );

        hooks.die_at(None);
        let summary = sb.sync();
        assert_eq!(summary.status, "ok", "{step}: {:?}", summary.errors);
        let row = sb.row(&sb.claude_file(S1));
        assert!(!row.packed, "{step}");
        assert_eq!(
            sb.backup_text(&sb.claude_file(S1)),
            fs::read(sb.claude_file(S1)).unwrap(),
            "{step}"
        );
        let left: Vec<String> = sb
            .raw_files()
            .into_iter()
            .filter(|r| r.contains(S1))
            .collect();
        assert_eq!(left, [row.mirror_rel], "{step}: nothing left over");
        assert!(sb
            .show(S1)
            .iter()
            .any(|t| t.contains("resumed new message 1")));
    }
}

#[test]
fn the_source_growing_while_it_is_compressed_loses_nothing() {
    let _hooks = Hooks::new();
    let mut sb = sandbox(false);
    three_sessions(&sb);
    sb.sync();
    let source = sb.claude_file(S1);
    let appended = source.clone();
    *shrink::ON_STEP.lock().unwrap() = Some(Box::new(move |step| {
        if step == "pack:after_tmp" {
            let mut f = fs::OpenOptions::new().append(true).open(&appended).unwrap();
            f.write_all(line(S1, 2, "meanwhile").as_bytes()).unwrap();
        }
    }));
    sb.env.pack.enabled = true;
    assert_eq!(sb.pass().packed_files, 1);
    *shrink::ON_STEP.lock().unwrap() = None;
    set_age(&source, 0);

    let summary = sb.sync();
    assert_eq!(summary.status, "ok", "{:?}", summary.errors);
    assert_eq!(summary.unpacked_files, 1);
    assert_eq!(sb.backup_text(&source), fs::read(&source).unwrap());
    assert!(sb
        .show(S1)
        .iter()
        .any(|t| t.contains("meanwhile new message 2")));
}

#[test]
fn codex_compressing_the_source_after_us_leaves_one_backup() {
    let _hooks = Hooks::new();
    let sb = sandbox(true);
    let meta = format!(
        r#"{{"timestamp":"2026-01-02T10:00:00Z","type":"session_meta","payload":{{"id":"{CODEX_ID}","cwd":"/Users/me/app","originator":"codex-tui","source":"cli"}}}}"#
    );
    let long = "retry ".repeat(4000);
    let message = format!(
        r#"{{"timestamp":"2026-01-02T10:00:01Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{long}the end"}}]}}}}"#
    );
    let text = format!("{meta}\n{message}\n");
    sb.write(&sb.codex_file(), &text, 60);
    sb.sync();
    assert!(sb.row(&sb.codex_file()).packed);

    // Codex compresses the rollout in place.
    let zst = PathBuf::from(format!("{}.zst", sb.codex_file().display()));
    fs::write(&zst, zstd::encode_all(text.as_bytes(), 3).unwrap()).unwrap();
    fs::remove_file(sb.codex_file()).unwrap();
    let summary = sb.sync();
    assert_eq!(summary.status, "ok", "{:?}", summary.errors);
    let row = sb.row(&sb.codex_file());
    assert!(row.is_compressed && !row.packed);
    assert!(row.mirror_rel.ends_with(".jsonl.zst"));
    assert_eq!(summary.duplicates_removed, 1, "our .packed.zst went");
    let left: Vec<String> = sb
        .raw_files()
        .into_iter()
        .filter(|r| r.contains(CODEX_ID))
        .collect();
    assert_eq!(left, [row.mirror_rel]);
    assert!(ends_in_full(&sb.show(CODEX_ID)));
}

#[test]
fn a_file_that_cannot_be_removed_stays_and_goes_next_time() {
    let hooks = Hooks::new();
    let sb = sandbox(true);
    three_sessions(&sb);
    let plain_name = format!("{S1}.jsonl");
    hooks.refuse_remove(&[&plain_name]);
    let summary = sb.sync();
    assert_eq!(summary.status, "ok", "{:?}", summary.errors);
    assert_eq!(summary.shrink.unwrap().left_behind, 1);
    assert!(sb.row(&sb.claude_file(S1)).packed);
    assert_eq!(
        sb.raw_files().iter().filter(|r| r.contains(S1)).count(),
        2,
        "the plain one is still there"
    );

    hooks.refuse_remove(&[]);
    let summary = sb.sync();
    assert_eq!(summary.duplicates_removed, 1);
    assert_eq!(sb.raw_files().iter().filter(|r| r.contains(S1)).count(), 1);
}

#[test]
fn a_leftover_at_the_unpack_target_never_blocks_for_good() {
    let hooks = Hooks::new();
    let sb = sandbox(true);
    three_sessions(&sb);
    let plain_name = format!("{S1}.jsonl");
    hooks.refuse_remove(&[&plain_name]);
    sb.sync();
    // The plain backup is still there (Windows would not let it go), and new
    // messages arrive: unpacking finds that file in its way and cannot remove it.
    sb.append(&sb.claude_file(S1), &line(S1, 1, "resumed"));
    let blocked = sb.sync();
    assert_eq!(
        blocked.status, "partial",
        "this file waits, the collection ends"
    );
    assert!(
        sb.row(&sb.claude_file(S1)).packed,
        "still the complete old backup"
    );

    hooks.refuse_remove(&[]);
    let summary = sb.sync();
    assert_eq!(summary.status, "ok", "{:?}", summary.errors);
    assert_eq!(
        sb.backup_text(&sb.claude_file(S1)),
        fs::read(sb.claude_file(S1)).unwrap()
    );

    // A leftover that is not part of the content is set aside, not removed.
    let sb = sandbox(true);
    three_sessions(&sb);
    sb.sync();
    let plain = sb
        .env
        .data
        .raw()
        .join(format!("claude/projects/-Users-me-proj/{S1}.jsonl"));
    fs::write(&plain, b"something else\n").unwrap();
    sb.append(&sb.claude_file(S1), &line(S1, 1, "resumed"));
    let summary = sb.sync();
    assert_eq!(summary.status, "ok", "{:?}", summary.errors);
    assert_eq!(
        sb.backup_text(&sb.claude_file(S1)),
        fs::read(sb.claude_file(S1)).unwrap()
    );
    let aside: Vec<PathBuf> = fs::read_dir(plain.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(".prev-"))
        .collect();
    assert_eq!(aside.len(), 1);
    assert_eq!(fs::read(&aside[0]).unwrap(), b"something else\n");
}

#[test]
fn switched_off_nothing_new_is_compressed_and_old_ones_stay_readable() {
    let _hooks = Hooks::new();
    let mut sb = sandbox(true);
    three_sessions(&sb);
    sb.sync();
    assert!(sb.row(&sb.claude_file(S1)).packed);

    sb.env.pack.enabled = false;
    const S4: &str = "bbbbbbbb-0000-0000-0000-000000000004";
    sb.write(&sb.claude_file(S4), &claude_text(S4, 20, "delta"), 60);
    let summary = sb.sync();
    assert!(summary.shrink.is_none());
    assert!(!sb.row(&sb.claude_file(S4)).packed);
    assert!(sb.row(&sb.claude_file(S1)).packed, "left as it is");
    assert!(ends_in_full(&sb.show(S1)));
}

#[test]
fn a_reader_that_finds_the_backup_replaced_reads_the_row_again() {
    let _hooks = Hooks::new();
    let sb = sandbox(false);
    three_sessions(&sb);
    sb.sync();
    let before = sb.show(S1);

    // Between reading the row and opening the file, a collection compresses it.
    let (db_path, data, row) = (
        sb.env.data.db(),
        sb.env.data.clone(),
        sb.row(&sb.claude_file(S1)),
    );
    *search::BEFORE_READ.lock().unwrap() = Some(Box::new(move || {
        let conn = db::open(&db_path).unwrap();
        let now = db::load_files(&conn)
            .unwrap()
            .into_iter()
            .find(|r| r.id == row.id)
            .unwrap();
        if !now.packed {
            shrink::pack_one(&conn, &data, &now).unwrap();
        }
    }));
    assert_eq!(sb.show(S1), before);
    assert!(sb.row(&sb.claude_file(S1)).packed);

    // And the other way: unpacked while being read.
    let (db_path, data, row) = (
        sb.env.data.db(),
        sb.env.data.clone(),
        sb.row(&sb.claude_file(S1)),
    );
    *search::BEFORE_READ.lock().unwrap() = Some(Box::new(move || {
        let conn = db::open(&db_path).unwrap();
        let now = db::load_files(&conn)
            .unwrap()
            .into_iter()
            .find(|r| r.id == row.id)
            .unwrap();
        if now.packed {
            shrink::unpack(&conn, &data, &now).unwrap();
        }
    }));
    assert_eq!(sb.show(S1), before);
    assert!(!sb.row(&sb.claude_file(S1)).packed);
}

#[test]
fn the_app_opens_a_compressed_session_under_its_old_or_new_name() {
    let _hooks = Hooks::new();
    let sb = sandbox(true);
    three_sessions(&sb);
    sb.sync();
    let row = sb.row(&sb.claude_file(S1));
    assert!(row.packed);
    let packed = sb.env.data.mirror_path(&row.mirror_rel).unwrap();
    let old_name = sb
        .env
        .data
        .mirror_path(&format!("claude/projects/-Users-me-proj/{S1}.jsonl"))
        .unwrap();
    let original = fs::read(sb.claude_file(S1)).unwrap();
    for path in [&packed, &old_name] {
        let bytes = crate::commands::session::session_bytes(path).unwrap();
        assert_eq!(&*bytes, original.as_slice(), "{}", path.display());
    }
}

#[test]
fn leftover_temp_files_are_removed() {
    let _hooks = Hooks::new();
    let sb = sandbox(true);
    three_sessions(&sb);
    sb.sync();
    let dir = sb.env.data.raw().join("claude/projects/-Users-me-proj");
    fs::write(dir.join(format!("{S2}.jsonl.packed.zst.tmp-4242")), b"x").unwrap();
    fs::write(dir.join("keep.tmp-notapid"), b"x").unwrap();
    let summary = sb.sync();
    assert_eq!(summary.shrink.unwrap().temp_files_removed, 1);
    assert!(dir.join("keep.tmp-notapid").exists());
}

#[test]
fn candidates_go_by_the_last_message_and_the_file_time() {
    let _hooks = Hooks::new();
    let sb = sandbox(false);
    three_sessions(&sb);
    // A session with recent messages in an old file is not due.
    const S5: &str = "bbbbbbbb-0000-0000-0000-000000000005";
    let mut text = claude_text(S5, 20, "epsilon");
    text.push_str(&line(S5, 1, "recent"));
    sb.write(&sb.claude_file(S5), &text, 60);
    sb.sync();
    let now = chrono::Utc::now();
    let due: Vec<String> = shrink::candidates(&sb.conn(), now, 30)
        .unwrap()
        .into_iter()
        .map(|r| r.mirror_rel)
        .collect();
    assert_eq!(due.len(), 1);
    assert!(due[0].contains(S1));
    let later = now + chrono::Duration::days(400);
    assert_eq!(
        shrink::candidates(&sb.conn(), later, 30).unwrap().len(),
        3,
        "S1, S2, S5"
    );
}

#[test]
fn moving_the_data_folder_keeps_compressed_backups_readable() {
    let _hooks = Hooks::new();
    let mut sb = sandbox(true);
    three_sessions(&sb);
    sb.sync();
    assert!(sb.row(&sb.claude_file(S1)).packed);
    let (search_before, show_before) = (sb.search("deadlock"), sb.show(S1));
    let new = sb.dir.path().join("Moved");
    super::migrate_dir::migrate_data(sb.env.data.root(), &new, &ProgressSink::default()).unwrap();
    sb.env.data = DataPaths::new(&new);
    assert_eq!(sb.search("deadlock"), search_before);
    assert_eq!(sb.show(S1), show_before);
    assert_eq!(sb.sync().status, "ok");
}

/// What `backup.rs` leaves on the disk: the snapshot and a copy of `raw/`.
fn copy_to_disk(sb: &Sb, folder: &Path, snapshot: bool) {
    if snapshot {
        fs::create_dir_all(folder).unwrap();
        super::backup::online_backup(&sb.conn(), &folder.join("kb.db")).unwrap();
    }
    for (rel, _) in fsutil::walk_files(&sb.env.data.raw()) {
        let to = folder.join("raw").join(&rel);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(sb.env.data.raw().join(&rel), to).unwrap();
    }
}

fn restore_into(sb_root: &Path, name: &str, folder: &Path) -> (Sb, super::restore::RestoreReport) {
    let home = sb_root.join(name);
    let mut target = sandbox(true);
    target.claude = home.join(".claude");
    fs::create_dir_all(target.claude.join("projects")).unwrap();
    target.env.data = DataPaths::new(home.join("AgentKB"));
    target.env.roots.claude = Some(target.claude.clone());
    let report = super::restore::restore(
        &target.env,
        folder,
        &super::transfer::Cancel::new(),
        &ProgressSink::default(),
    )
    .unwrap();
    (target, report)
}

#[test]
fn the_disk_backup_follows_and_a_restore_finds_the_compressed_backup() {
    let _hooks = Hooks::new();
    let mut sb = sandbox(false);
    three_sessions(&sb);
    sb.sync();
    let folder = sb.dir.path().join("disk/AgentKB-backup/this-mac");
    // The day's snapshot names the plain backup.
    copy_to_disk(&sb, &folder, true);
    let show_before = sb.show(S1);

    // Later that day the backup is compressed and the next backup carries
    // it; the duplicate cleanup on the disk removes the plain copy.
    sb.env.pack.enabled = true;
    sb.sync();
    copy_to_disk(&sb, &folder, false);
    let log = super::log::SyncLog::open(&sb.env.data.logs());
    let removed = super::dedupe::clean_disk(&sb.env.data, &folder.join("raw"), &log, |_| Ok(()));
    assert_eq!(removed.files, 1);
    let plain_rel = format!("claude/projects/-Users-me-proj/{S1}.jsonl");
    assert!(!folder.join("raw").join(&plain_rel).exists());
    assert!(folder
        .join("raw")
        .join(format!("{plain_rel}.packed.zst"))
        .exists());

    let (target, report) = restore_into(sb.dir.path(), "other", &folder);
    assert_eq!((report.missing_files, report.repointed_files), (0, 1));
    let row = target.row(&target.claude_file(S1));
    assert!(row.packed, "pointed at the compressed backup");
    assert!(row.source_path.ends_with(".jsonl"), "{}", row.source_path);
    assert_eq!(target.show(S1), show_before);

    // A snapshot taken after the compression restores as it is, and the
    // session's source is placed where this computer keeps its sessions.
    let folder2 = sb.dir.path().join("disk2/AgentKB-backup/this-mac");
    copy_to_disk(&sb, &folder2, true);
    let (target, report) = restore_into(sb.dir.path(), "third", &folder2);
    assert_eq!((report.missing_files, report.repointed_files), (0, 0));
    let row = target.row(&target.claude_file(S1));
    assert!(row.packed);
    assert_eq!(
        row.source_path,
        target.claude_file(S1).to_string_lossy(),
        "rehomed to the plain source name"
    );
    assert_eq!(target.show(S1), show_before);
    let again = target.sync();
    assert_eq!(
        again.status, "ok",
        "collecting again works: {:?}",
        again.errors
    );
}
