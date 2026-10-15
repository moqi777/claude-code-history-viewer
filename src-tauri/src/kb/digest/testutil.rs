//! Fixtures for the digest tests: a database with hand-made sessions and
//! messages, and a fake `claude`/`codex` command (`tests/fake_ai/fake-ai.sh`).

use std::cell::Cell;
use std::path::PathBuf;

use rusqlite::{params, Connection};

use crate::kb::db;
use crate::kb::paths::DataPaths;

pub struct TestKb {
    pub dir: tempfile::TempDir,
    pub data: DataPaths,
    pub conn: Connection,
    next_file: Cell<i64>,
}

impl Default for TestKb {
    fn default() -> Self {
        Self::new()
    }
}

impl TestKb {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = DataPaths::new(dir.path().join("AgentKB"));
        let conn = db::open(&data.db()).unwrap();
        Self {
            dir,
            data,
            conn,
            next_file: Cell::new(1),
        }
    }

    /// Insert a session (and the file row it hangs on); returns the file id.
    pub fn session(
        &self,
        source: &str,
        session_id: &str,
        cwd: &str,
        parent: Option<&str>,
        last_activity: &str,
    ) -> i64 {
        let id = self.next_file.get();
        self.next_file.set(id + 1);
        self.conn
            .execute(
                "INSERT INTO files (id, source, source_path, mirror_rel, src_size, src_mtime_ns, \
                 head_hash, head_len, mirror_offset, source_status, first_seen_at, last_synced_at) \
                 VALUES (?1,?2,?3,?4,0,0,x'00',0,0,'present','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![
                    id,
                    source,
                    format!("/src/{session_id}.jsonl"),
                    format!("{source}/{session_id}.jsonl")
                ],
            )
            .unwrap();
        self.conn
            .execute(
                "INSERT INTO sessions (file_id, source, session_id, cwd, project_name, title, \
                 started_at, last_activity_at, parent_session_id, is_subagent) \
                 VALUES (?1,?2,?3,?4,'proj',?5,?6,?6,?7,?8)",
                params![
                    id,
                    source,
                    session_id,
                    cwd,
                    format!("title of {session_id}"),
                    last_activity,
                    parent,
                    i64::from(parent.is_some())
                ],
            )
            .unwrap();
        id
    }

    /// Message rows numbered from `first_seq`: `(kind, timestamp, text)`.
    pub fn messages(&self, file_id: i64, first_seq: i64, rows: &[(&str, &str, &str)]) {
        for (offset, (kind, ts, text)) in rows.iter().enumerate() {
            let seq = first_seq + i64::try_from(offset).unwrap();
            self.conn
                .execute(
                    "INSERT INTO messages (file_id, seq, kind, ts, byte_off, msg_key, text) \
                     VALUES (?1,?2,?3,?4,0,?5,?6)",
                    params![file_id, seq, kind, ts, format!("key-{seq}"), text],
                )
                .unwrap();
        }
    }

    /// Two more messages after `first_seq - 1`, an hour into the 16th.
    pub fn chat_more(&self, file_id: i64, first_seq: i64) {
        self.messages(
            file_id,
            first_seq,
            &[
                ("user", "2026-10-16T01:20:00Z", "again, one more question"),
                ("assistant", "2026-10-16T01:21:00Z", "one more answer"),
            ],
        );
    }

    /// `n` alternating user/assistant messages, one minute apart from `start`.
    pub fn chat(&self, file_id: i64, n: usize, start: &str) {
        let base: chrono::DateTime<chrono::Utc> = start.parse().unwrap();
        let rows: Vec<(&str, String, String)> = (0..n)
            .map(|i| {
                let kind = if i % 2 == 0 { "user" } else { "assistant" };
                let ts = (base + chrono::Duration::minutes(i64::try_from(i).unwrap())).to_rfc3339();
                (kind, ts, format!("message {i} about locks and deadlocks"))
            })
            .collect();
        let borrowed: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|(k, t, x)| (*k, t.as_str(), x.as_str()))
            .collect();
        self.messages(file_id, 1, &borrowed);
    }
}

/// The fake AI command with its control directory.
pub struct FakeAi {
    pub dir: PathBuf,
    pub script: PathBuf,
}

impl FakeAi {
    pub fn new(root: &std::path::Path) -> Self {
        let dir = root.join("fake-ai");
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-ai.sh");
        std::fs::write(&script, include_str!("../../../tests/fake_ai/fake-ai.sh")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("FAKE_AI_DIR", &dir);
        Self { dir, script }
    }

    pub fn mode(&self, mode: &str) {
        std::fs::write(self.dir.join("mode"), mode).unwrap();
    }

    /// Override the mode of one call (1-based) only.
    pub fn mode_for_call(&self, call: usize, mode: &str) {
        std::fs::write(self.dir.join(format!("mode-{call}")), mode).unwrap();
    }

    pub fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.join("count"))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn input(&self, call: usize) -> String {
        std::fs::read_to_string(self.dir.join(format!("input-{call}"))).unwrap()
    }

    pub fn args(&self, call: usize) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(format!("args-{call}")))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn cwd(&self, call: usize) -> String {
        std::fs::read_to_string(self.dir.join(format!("cwd-{call}")))
            .unwrap()
            .trim()
            .to_string()
    }
}
