//! Incremental indexer for Claude Code `*.jsonl` sessions.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::text::{fallback_title, first_text_of_content};
use super::{
    lenient_bool, lenient_string, lines, project_name, FileMeta, SessionRecord, UnknownTypes,
};

/// Record types Claude Code is known to write. Anything else is counted in
/// `unknown` and logged, never an error.
const KNOWN_TYPES: &[&str] = &[
    "user",
    "assistant",
    "system",
    "attachment",
    "ai-title",
    "custom-title",
    "last-prompt",
    "mode",
    "permission-mode",
    "file-history-snapshot",
    "summary",
    "progress",
    "queue-operation",
    "agent-name",
    "pr-link",
    "tag",
    // Metadata records of newer Claude Code versions (no conversation content).
    "bridge-session",
    "atis-latch",
    "cost-state",
    "file-history-delta",
    "artifact-autoreact-ledger",
    "artifact-comment-monitor",
    "frame-link",
];

/// Fallback-title parsing is skipped after this many user lines without a
/// usable text, so sessions made of huge tool results stay cheap.
const FALLBACK_ATTEMPTS: u32 = 50;

fn short_agent_id(session_id: &str) -> String {
    session_id
        .strip_prefix("agent-")
        .unwrap_or(session_id)
        .chars()
        .take(8)
        .collect()
}

pub fn file_meta(path: &Path) -> FileMeta {
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let dir = path.parent();
    let is_subagent = dir
        .and_then(Path::file_name)
        .is_some_and(|name| name == "subagents");
    let parent_session_id = if is_subagent {
        dir.and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .map(str::to_owned)
    } else {
        None
    };
    FileMeta {
        session_id,
        parent_session_id,
        is_subagent,
    }
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default, deserialize_with = "lenient_string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    timestamp: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    cwd: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    entrypoint: Option<String>,
    #[serde(rename = "aiTitle", default, deserialize_with = "lenient_string")]
    ai_title: Option<String>,
    #[serde(rename = "isMeta", default, deserialize_with = "lenient_bool")]
    is_meta: Option<bool>,
}

#[derive(Deserialize)]
struct UserLine {
    message: Option<UserMessage>,
}

#[derive(Deserialize)]
struct UserMessage {
    content: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ClaudeIndexer {
    #[serde(skip)]
    meta: FileMeta,
    message_count: u64,
    first_ts: Option<String>,
    last_ts: Option<String>,
    cwd: Option<String>,
    client: Option<String>,
    ai_title: Option<String>,
    fallback: Option<String>,
    title_attempts: u32,
    pub unknown: UnknownTypes,
    pub bad_lines: u64,
}

impl ClaudeIndexer {
    pub fn new(meta: FileMeta) -> Self {
        Self {
            meta,
            ..Self::default()
        }
    }

    pub fn restore(meta: FileMeta, state: &str) -> Option<Self> {
        let mut restored: Self = serde_json::from_str(state).ok()?;
        restored.meta = meta;
        Some(restored)
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        for line in lines(chunk) {
            self.feed_line(line);
        }
    }

    fn feed_line(&mut self, raw: &[u8]) {
        let Ok(line) = serde_json::from_slice::<Line>(raw) else {
            self.bad_lines += 1;
            return;
        };
        let Some(kind) = line.kind.as_deref() else {
            self.bad_lines += 1;
            return;
        };
        if !KNOWN_TYPES.contains(&kind) {
            *self.unknown.entry(kind.to_string()).or_insert(0) += 1;
        }

        if let Some(ts) = line.timestamp.as_deref().filter(|t| !t.is_empty()) {
            if self.first_ts.is_none() {
                self.first_ts = Some(ts.to_string());
            }
            if self.last_ts.as_deref().map_or(true, |last| ts > last) {
                self.last_ts = Some(ts.to_string());
            }
        }
        if self.cwd.is_none() {
            self.cwd = line
                .cwd
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_owned);
        }
        if self.client.is_none() {
            self.client = line.entrypoint.filter(|e| !e.is_empty());
        }

        match kind {
            "ai-title" => {
                if let Some(title) = line
                    .ai_title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    self.ai_title = Some(title.to_string());
                }
            }
            "user" | "assistant" if line.is_meta != Some(true) => {
                self.message_count += 1;
                if kind == "user"
                    && self.fallback.is_none()
                    && self.title_attempts < FALLBACK_ATTEMPTS
                {
                    self.title_attempts += 1;
                    self.fallback = serde_json::from_slice::<UserLine>(raw)
                        .ok()
                        .and_then(|l| l.message)
                        .and_then(|m| m.content)
                        .and_then(|c| first_text_of_content(&c))
                        .and_then(|t| fallback_title(&t));
                }
            }
            _ => {}
        }
    }

    pub fn record(&self) -> SessionRecord {
        let (title, title_source) = match (&self.ai_title, &self.fallback) {
            (Some(t), _) => (Some(t.clone()), Some("ai-title")),
            (None, Some(t)) => (Some(t.clone()), Some("first_message")),
            // A sub-agent resumed mid-task can hold only tool results: there is
            // no prompt to title it by, so label it by its agent id.
            (None, None) if self.meta.is_subagent => (
                Some(format!("agent {}", short_agent_id(&self.meta.session_id))),
                Some("agent_meta"),
            ),
            (None, None) => (None, None),
        };
        SessionRecord {
            session_id: self.meta.session_id.clone(),
            client: self.client.clone(),
            project_name: self.cwd.as_deref().and_then(project_name),
            cwd: self.cwd.clone(),
            title,
            title_source: title_source.map(str::to_owned),
            started_at: self.first_ts.clone(),
            last_activity_at: self.last_ts.clone(),
            message_count: self.message_count,
            parent_session_id: self.meta.parent_session_id.clone(),
            is_subagent: self.meta.is_subagent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn meta() -> FileMeta {
        file_meta(&PathBuf::from("/p/-Users-me-proj/abc-123.jsonl"))
    }

    const SAMPLE: &str = concat!(
        r#"{"type":"mode","mode":"x"}"#,
        "\n",
        r#"{"type":"user","uuid":"1","timestamp":"2026-01-01T00:00:01Z","cwd":"/Users/me/proj","entrypoint":"cli","message":{"role":"user","content":"<command-name>/init</command-name>"}}"#,
        "\n",
        r#"{"type":"user","uuid":"2","timestamp":"2026-01-01T00:00:02Z","cwd":"/Users/me/proj","message":{"role":"user","content":[{"type":"text","text":"fix the login bug please"}]}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"3","timestamp":"2026-01-01T00:00:03Z","message":{"role":"assistant","content":[]}}"#,
        "\n",
        r#"{"type":"user","isMeta":true,"timestamp":"2026-01-01T00:00:04Z","message":{"role":"user","content":"meta"}}"#,
        "\n",
        r#"{"type":"ai-title","aiTitle":"Fix login"}"#,
        "\n",
        r#"{"type":"brand-new-type"}"#,
        "\n",
        "not json\n",
    );

    #[test]
    fn extracts_session_fields() {
        let mut indexer = ClaudeIndexer::new(meta());
        indexer.feed(SAMPLE.as_bytes());
        let record = indexer.record();
        assert_eq!(record.session_id, "abc-123");
        assert_eq!(record.cwd.as_deref(), Some("/Users/me/proj"));
        assert_eq!(record.project_name.as_deref(), Some("proj"));
        assert_eq!(record.client.as_deref(), Some("cli"));
        assert_eq!(record.message_count, 3, "isMeta excluded");
        assert_eq!(record.started_at.as_deref(), Some("2026-01-01T00:00:01Z"));
        assert_eq!(
            record.last_activity_at.as_deref(),
            Some("2026-01-01T00:00:04Z")
        );
        assert_eq!(record.title.as_deref(), Some("Fix login"));
        assert_eq!(record.title_source.as_deref(), Some("ai-title"));
        assert_eq!(indexer.unknown.get("brand-new-type"), Some(&1));
        assert_eq!(indexer.bad_lines, 1);
    }

    #[test]
    fn fallback_title_skips_injected_first_message() {
        let mut indexer = ClaudeIndexer::new(meta());
        indexer.feed(
            SAMPLE
                .lines()
                .take(4)
                .collect::<Vec<_>>()
                .join("\n")
                .as_bytes(),
        );
        let record = indexer.record();
        assert_eq!(record.title.as_deref(), Some("fix the login bug please"));
        assert_eq!(record.title_source.as_deref(), Some("first_message"));
    }

    #[test]
    fn incremental_feed_equals_one_shot_after_state_roundtrip() {
        let cut = SAMPLE.find(r#"{"type":"assistant""#).unwrap();

        let mut first = ClaudeIndexer::new(meta());
        first.feed(&SAMPLE.as_bytes()[..cut]);
        let state = serde_json::to_string(&first).unwrap();
        let mut resumed = ClaudeIndexer::restore(meta(), &state).unwrap();
        resumed.feed(&SAMPLE.as_bytes()[cut..]);

        let mut whole = ClaudeIndexer::new(meta());
        whole.feed(SAMPLE.as_bytes());
        assert_eq!(resumed.record(), whole.record());
        assert_eq!(resumed.unknown, whole.unknown);
    }

    #[test]
    fn subagent_without_any_user_text_is_titled_by_its_agent_id() {
        let meta = file_meta(&PathBuf::from(
            "/p/proj/PARENT/subagents/agent-a8b3c72952e6cfe40.jsonl",
        ));
        let mut indexer = ClaudeIndexer::new(meta);
        indexer.feed(
            br#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}
{"type":"assistant","timestamp":"2026-01-01T00:00:02Z","message":{"role":"assistant","content":[]}}
"#,
        );
        let record = indexer.record();
        assert_eq!(record.title.as_deref(), Some("agent a8b3c729"));
        assert_eq!(record.title_source.as_deref(), Some("agent_meta"));
        assert_eq!(record.message_count, 2);
    }

    #[test]
    fn new_metadata_record_types_are_known_not_unknown() {
        let mut indexer = ClaudeIndexer::new(meta());
        for kind in [
            "atis-latch",
            "file-history-delta",
            "bridge-session",
            "cost-state",
            "frame-link",
        ] {
            indexer.feed(format!("{{\"type\":\"{kind}\"}}\n").as_bytes());
        }
        assert!(indexer.unknown.is_empty(), "{:?}", indexer.unknown);
    }

    #[test]
    fn subagent_files_know_their_parent() {
        let meta = file_meta(&PathBuf::from("/p/proj/PARENT-ID/subagents/agent-1.jsonl"));
        assert!(meta.is_subagent);
        assert_eq!(meta.parent_session_id.as_deref(), Some("PARENT-ID"));
        assert_eq!(meta.session_id, "agent-1");
    }
}
