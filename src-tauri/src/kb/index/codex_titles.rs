//! Codex's own session titles: `threads.title` in `state_*.sqlite`, falling
//! back to `session_index.jsonl`.
//!
//! The state database is a WAL database. Once Codex exits it removes
//! `-wal`/`-shm`, and a plain read-only open of such a file then fails with
//! CANTOPEN (`SQLite` wants to create the `-shm`). With no `-wal` present all
//! data is already in the main file, so reading it `immutable=1` is safe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use super::text::{clean_native_title, is_status_block_title};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    Threads,
    SessionIndex,
}

impl TitleSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::SessionIndex => "session_index",
        }
    }
}

#[derive(Debug, Default)]
pub struct NativeTitles {
    pub titles: HashMap<String, (String, TitleSource)>,
    /// Set when a state database exists but could not be read at all. Titles
    /// then come from `session_index.jsonl`; the sync reports it and goes on.
    pub warning: Option<String>,
}

pub fn load(codex_root: &Path) -> NativeTitles {
    let mut out = NativeTitles::default();

    // Lower priority first, so `threads` overwrites it.
    for (id, title) in read_session_index(&codex_root.join("session_index.jsonl")) {
        out.titles.insert(id, (title, TitleSource::SessionIndex));
    }

    let Some(db_path) = newest_state_db(codex_root) else {
        return out;
    };
    match read_threads(&db_path) {
        Ok(rows) => {
            for (id, title) in rows {
                out.titles.insert(id, (title, TitleSource::Threads));
            }
        }
        Err(e) => {
            out.warning = Some(format!(
                "cannot read Codex titles from {}: {e}; using session_index.jsonl",
                db_path.display()
            ));
        }
    }
    out
}

/// `state_<n>.sqlite` with the highest `<n>`; the version number changes
/// between Codex releases.
pub fn newest_state_db(codex_root: &Path) -> Option<PathBuf> {
    std::fs::read_dir(codex_root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let version: u64 = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse()
                .ok()?;
            entry.path().is_file().then(|| (version, entry.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

fn read_session_index(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    // Several lines per id (renames): the latest `updated_at` wins.
    let mut best: HashMap<String, (String, String)> = HashMap::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (Some(id), Some(name)) = (
            value.get("id").and_then(|v| v.as_str()),
            value.get("thread_name").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if is_status_block_title(name) {
            continue;
        }
        let name = clean_native_title(name);
        let updated = value
            .get("updated_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let newer = best.get(id).map_or(true, |(prev, _)| updated >= *prev);
        if newer {
            best.insert(id.to_string(), (updated, name));
        }
    }
    best.into_iter().map(|(id, (_, name))| (id, name)).collect()
}

pub(crate) fn sqlite_uri(path: &Path, query: &str) -> String {
    let encoded: Vec<String> = path
        .to_string_lossy()
        .replace('\\', "/")
        .split('/')
        .map(|part| urlencoding::encode(part).into_owned())
        .collect();
    let joined = encoded.join("/");
    let joined = if joined.starts_with('/') {
        joined
    } else {
        format!("/{joined}")
    };
    format!("file://{joined}?{query}")
}

fn query_threads(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT id, title FROM threads")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, title) = row?;
        let title = title.trim();
        if !title.is_empty() && !is_status_block_title(title) {
            out.push((id, clean_native_title(title)));
        }
    }
    Ok(out)
}

fn read_threads_with(path: &Path, query: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let conn = Connection::open_with_flags(
        sqlite_uri(path, query),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    query_threads(&conn)
}

/// Open without ever writing next to the database.
///
/// The `-wal` file decides how: while Codex runs, `-wal` and `-shm` exist and
/// a plain `mode=ro` open just uses them. Once Codex has exited both are gone,
/// everything is in the main file, and `immutable=1` reads it safely. Trying
/// `mode=ro` first in that state is NOT harmless: the bundled `SQLite` succeeds
/// by creating fresh `-shm`/`-wal` files in Codex's directory, i.e. it would
/// write into the source tree, and the stock `sqlite3` CLI fails outright
/// (CANTOPEN) instead.
fn read_threads(path: &Path) -> Result<Vec<(String, String)>, String> {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let query = if Path::new(&wal).exists() {
        "mode=ro"
    } else {
        "immutable=1"
    };
    read_threads_with(path, query).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state_db(dir: &Path, name: &str, rows: &[(&str, &str)]) -> PathBuf {
        let path = dir.join(name);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        conn.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL);")
            .unwrap();
        for (id, title) in rows {
            conn.execute("INSERT INTO threads VALUES (?1, ?2)", [id, title])
                .unwrap();
        }
        drop(conn);
        path
    }

    #[test]
    fn wal_database_without_wal_and_shm_files_is_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        make_state_db(
            dir.path(),
            "state_5.sqlite",
            &[("a", "Title A"), ("b", "  ")],
        );
        // Closing the last connection checkpoints and removes the side files,
        // exactly the state Codex leaves behind after it exits.
        let _ = std::fs::remove_file(dir.path().join("state_5.sqlite-wal"));
        let _ = std::fs::remove_file(dir.path().join("state_5.sqlite-shm"));
        assert!(!dir.path().join("state_5.sqlite-wal").exists());
        assert!(!dir.path().join("state_5.sqlite-shm").exists());

        let titles = load(dir.path());
        assert!(titles.warning.is_none(), "{:?}", titles.warning);
        assert!(
            !dir.path().join("state_5.sqlite-wal").exists()
                && !dir.path().join("state_5.sqlite-shm").exists(),
            "reading must not create side files in the source directory"
        );
        assert_eq!(
            titles.titles["a"],
            ("Title A".to_string(), TitleSource::Threads)
        );
        assert!(!titles.titles.contains_key("b"), "blank titles are ignored");
    }

    #[test]
    fn live_wal_database_is_read_through_its_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state_5.sqlite");
        // Keep the writer open so -wal/-shm exist, as while Codex is running.
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        writer
            .execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL);")
            .unwrap();
        writer
            .execute("INSERT INTO threads VALUES ('a', 'only in the WAL')", [])
            .unwrap();
        assert!(dir.path().join("state_5.sqlite-wal").exists());

        let titles = load(dir.path());
        assert!(titles.warning.is_none(), "{:?}", titles.warning);
        assert_eq!(titles.titles["a"].0, "only in the WAL");
        drop(writer);
    }

    #[test]
    fn newest_state_db_picks_the_highest_version_and_ignores_others() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "state_5.sqlite",
            "state_12.sqlite",
            "state_x.sqlite",
            "logs_2.sqlite",
        ] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        assert_eq!(
            newest_state_db(dir.path()).unwrap().file_name().unwrap(),
            "state_12.sqlite"
        );
    }

    #[test]
    fn unreadable_database_falls_back_to_session_index_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state_5.sqlite"), b"this is not sqlite").unwrap();
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            concat!(
                r#"{"id":"x","thread_name":"old name","updated_at":"2026-01-01T00:00:00Z"}"#,
                "\n",
                r#"{"id":"x","thread_name":"new name","updated_at":"2026-02-01T00:00:00Z"}"#,
                "\n",
            ),
        )
        .unwrap();
        let titles = load(dir.path());
        assert!(titles.warning.is_some());
        assert_eq!(
            titles.titles["x"],
            ("new name".to_string(), TitleSource::SessionIndex)
        );
    }

    #[test]
    fn threads_title_beats_session_index() {
        let dir = tempfile::tempdir().unwrap();
        make_state_db(dir.path(), "state_5.sqlite", &[("x", "from threads")]);
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            r#"{"id":"x","thread_name":"from index","updated_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()).titles["x"].0, "from threads");
    }

    #[test]
    fn uri_encodes_spaces_and_keeps_slashes() {
        let uri = sqlite_uri(Path::new("/Users/a b/.codex/state_5.sqlite"), "mode=ro");
        assert_eq!(uri, "file:///Users/a%20b/.codex/state_5.sqlite?mode=ro");
    }
}
