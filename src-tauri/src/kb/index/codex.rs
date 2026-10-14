//! Incremental indexer for Codex `rollout-*.jsonl` sessions (CLI, VS Code and
//! the desktop app all write the same format).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::text::{fallback_title, first_text_of_content, is_injected_user_text};
use super::{lenient_string, lines, project_name, FileMeta, SessionRecord, UnknownTypes};

const KNOWN_TYPES: &[&str] = &[
    "session_meta",
    "response_item",
    "event_msg",
    "turn_context",
    "compacted",
    "world_state",
    "token_count",
];

const FALLBACK_ATTEMPTS: u32 = 50;

pub fn file_meta(path: &Path) -> FileMeta {
    let session_id = crate::providers::codex::session_id_from_rollout_filename(path)
        .or_else(|| {
            path.file_name().and_then(|n| n.to_str()).map(|n| {
                n.trim_end_matches(".zst")
                    .trim_end_matches(".jsonl")
                    .to_owned()
            })
        })
        .unwrap_or_default();
    FileMeta {
        session_id,
        parent_session_id: None,
        is_subagent: false,
    }
}

#[derive(Deserialize)]
struct Line {
    #[serde(default, deserialize_with = "lenient_string")]
    timestamp: Option<String>,
    #[serde(rename = "type", default, deserialize_with = "lenient_string")]
    kind: Option<String>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type", default, deserialize_with = "lenient_string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    role: Option<String>,
    content: Option<Value>,
    #[serde(default, deserialize_with = "lenient_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    cwd: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    originator: Option<String>,
    source: Option<Value>,
    #[serde(default, deserialize_with = "lenient_string")]
    parent_thread_id: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct CodexIndexer {
    #[serde(skip)]
    meta: FileMeta,
    meta_seen: bool,
    session_id: Option<String>,
    cwd: Option<String>,
    turn_cwd: Option<String>,
    client: Option<String>,
    parent: Option<String>,
    message_count: u64,
    first_ts: Option<String>,
    last_ts: Option<String>,
    fallback: Option<String>,
    title_attempts: u32,
    pub unknown: UnknownTypes,
    pub bad_lines: u64,
}

/// `desktop` / `vscode` / `cli` / `exec`; subagent sessions (whose `source` is
/// an object) without a better hint are `subagent`.
fn client_of(originator: Option<&str>, source: Option<&Value>) -> Option<String> {
    if originator.is_some_and(|o| o.to_ascii_lowercase().contains("desktop")) {
        return Some("desktop".to_string());
    }
    match source {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Object(_)) => Some("subagent".to_string()),
        _ => originator.filter(|o| !o.is_empty()).map(str::to_owned),
    }
}

/// Top-level `parent_thread_id` first, then `source.subagent.thread_spawn`.
fn parent_of(payload: &Payload) -> Option<String> {
    payload.parent_thread_id.clone().or_else(|| {
        payload
            .source
            .as_ref()?
            .get("subagent")?
            .get("thread_spawn")?
            .get("parent_thread_id")?
            .as_str()
            .map(str::to_owned)
    })
}

impl CodexIndexer {
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
        let Some(payload) = line.payload else { return };

        match kind {
            // Only the FIRST session_meta identifies the file: `codex fork`
            // replays the source rollout (and its session_meta) after its own.
            "session_meta" if !self.meta_seen => {
                self.meta_seen = true;
                self.session_id = payload.id.clone().filter(|id| !id.is_empty());
                self.cwd = payload.cwd.clone().filter(|c| !c.is_empty());
                self.client = client_of(payload.originator.as_deref(), payload.source.as_ref());
                self.parent = parent_of(&payload);
            }
            "turn_context" => {
                if let Some(cwd) = payload.cwd.filter(|c| !c.is_empty()) {
                    self.turn_cwd = Some(cwd);
                }
            }
            "response_item" if payload.kind.as_deref() == Some("message") => {
                self.feed_message(&payload);
            }
            _ => {}
        }
    }

    fn feed_message(&mut self, payload: &Payload) {
        match payload.role.as_deref() {
            Some("assistant") => self.message_count += 1,
            Some("user") => {
                let text = payload.content.as_ref().and_then(first_text_of_content);
                if text.as_deref().is_some_and(is_injected_user_text) {
                    return;
                }
                self.message_count += 1;
                if self.fallback.is_none() && self.title_attempts < FALLBACK_ATTEMPTS {
                    self.title_attempts += 1;
                    self.fallback = text.as_deref().and_then(fallback_title);
                }
            }
            _ => {}
        }
    }

    pub fn record(&self) -> SessionRecord {
        let cwd = self.cwd.clone().or_else(|| self.turn_cwd.clone());
        SessionRecord {
            session_id: self
                .session_id
                .clone()
                .unwrap_or_else(|| self.meta.session_id.clone()),
            client: self.client.clone(),
            project_name: cwd.as_deref().and_then(project_name),
            cwd,
            title: self.fallback.clone(),
            title_source: self.fallback.as_ref().map(|_| "first_message".to_string()),
            started_at: self.first_ts.clone(),
            last_activity_at: self.last_ts.clone(),
            message_count: self.message_count,
            is_subagent: self.parent.is_some(),
            parent_session_id: self.parent.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const ID: &str = "019e7371-a6b9-7a50-9825-b7acd6255914";

    fn meta() -> FileMeta {
        file_meta(&PathBuf::from(format!(
            "/c/sessions/2026/05/29/rollout-2026-05-29T11-14-48-{ID}.jsonl"
        )))
    }

    fn sample() -> String {
        [
            format!(r#"{{"timestamp":"2026-05-29T11:15:04Z","type":"session_meta","payload":{{"id":"{ID}","cwd":"/Users/me/app","originator":"codex-tui","source":"cli"}}}}"#),
            r##"{"timestamp":"2026-05-29T11:15:05Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /Users/me/app"}]}}"##.to_string(),
            r#"{"timestamp":"2026-05-29T11:15:05Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context><cwd>/x</cwd>"}]}}"#.to_string(),
            r#"{"timestamp":"2026-05-29T11:15:06Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a retry to the uploader"}]}}"#.to_string(),
            r#"{"timestamp":"2026-05-29T11:15:07Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}}"#.to_string(),
            r#"{"timestamp":"2026-05-29T11:15:08Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{}"}}"#.to_string(),
            r#"{"timestamp":"2026-05-29T11:15:09Z","type":"event_msg","payload":{"type":"token_count"}}"#.to_string(),
            r#"{"timestamp":"2026-05-29T11:15:10Z","type":"mystery","payload":{"x":1}}"#.to_string(),
            r#"{"timestamp":"2026-05-29T11:16:00Z","type":"session_meta","payload":{"id":"FORKED","cwd":"/other"}}"#.to_string(),
        ]
        .join("\n")
            + "\n"
    }

    #[test]
    fn extracts_fields_ignoring_injected_messages_and_forked_meta() {
        let mut indexer = CodexIndexer::new(meta());
        indexer.feed(sample().as_bytes());
        let record = indexer.record();
        assert_eq!(record.session_id, ID);
        assert_eq!(record.cwd.as_deref(), Some("/Users/me/app"));
        assert_eq!(record.project_name.as_deref(), Some("app"));
        assert_eq!(record.client.as_deref(), Some("cli"));
        assert_eq!(record.message_count, 2, "injected user messages excluded");
        assert_eq!(record.title.as_deref(), Some("add a retry to the uploader"));
        assert_eq!(record.started_at.as_deref(), Some("2026-05-29T11:15:04Z"));
        assert_eq!(
            record.last_activity_at.as_deref(),
            Some("2026-05-29T11:16:00Z")
        );
        assert_eq!(indexer.unknown.get("mystery"), Some(&1));
        assert!(record.parent_session_id.is_none());
    }

    #[test]
    fn subagent_parent_is_read_from_top_level_then_nested_source() {
        let top = format!(
            r#"{{"type":"session_meta","payload":{{"id":"{ID}","parent_thread_id":"TOP","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"NESTED"}}}}}}}}}}"#
        );
        let mut a = CodexIndexer::new(meta());
        a.feed(top.as_bytes());
        assert_eq!(a.record().parent_session_id.as_deref(), Some("TOP"));
        assert!(a.record().is_subagent);

        let nested = format!(
            r#"{{"type":"session_meta","payload":{{"id":"{ID}","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"NESTED","depth":1}}}}}}}}}}"#
        );
        let mut b = CodexIndexer::new(meta());
        b.feed(nested.as_bytes());
        assert_eq!(b.record().parent_session_id.as_deref(), Some("NESTED"));
        assert_eq!(b.record().client.as_deref(), Some("subagent"));
    }

    #[test]
    fn client_detection() {
        assert_eq!(
            client_of(Some("Codex Desktop"), Some(&Value::String("vscode".into()))).as_deref(),
            Some("desktop")
        );
        assert_eq!(
            client_of(Some("codex_vscode"), Some(&Value::String("vscode".into()))).as_deref(),
            Some("vscode")
        );
        assert_eq!(
            client_of(Some("codex-tui"), None).as_deref(),
            Some("codex-tui")
        );
    }

    #[test]
    fn cwd_falls_back_to_turn_context_without_session_meta() {
        let mut indexer = CodexIndexer::new(meta());
        indexer.feed(br#"{"type":"turn_context","payload":{"cwd":"/from/turn"}}"#);
        let record = indexer.record();
        assert_eq!(record.cwd.as_deref(), Some("/from/turn"));
        assert_eq!(record.session_id, ID, "filename id is the fallback");
    }

    #[test]
    fn state_roundtrip_resumes_identically() {
        let text = sample();
        let cut = text.find(r#"{"timestamp":"2026-05-29T11:15:07Z""#).unwrap();
        let mut first = CodexIndexer::new(meta());
        first.feed(&text.as_bytes()[..cut]);
        let state = serde_json::to_string(&first).unwrap();
        let mut resumed = CodexIndexer::restore(meta(), &state).unwrap();
        resumed.feed(&text.as_bytes()[cut..]);
        let mut whole = CodexIndexer::new(meta());
        whole.feed(text.as_bytes());
        assert_eq!(resumed.record(), whole.record());
    }
}
