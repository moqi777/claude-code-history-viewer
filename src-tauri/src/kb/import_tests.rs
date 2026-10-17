//! Export and import end to end, on throw-away libraries in temp folders.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::config::KbConfig;
use super::export::{self, ExportReport};
use super::import::{self, Mode, Options};
use super::lock::SyncLock;
use super::notes::testing::sample;
use super::progress::ProgressSink;
use super::search::{search_in, SearchParams};
use super::sync::{run_sync, KbEnv, Summary};
use super::transfer::{Cancel, TransferError};
use super::{db, fsutil};

/// One machine: a home folder with `.claude`, `.codex` and `AgentKB`.
#[derive(Clone)]
struct Lib {
    home: PathBuf,
    claude: PathBuf,
    config_file: PathBuf,
}

impl Lib {
    fn new(root: &Path, name: &str) -> Self {
        let home = root.join(name);
        let claude = home.join(".claude");
        fs::create_dir_all(claude.join("projects")).unwrap();
        fs::create_dir_all(home.join(".codex").join("sessions")).unwrap();
        let lib = Self {
            config_file: home.join(".config").join("agent-kb").join("config.json"),
            home,
            claude,
        };
        lib.enter();
        lib
    }

    /// Make this machine "the" machine for `home_dir()`.
    fn enter(&self) {
        std::env::set_var("CCHV_TEST_HOME", &self.home);
    }

    fn config(&self) -> KbConfig {
        self.enter();
        let mut config = KbConfig::default();
        config.data_dir = self.home.join("AgentKB");
        config.sources.claude.root = Some(self.claude.clone());
        config.sources.codex.root = Some(self.home.join(".codex"));
        config
    }

    fn env(&self) -> KbEnv {
        KbEnv::from_config(&self.config())
    }

    fn session_file(&self, id: &str) -> PathBuf {
        self.claude
            .join("projects")
            .join("-Users-me-proj")
            .join(format!("{id}.jsonl"))
    }

    fn text(id: &str, pairs: usize, tag: &str) -> String {
        let mut out = String::new();
        for i in 0..pairs {
            out.push_str(&format!(
                r#"{{"type":"user","uuid":"{id}-u{i}","timestamp":"2026-01-01T00:{i:02}:01Z","cwd":"/Users/me/proj","message":{{"role":"user","content":"{tag} deadlock question {i}"}}}}
{{"type":"assistant","uuid":"{id}-a{i}","timestamp":"2026-01-01T00:{i:02}:02Z","message":{{"role":"assistant","content":[{{"type":"text","text":"{tag} answer {i} about the lock"}}]}}}}
"#
            ));
        }
        out
    }

    fn write_session(&self, id: &str, pairs: usize, tag: &str) {
        let path = self.session_file(id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, Self::text(id, pairs, tag)).unwrap();
    }

    fn sync(&self) -> Summary {
        self.enter();
        run_sync(&self.env(), "test").unwrap()
    }

    fn note(&self, id: &str, entries: usize, digested_at: &str) {
        let env = self.env();
        let mut file = sample("claude", id, entries);
        file.digested_at = digested_at.into();
        let path = env.data.notes_file("claude", id).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    }

    fn conn(&self) -> Connection {
        db::open(&self.env().data.db()).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn export(&self, to: &Path) -> ExportReport {
        self.enter();
        export::export(
            &self.env(),
            &self.config(),
            to,
            false,
            true,
            &Cancel::new(),
            &ProgressSink::default(),
        )
        .unwrap()
    }

    fn opts(&self, write_config: bool) -> Options {
        Options {
            adopt_settings: false,
            write_config,
            config_exists: self.config_file.is_file(),
        }
    }

    fn import(&self, package: &Path) -> Result<import::ImportReport, TransferError> {
        self.enter();
        let config = if self.config_file.is_file() {
            KbConfig::load_from(&self.config_file).unwrap()
        } else {
            self.config()
        };
        import::import(
            &self.env(),
            &config,
            &self.opts(true),
            &self.config_file,
            package,
            &Cancel::new(),
            &ProgressSink::default(),
        )
    }

    fn data_tree(&self) -> BTreeMap<String, String> {
        tree(self.env().data.root(), &["kb.db", "kb.db-wal", "kb.db-shm"])
    }
}

/// Every file under `root` (skipping top-level names in `skip`) with its hash.
fn tree(root: &Path, skip: &[&str]) -> BTreeMap<String, String> {
    fsutil::walk_files(root)
        .into_iter()
        .filter(|(rel, _)| !skip.contains(&rel.as_str()) && !rel.starts_with("logs/"))
        .filter(|(rel, _)| !rel.contains("kb.db.") && !rel.starts_with(".import-"))
        .map(|(rel, _)| {
            let hash = fsutil::sha256_file(&root.join(&rel)).unwrap();
            (rel, hash)
        })
        .collect()
}

fn ids(lib: &Lib, query: &str) -> Vec<String> {
    lib.enter();
    let response = search_in(
        &lib.env().data,
        &SearchParams {
            query: query.into(),
            ..SearchParams::default()
        },
    )
    .unwrap();
    serde_json::to_value(&response).unwrap()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["session"]["session_id"].as_str().unwrap().to_string())
        .collect()
}

const S1: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const S2: &str = "aaaaaaaa-0000-0000-0000-000000000002";
const S3: &str = "aaaaaaaa-0000-0000-0000-000000000003";

fn library_a(root: &Path) -> Lib {
    let a = Lib::new(root, "alice");
    a.write_session(S1, 3, "alpha");
    a.write_session(S2, 5, "beta");
    a.write_session(S3, 2, "gamma");
    assert_eq!(a.sync().status, "ok");
    a.note(S1, 2, "2026-10-16T03:00:00Z");
    assert_eq!(a.sync().status, "ok");
    a
}

#[test]
fn an_exported_library_imports_into_an_empty_one_with_the_same_numbers() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    let exported = a.export(&package);
    assert_eq!((exported.sessions, exported.notes), (3, 2));
    assert_eq!(fs::metadata(&package).unwrap().len(), exported.file_bytes);
    let manifest = super::pack::read_manifest(&package).unwrap();
    assert_eq!(manifest.counts.raw_files, 3);
    // no leftovers in the exporter's data folder
    assert!(tree(a.env().data.root(), &[])
        .keys()
        .all(|k| !k.starts_with(".export-")));

    // another machine, another user name
    let b = Lib::new(root.path(), "bob");
    let before = ids(&a, "deadlock");
    let report = b.import(&package).unwrap();
    assert_eq!(report.mode, Mode::Empty);
    assert!(report.all_match, "{:?}", report.checks);
    assert_eq!(b.count("SELECT COUNT(*) FROM sessions"), 3);
    assert_eq!(b.count("SELECT COUNT(*) FROM notes"), 2);

    // the same files, byte for byte
    let ta = tree(a.env().data.root(), &["kb.db", "kb.db-wal", "kb.db-shm"]);
    let tb = b.data_tree();
    let only = |t: &BTreeMap<String, String>| -> BTreeMap<String, String> {
        t.iter()
            .filter(|(k, _)| k.starts_with("raw/") || k.starts_with("notes/"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    assert_eq!(only(&ta), only(&tb));

    // notes keep their row order, so equal scores sort the same way
    let note_order = |l: &Lib| -> Vec<(String, String)> {
        let conn = l.conn();
        let mut stmt = conn
            .prepare("SELECT session_id, entry_id FROM notes ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(note_order(&a), note_order(&b));

    // the search answers the same
    assert_eq!(ids(&b, "deadlock"), before);
    assert_ne!(before.len(), 0);

    // sessions from elsewhere are not "deleted", and cannot be resumed
    let kind: String = b
        .conn()
        .query_row("SELECT origin FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kind, super::transfer::device_name());
    let listed =
        super::status::list_sessions(&b.env(), &super::status::SessionFilter::default()).unwrap();
    assert_eq!(listed.len(), 3);
    assert!(listed.iter().all(|s| s.source_status == "elsewhere"));
    // a collection on the new machine does not call them deleted either
    let sync = b.sync();
    assert_eq!(sync.deleted_sources, 0);
    let still =
        super::status::list_sessions(&b.env(), &super::status::SessionFilter::default()).unwrap();
    assert!(still.iter().all(|s| s.source_status == "elsewhere"));
    // and the original path moved to the new home
    let path: String = b
        .conn()
        .query_row("SELECT source_path FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert!(path.starts_with(b.claude.to_str().unwrap()), "{path}");
}

#[test]
fn a_codex_title_given_by_the_other_computer_survives_the_collection_here() {
    const CODEX: &str = "019e7371-a6b9-7a50-9825-b7acd6255914";
    let root = tempfile::tempdir().unwrap();
    let a = Lib::new(root.path(), "alice");
    let file = a.home.join(format!(
        ".codex/sessions/2026/05/29/rollout-2026-05-29T11-14-48-{CODEX}.jsonl"
    ));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        format!(
            r#"{{"timestamp":"2026-05-29T11:15:04Z","type":"session_meta","payload":{{"id":"{CODEX}","cwd":"/Users/me/app","originator":"codex-tui","source":"cli"}}}}
{{"timestamp":"2026-05-29T11:15:06Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"add a retry"}}]}}}}
"#
        ),
    )
    .unwrap();
    // the title Codex itself gave the thread
    fs::write(
        a.home.join(".codex").join("session_index.jsonl"),
        format!(r#"{{"id":"{CODEX}","thread_name":"Retry for the uploader","updated_at":"2026-05-29T12:00:00Z"}}"#),
    )
    .unwrap();
    a.sync();
    let title = |l: &Lib| -> (String, String) {
        l.conn()
            .query_row(
                "SELECT title, title_source FROM sessions WHERE source='codex'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(
        title(&a),
        ("Retry for the uploader".into(), "session_index".into())
    );
    let package = root.path().join("a.agentkb");
    a.export(&package);

    // the other computer's Codex has never heard of this thread
    let b = Lib::new(root.path(), "bob");
    b.import(&package).unwrap();
    b.sync();
    b.sync();
    assert_eq!(title(&b), title(&a), "not mistaken for a deleted thread");
}

#[test]
fn a_source_file_that_shows_up_later_is_taken_back() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    a.export(&package);
    let b = Lib::new(root.path(), "bob");
    b.import(&package).unwrap();
    // the user copies ~/.claude over
    b.write_session(S1, 3, "alpha");
    b.sync();
    let origin: Option<String> = b
        .conn()
        .query_row(
            "SELECT origin FROM files WHERE source_path LIKE ?1",
            [format!("%{S1}.jsonl")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(origin, None);
    let status: String = b
        .conn()
        .query_row(
            "SELECT source_status FROM files WHERE source_path LIKE ?1",
            [format!("%{S1}.jsonl")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "present");
}

#[test]
fn one_changed_byte_is_refused_and_nothing_changes() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    a.export(&package);
    let b = Lib::new(root.path(), "bob");
    b.write_session("bbbbbbbb-0000-0000-0000-000000000001", 2, "local");
    b.sync();
    let before = b.data_tree();
    let rows = b.count("SELECT COUNT(*) FROM sessions");
    let bytes = fs::read(&package).unwrap();
    for at in [
        0usize,
        9,
        12,
        bytes.len() / 2,
        bytes.len() - 60,
        bytes.len() - 1,
    ] {
        let mut bad = bytes.clone();
        bad[at] ^= 0x01;
        let broken = root.path().join("bad.agentkb");
        fs::write(&broken, bad).unwrap();
        let err = b.import(&broken).unwrap_err();
        assert!(matches!(err.code, "corrupt" | "too_new"), "{at}: {err}");
        assert_eq!(b.data_tree(), before, "byte {at}");
        assert_eq!(b.count("SELECT COUNT(*) FROM sessions"), rows);
    }
}

#[test]
fn a_crash_between_the_renames_of_an_empty_import_is_repaired_by_importing_again() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    a.export(&package);

    // the reference: one clean import
    let clean = Lib::new(root.path(), "clean");
    clean.import(&package).unwrap();

    for step in ["after_raw", "after_notes"] {
        for first in ["collection", "app"] {
            let b = Lib::new(root.path(), &format!("bob-{step}-{first}"));
            import::DIE_AT.with(|d| d.set(Some(step)));
            let err = b.import(&package).unwrap_err();
            import::DIE_AT.with(|d| d.set(None));
            assert!(err.message.contains("simulated crash"));
            // what happens next on the real machine: the scheduled collection
            // runs, or the app opens the database
            if first == "collection" {
                b.sync();
            } else {
                let _ = b.conn();
            }
            let report = b.import(&package).unwrap();
            assert!(
                report.all_match || report.mode == Mode::Merge,
                "{:?}",
                report.checks
            );
            // the collection after an import rebuilds an index a crash left stale
            b.sync();
            assert_eq!(
                b.count("SELECT COUNT(*) FROM sessions"),
                3,
                "{step} {first}"
            );
            assert_eq!(b.count("SELECT COUNT(*) FROM notes"), 2, "{step} {first}");
            assert_eq!(
                ids(&b, "deadlock"),
                ids(&clean, "deadlock"),
                "{step} {first}"
            );
            let files = |l: &Lib| -> Vec<String> {
                l.data_tree()
                    .into_iter()
                    .filter(|(k, _)| k.starts_with("raw/") || k.starts_with("notes/"))
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect()
            };
            assert_eq!(files(&b), files(&clean), "{step} {first}");
        }
    }
}

/// Unpack `package`, let `edit` change the database, pack it again claiming
/// `schema` (the way a file from another version looks).
fn repack(package: &Path, out: &Path, schema: i64, edit: impl Fn(&Path)) {
    repack_with(package, out, edit, |m| m.schema_version = schema);
}

fn repack_with(
    package: &Path,
    out: &Path,
    edit: impl Fn(&Path),
    edit_manifest: impl Fn(&mut super::pack::Manifest),
) {
    let manifest = super::pack::read_manifest(package).unwrap();
    let dir = out.with_extension("dir");
    super::pack::extract(package, &manifest, &dir, &Cancel::new(), &mut |_, _, _| {}).unwrap();
    edit(&dir.join("kb.db"));
    let mut writer = super::pack::Writer::new(fs::File::create(out).unwrap()).unwrap();
    for f in &manifest.files {
        let path = dir.join(&f.path);
        let len = fs::metadata(&path).unwrap().len();
        writer
            .add(
                &f.path,
                len,
                &mut fs::File::open(&path).unwrap(),
                &Cancel::new(),
                &mut |_| {},
            )
            .unwrap();
    }
    let mut next = manifest.clone();
    edit_manifest(&mut next);
    writer.finish(next).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_database_from_an_older_schema_is_upgraded_and_a_newer_one_refused() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    a.export(&package);

    // newer than this program: refused, nothing unpacked
    let newer = root.path().join("newer.agentkb");
    repack(&package, &newer, db::latest_version() + 1, |_| {});
    let b = Lib::new(root.path(), "bob");
    let report = import::inspect(&b.env(), &newer).unwrap();
    assert_eq!(report.problem.as_deref(), Some("too_new"));
    let err = b.import(&newer).unwrap_err();
    assert_eq!(err.code, "too_new");
    assert!(!b.env().data.root().join("raw").exists());
    assert!(fsutil::walk_files(b.env().data.root())
        .iter()
        .all(|(rel, _)| !rel.starts_with(".import-")));

    // the database inside claims an even newer schema than the manifest says
    let lying = root.path().join("lying.agentkb");
    repack(&package, &lying, db::latest_version(), |db_path| {
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            db::latest_version() + 5
        ))
        .unwrap();
    });
    assert_eq!(b.import(&lying).unwrap_err().code, "too_new");

    // one version older (before `origin` existed): upgraded on the way in
    let older = root.path().join("older.agentkb");
    repack(&package, &older, db::latest_version() - 1, |db_path| {
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch(&format!(
            "ALTER TABLE files DROP COLUMN origin; PRAGMA user_version = {}",
            db::latest_version() - 1
        ))
        .unwrap();
    });
    let report = b.import(&older).unwrap();
    assert!(report.all_match, "{:?}", report.checks);
    assert_eq!(b.count("SELECT COUNT(*) FROM sessions"), 3);
    assert_eq!(
        b.count("SELECT COUNT(*) FROM files WHERE origin IS NOT NULL"),
        3
    );
    assert_eq!(b.count("PRAGMA user_version"), db::latest_version());
}

#[test]
fn merging_two_libraries_keeps_everything_either_side_had() {
    let root = tempfile::tempdir().unwrap();
    // alice (the exporter)
    let a = Lib::new(root.path(), "alice");
    let only_a = "aaaaaaaa-0000-0000-0000-00000000000a";
    let same = "aaaaaaaa-0000-0000-0000-00000000000b";
    let longer_in_a = "aaaaaaaa-0000-0000-0000-00000000000c";
    let live_in_b = "aaaaaaaa-0000-0000-0000-00000000000d";
    let diverged = "aaaaaaaa-0000-0000-0000-00000000000e";
    let longer_in_b = "aaaaaaaa-0000-0000-0000-00000000000f";
    a.write_session(only_a, 2, "onlya");
    a.write_session(same, 2, "same");
    a.write_session(longer_in_a, 4, "grow");
    a.write_session(live_in_b, 4, "live");
    a.write_session(diverged, 3, "theirs");
    a.write_session(longer_in_b, 2, "short");
    a.sync();
    a.note(same, 1, "2026-10-16T05:00:00Z");
    a.note(only_a, 1, "2026-10-16T03:00:00Z");
    a.sync();
    let package = root.path().join("a.agentkb");
    let exported = a.export(&package);

    // bob (this machine): has his own data
    let b = Lib::new(root.path(), "bob");
    let only_b = "bbbbbbbb-0000-0000-0000-00000000000a";
    b.write_session(only_b, 2, "onlyb");
    b.write_session(same, 2, "same");
    b.write_session(longer_in_a, 2, "grow"); // a prefix of alice's, source removed later
    b.write_session(live_in_b, 2, "live"); // a prefix of alice's, source stays
    b.write_session(diverged, 3, "mine");
    b.write_session(longer_in_b, 4, "short"); // alice's is its prefix
    b.sync();
    b.note(same, 1, "2026-10-16T09:00:00Z"); // newer than alice's
    b.note(only_b, 1, "2026-10-16T03:00:00Z");
    b.sync();
    // the source of one session is deleted on bob's machine
    fs::remove_file(b.session_file(longer_in_a)).unwrap();
    let s = b.sync();
    assert_eq!(s.deleted_sources, 1);

    let before = b.data_tree();
    let sessions_before = b.count("SELECT COUNT(*) FROM sessions");
    let report = b.import(&package).unwrap();
    assert_eq!(report.mode, Mode::Merge);
    let summary = report.summary.clone().unwrap();
    assert_eq!(summary.extended, 1, "{summary:?}");
    assert_eq!(summary.kept_both_live, 1, "{summary:?}");
    assert_eq!(summary.kept_both_diverged, 1, "{summary:?}");
    assert_eq!(summary.notes_conflicts, 1, "{summary:?}");
    assert!(report.all_match, "{:?}", report.checks);

    // nothing bob had is gone or changed, except the one file that grew
    let after = b.data_tree();
    let grew = format!("raw/claude/projects/-Users-me-proj/{longer_in_a}.jsonl");
    for (path, hash) in &before {
        if path == &grew {
            continue;
        }
        if path.starts_with("notes/claude/") && path.contains(same) {
            continue; // replaced by the newer alice... checked below
        }
        assert_eq!(after.get(path), Some(hash), "{path} changed or disappeared");
    }
    // the grown file: bob's bytes are still the start of it
    let old = fs::read(a.session_file(longer_in_a)).unwrap();
    let now = fs::read(b.env().data.raw().join(grew.trim_start_matches("raw/"))).unwrap();
    assert_eq!(now, old, "extended to alice's full content");

    // sessions: bob's own + alice's that bob lacked + two stored beside
    assert_eq!(
        b.count("SELECT COUNT(*) FROM sessions"),
        sessions_before + 1 /* only_a */ + 2 /* beside */
    );
    // the two kept beside are both there next to the originals
    let beside: Vec<&String> = after.keys().filter(|k| k.contains(".imported-")).collect();
    assert_eq!(beside.len(), 2, "{beside:?}");
    // both versions of the diverged session are searchable
    assert_eq!(
        ids(&b, "theirs").len(),
        1,
        "alice's version of the diverged session"
    );
    assert_eq!(ids(&b, "mine").len(), 1);

    // notes: the newer one stays, the other is kept under .conflict-*
    let kept = fs::read_to_string(b.env().data.notes_file("claude", same).unwrap()).unwrap();
    assert!(
        kept.contains("2026-10-16T09:00:00Z"),
        "bob's newer note stays"
    );
    let conflict = after
        .keys()
        .find(|k| k.starts_with("notes/.conflict-") && k.contains(same));
    assert!(conflict.is_some(), "{:?}", after.keys().collect::<Vec<_>>());
    assert!(after
        .keys()
        .any(|k| k == &format!("notes/claude/{only_a}.json")));
    let _ = exported;

    // importing the same file again changes nothing more
    let again = b.import(&package).unwrap();
    assert_eq!(
        again
            .summary
            .as_ref()
            .map(|s| (s.sessions_added, s.extended)),
        Some((0, 0)),
        "{:?}",
        again.summary
    );
}

#[test]
fn a_crash_in_the_middle_of_a_merge_gives_the_same_result_as_one_run() {
    let root = tempfile::tempdir().unwrap();
    let make = |name: &str| {
        let b = Lib::new(root.path(), name);
        b.write_session("bbbbbbbb-0000-0000-0000-000000000001", 2, "mine");
        // alice's session, shorter than what the package has, source gone
        b.write_session(S1, 1, "alpha");
        b.sync();
        fs::remove_file(b.session_file(S1)).unwrap();
        b.sync();
        b
    };
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    a.export(&package);

    let clean = make("clean");
    clean.import(&package).unwrap();

    for step in ["after_files", "after_db"] {
        let b = make(&format!("crash-{step}"));
        import::DIE_AT.with(|d| d.set(Some(step)));
        assert!(b.import(&package).is_err());
        import::DIE_AT.with(|d| d.set(None));
        // in between, a normal collection runs
        b.sync();
        b.import(&package).unwrap();
        // the collection that follows an import rebuilds an index a crash left stale
        b.sync();
        let strip = |t: BTreeMap<String, String>| -> Vec<String> {
            t.into_iter()
                .filter(|(k, _)| k.starts_with("raw/") || k.starts_with("notes/"))
                .map(|(k, v)| format!("{k}={v}"))
                .collect()
        };
        assert_eq!(strip(b.data_tree()), strip(clean.data_tree()), "{step}");
        assert_eq!(
            b.count("SELECT COUNT(*) FROM sessions"),
            clean.count("SELECT COUNT(*) FROM sessions"),
            "{step}"
        );
        let sorted = |l: &Lib| {
            let mut v = ids(l, "deadlock");
            v.sort();
            v
        };
        assert_eq!(sorted(&b), sorted(&clean), "{step}");
    }
}

#[test]
fn a_tail_that_was_half_appended_when_the_process_died_is_cut_and_written_again() {
    let root = tempfile::tempdir().unwrap();
    let a = Lib::new(root.path(), "alice");
    a.write_session(S1, 6, "alpha");
    a.sync();
    let package = root.path().join("a.agentkb");
    a.export(&package);
    let full = fs::read(
        a.env()
            .data
            .raw()
            .join(format!("claude/projects/-Users-me-proj/{S1}.jsonl")),
    )
    .unwrap();

    let make = |name: &str| {
        // bob has the first two exchanges of the session, whose source is gone
        let b = Lib::new(root.path(), name);
        b.write_session(S1, 2, "alpha");
        b.sync();
        fs::remove_file(b.session_file(S1)).unwrap();
        b.sync();
        b
    };
    let clean = make("clean");
    let mirror = |l: &Lib| {
        l.env()
            .data
            .raw()
            .join(format!("claude/projects/-Users-me-proj/{S1}.jsonl"))
    };
    clean.import(&package).unwrap();
    assert_eq!(fs::read(mirror(&clean)).unwrap(), full);

    // the process died after writing half of the new bytes and before the
    // database moved: junk past the cursor
    let b = make("crashed");
    let start = fs::read(mirror(&b)).unwrap();
    assert!(start.len() < full.len() && full.starts_with(&start));
    let half = start.len() + (full.len() - start.len()) / 2;
    let mut torn = full[..half].to_vec();
    torn.extend_from_slice(b"garbage that is not part of any session");
    fs::write(mirror(&b), &torn).unwrap();

    b.import(&package).unwrap();
    b.sync();
    assert_eq!(
        fs::read(mirror(&b)).unwrap(),
        full,
        "same bytes as an import that never failed"
    );
    assert_eq!(
        b.count("SELECT message_count FROM sessions"),
        clean.count("SELECT message_count FROM sessions")
    );
    assert_eq!(ids(&b, "alpha"), ids(&clean, "alpha"));
}

#[test]
fn settings_travel_but_the_machine_specific_ones_do_not() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let mut config = a.config();
    config.exclude_prefixes = vec![format!("{}/old", a.claude.display())];
    config.exclude_cwd_prefixes = vec![format!("{}/secret", a.home.display())];
    config.interval_secs = 300;
    config.digest.enabled = true;
    config.digest.cli_path = Some(a.home.join("bin/claude"));
    config.digest.time = "07:30".into();
    let package = root.path().join("a.agentkb");
    a.enter();
    export::export(
        &a.env(),
        &config,
        &package,
        false,
        true,
        &Cancel::new(),
        &ProgressSink::default(),
    )
    .unwrap();
    let manifest = super::pack::read_manifest(&package).unwrap();
    assert_eq!(manifest.settings.digest.time, "07:30");

    // a machine with no configuration yet
    let b = Lib::new(root.path(), "bob");
    let report = b.import(&package).unwrap();
    assert!(report.config_written);
    let saved = KbConfig::load_from(&b.config_file).unwrap();
    assert!(!saved.digest.enabled, "the digest stays off");
    assert_eq!(saved.digest.cli_path, None);
    assert_eq!(saved.digest.time, "07:30");
    assert_eq!(saved.interval_secs, 300);
    assert_eq!(saved.data_dir, b.home.join("AgentKB"));
    assert!(!saved.mirror.enabled);
    // exclusions follow the user to the new home
    assert_eq!(
        saved.exclude_prefixes,
        vec![b.claude.join("old").to_string_lossy().into_owned()]
    );
    // as written (matches the sessions that came with the file) and for this home
    assert_eq!(
        saved.exclude_cwd_prefixes,
        vec![
            format!("{}/secret", a.home.display()),
            b.home.join("secret").to_string_lossy().into_owned()
        ]
    );

    // a machine that is set up already: exclusions are merged, the rest is kept
    let c = Lib::new(root.path(), "carol");
    let mut mine = c.config();
    mine.interval_secs = 900;
    mine.digest.enabled = true;
    mine.exclude_cwd_prefixes = vec![format!("{}/mine", c.home.display())];
    mine.save_to(&c.config_file).unwrap();
    let report = c.import(&package).unwrap();
    let saved = KbConfig::load_from(&c.config_file).unwrap();
    assert_eq!(saved.interval_secs, 900);
    assert!(saved.digest.enabled, "the digest switch is left as it was");
    assert_eq!(saved.exclude_cwd_prefixes.len(), 3);
    assert!(report.backup_config.is_some());
    assert!(Path::new(report.backup_config.as_ref().unwrap()).is_file());
}

#[test]
fn a_collection_runs_while_an_export_is_being_written() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    let late = "aaaaaaaa-0000-0000-0000-0000000000ff";
    let lib_ptr = a.clone();
    export::AFTER_SNAPSHOT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            // the scheduled collection arrives mid-export
            lib_ptr.write_session(late, 2, "latecomer");
            let summary = lib_ptr.sync();
            assert_eq!(summary.status, "ok", "{summary:?}");
            assert_eq!(summary.new_sessions, 1);
        }));
    });
    let report = {
        let r = a.export(&package);
        export::AFTER_SNAPSHOT.with(|hook| *hook.borrow_mut() = None);
        r
    };
    // the file is the state at the snapshot: three sessions, not four
    assert_eq!(report.sessions, 3);
    assert!(!report.paused_collection);
    assert_eq!(a.count("SELECT COUNT(*) FROM sessions"), 4);
    let b = Lib::new(root.path(), "bob");
    let imported = b.import(&package).unwrap();
    assert!(imported.all_match, "{:?}", imported.checks);
    assert_eq!(b.count("SELECT COUNT(*) FROM sessions"), 3);
}

#[test]
fn a_running_digest_stops_the_export_and_a_bad_destination_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    {
        let _digest = SyncLock::acquire(&a.env().data.digest_lock(), std::time::Duration::ZERO)
            .unwrap()
            .unwrap();
        let err = export::export(
            &a.env(),
            &a.config(),
            &package,
            false,
            true,
            &Cancel::new(),
            &ProgressSink::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, "digest_running", "{err}");
        assert!(!package.exists());
    }
    let inside = a.env().data.root().join("x.agentkb");
    let err = export::export(
        &a.env(),
        &a.config(),
        &inside,
        false,
        true,
        &Cancel::new(),
        &ProgressSink::default(),
    )
    .unwrap_err();
    assert_eq!(err.code, "bad_destination");
    a.export(&package);
    let err = export::export(
        &a.env(),
        &a.config(),
        &package,
        false,
        true,
        &Cancel::new(),
        &ProgressSink::default(),
    )
    .unwrap_err();
    assert_eq!(err.code, "exists");
    // the file is readable only by its owner
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&package).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn cancelling_leaves_no_file_and_no_work_folder() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    let cancel = Cancel::new();
    cancel.cancel();
    a.enter();
    let err = export::export(
        &a.env(),
        &a.config(),
        &package,
        false,
        true,
        &cancel,
        &ProgressSink::default(),
    )
    .unwrap_err();
    assert_eq!(err.code, "cancelled");
    assert!(!package.exists());
    assert!(!root.path().join("a.agentkb.partial").exists());
    assert!(fsutil::walk_files(a.env().data.root())
        .iter()
        .all(|(rel, _)| !rel.starts_with(".export-")));
}

#[test]
fn paths_are_converted_for_a_different_user_and_left_alone_otherwise() {
    use super::import::{convert_path, LocalPaths};
    use super::pack::SourceDevice;
    let from = SourceDevice {
        name: "A".into(),
        os: std::env::consts::OS.into(),
        home: "/Users/a".into(),
        data_dir: "/Users/a/AgentKB".into(),
        claude_root: Some("/Users/a/.claude".into()),
        codex_root: Some("/Users/a/.codex".into()),
    };
    let to = LocalPaths {
        os: std::env::consts::OS.into(),
        home: "/Users/b".into(),
        claude: Some("/Users/b/.claude".into()),
        codex: Some("/Users/b/.codex".into()),
        data_dir: "/Users/b/AgentKB".into(),
    };
    let c = |p: &str| convert_path(p, &from, &to);
    assert_eq!(
        c("/Users/a/.claude/projects/x/s.jsonl"),
        "/Users/b/.claude/projects/x/s.jsonl"
    );
    assert_eq!(
        c("/Users/a/.codex/sessions/2026/r.jsonl"),
        "/Users/b/.codex/sessions/2026/r.jsonl"
    );
    assert_eq!(
        c("/Users/a/AgentKB/digest-work"),
        "/Users/b/AgentKB/digest-work"
    );
    assert_eq!(c("/Users/a/Dev/proj"), "/Users/b/Dev/proj");
    assert_eq!(c("/Users/a"), "/Users/b");
    // not under the home: unchanged; a longer name that merely starts the same
    assert_eq!(c("/Volumes/Disk/proj"), "/Volumes/Disk/proj");
    assert_eq!(c("/Users/ab/x"), "/Users/ab/x");
    // another operating system: unchanged
    let mut other = to.clone();
    other.os = "plan9".into();
    assert_eq!(convert_path("/Users/a/x", &from, &other), "/Users/a/x");
    // a different Claude root than the home layout
    let mut moved = to.clone();
    moved.claude = Some("/data/claude".into());
    assert_eq!(
        convert_path("/Users/a/.claude/projects/p", &from, &moved),
        "/data/claude/projects/p"
    );
}

/// Evidence helper, not a regression test: rewrite who made a real export so it
/// reads as if it came from a machine whose user is `AGENTKB_AS_USER` (e.g.
/// `/Users/mo7`). Run by hand:
/// `AGENTKB_REPACK_FROM=a.agentkb AGENTKB_REPACK_TO=b.agentkb AGENTKB_AS_HOME=/Users/mo7 \
///  cargo test --release --lib repack_as_another_machine -- --ignored`.
#[test]
#[ignore = "manual evidence helper"]
fn repack_as_another_machine() {
    let (Ok(from), Ok(to), Ok(home)) = (
        std::env::var("AGENTKB_REPACK_FROM"),
        std::env::var("AGENTKB_REPACK_TO"),
        std::env::var("AGENTKB_AS_HOME"),
    ) else {
        return;
    };
    repack_with(
        Path::new(&from),
        Path::new(&to),
        |_| {},
        |m| {
            m.source_device.home.clone_from(&home);
            m.source_device.data_dir = format!("{home}/AgentKB");
            m.source_device.claude_root = Some(format!("{home}/.claude"));
            m.source_device.codex_root = Some(format!("{home}/.codex"));
            m.source_device.name = "Other-Mac".into();
        },
    );
}

// ----- the semantic index -------------------------------------------------

fn put_vectors(lib: &Lib, rows: usize, model_sha: &str) {
    use super::semantic::chunk::{text_sha, Chunk};
    use super::semantic::store::{path, Store};
    lib.enter();
    let data = lib.env().data;
    let mut store = Store::open(&path(&data), model_sha).unwrap();
    let chunks: Vec<Chunk> = (0..rows)
        .map(|i| Chunk {
            source: "claude".into(),
            session_id: S1.into(),
            key: format!("m:{i}:0"),
            kind: "user",
            text: format!("text {i}"),
            sha: text_sha(&format!("text {i}")),
        })
        .collect();
    let pairs: Vec<(&Chunk, Vec<f32>)> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut v = vec![0f32; super::semantic::DIM];
            v[i % super::semantic::DIM] = 1.0;
            (c, v)
        })
        .collect();
    store.put(&pairs).unwrap();
}

fn vector_rows(lib: &Lib) -> usize {
    use super::semantic::{model, store};
    lib.enter();
    store::Store::open_ro(&store::path(&lib.env().data), model::model_sha())
        .unwrap()
        .map_or(0, |s| s.count())
}

fn export_with(lib: &Lib, to: &Path, vectors: bool) {
    lib.enter();
    export::export(
        &lib.env(),
        &lib.config(),
        to,
        false,
        vectors,
        &Cancel::new(),
        &ProgressSink::default(),
    )
    .unwrap();
}

#[test]
fn the_semantic_index_travels_into_an_empty_library_and_only_there() {
    use super::semantic::model;
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    put_vectors(&a, 3, model::model_sha());
    let with = root.path().join("with.agentkb");
    let without = root.path().join("without.agentkb");
    export_with(&a, &with, true);
    export_with(&a, &without, false);
    let listed = |p: &Path| -> Vec<String> {
        super::pack::read_manifest(p)
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect()
    };
    assert!(listed(&with).iter().any(|f| f.starts_with("vectors/")));
    assert!(listed(&without).iter().all(|f| !f.starts_with("vectors/")));

    let b = Lib::new(root.path(), "bob");
    let report = b.import(&with).unwrap();
    assert!(
        report.vectors_adopted && report.all_match,
        "{:?}",
        report.checks
    );
    assert_eq!(vector_rows(&b), 3);

    let c = Lib::new(root.path(), "carol");
    assert!(!c.import(&without).unwrap().vectors_adopted);
    assert_eq!(vector_rows(&c), 0);

    // a library that has data of its own keeps its vectors and does not take the file's
    let d = Lib::new(root.path(), "dave");
    d.write_session("bbbbbbbb-0000-0000-0000-000000000009", 2, "delta");
    assert_eq!(d.sync().status, "ok");
    put_vectors(&d, 1, model::model_sha());
    let report = d.import(&with).unwrap();
    assert_eq!(report.mode, Mode::Merge);
    assert!(!report.vectors_adopted);
    assert_eq!(vector_rows(&d), 1);
}

#[test]
fn vectors_made_by_another_model_are_not_exported_or_taken() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    put_vectors(&a, 2, "some-other-model-sha");
    // the file on disk is for another model: the exporter does not even see it as usable
    let package = root.path().join("a.agentkb");
    export_with(&a, &package, true);
    let manifest = super::pack::read_manifest(&package).unwrap();
    assert!(manifest
        .files
        .iter()
        .all(|f| !f.path.starts_with("vectors/")));
}

#[test]
fn an_import_does_not_start_while_the_vector_builder_runs() {
    let root = tempfile::tempdir().unwrap();
    let a = library_a(root.path());
    let package = root.path().join("a.agentkb");
    a.export(&package);
    let b = Lib::new(root.path(), "bob");
    b.enter();
    let _builder = SyncLock::acquire(
        &super::semantic::build::lock_path(&b.env().data),
        std::time::Duration::ZERO,
    )
    .unwrap()
    .unwrap();
    let err = b.import(&package).unwrap_err();
    assert_eq!(err.code, "embed_running", "{err}");
    assert!(!b.env().data.raw().exists(), "nothing was applied");
}
