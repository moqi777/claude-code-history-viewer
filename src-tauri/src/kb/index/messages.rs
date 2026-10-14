//! Message-level extraction for full-text search.
//!
//! The session indexers ([`super::claude`], [`super::codex`]) only keep
//! per-session facts. This module turns the same JSONL lines into message
//! rows (what the user asked, what the assistant answered, tool commands and
//! truncated tool output), as a stream of [`MsgOp`]s that `commit_file`
//! applies inside the same transaction as the cursor.
//!
//! Three rules shape the output:
//! * **Truncation and redaction.** Stored text is redacted first
//!   ([`crate::kb::redact`]) and cut after, see the `CAP_*` constants.
//! * **No searching yourself.** `--kb-search` / `--kb-show` calls and their
//!   results are never indexed. A *pure Q&A turn* (the only tool calls are
//!   those) is dropped entirely — question and answer too; a turn that also
//!   did real work keeps everything except the kb calls. See [`Fate`].
//! * **Stable numbering.** `seq` counts every candidate message, including
//!   skipped ones, so an incremental build and a full rebuild number
//!   identically.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::text::is_injected_user_text;
use crate::kb::redact::redact;
use crate::kb::Source;

/// User / assistant text kept per message.
pub const CAP_BODY: usize = 16 * 1024;
/// Tool command line kept.
pub const CAP_TOOL_USE: usize = 2 * 1024;
/// Tool output: the head and the tail are kept (errors tend to sit at either end).
pub const RESULT_HEAD: usize = 1000;
pub const RESULT_TAIL: usize = 400;
pub const ERROR_HEAD: usize = 2000;
/// Redaction runs on a window this much larger than the cap, so a secret cut
/// in half by the cap is still recognised.
const REDACT_MARGIN: usize = 512;

/// First field of every kb command output; results carrying it are never indexed.
pub const MARKER: &str = "agentkb-result-v1";

/// Most prompt/assistant rows remembered for the open turn.
const TURN_ROWS_MAX: usize = 32;
const TURN_BYTES_MAX: usize = 64 * 1024;
const KB_CALLS_MAX: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    User,
    Assistant,
    ToolUse,
    ToolResult,
    ToolError,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::ToolUse => "tool_use",
            Self::ToolResult => "tool_result",
            Self::ToolError => "tool_error",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool_use" => Self::ToolUse,
            "tool_result" => Self::ToolResult,
            "tool_error" => Self::ToolError,
            _ => return None,
        })
    }

    /// Conversation text goes to the `body` FTS column, tool traffic to `tool`.
    pub const fn is_body(self) -> bool {
        matches!(self, Self::User | Self::Assistant)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRow {
    pub seq: u64,
    pub kind: Kind,
    pub ts: Option<String>,
    /// Start of the JSONL line in the (uncompressed) mirror stream.
    pub byte_off: u64,
    /// Claude: the line's `uuid`. Codex: none.
    pub msg_key: Option<String>,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsgOp {
    Insert(MessageRow),
    Remove(u64),
}

/// What happens to the prompt and answers of the current turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Fate {
    /// No kb call seen yet; rows are indexed as they come.
    #[default]
    Undecided,
    /// Only kb calls so far: the turn's own rows are held back (and the ones
    /// already indexed were removed). A real tool call turns this into `Work`.
    KbOnly,
    /// The turn did real work: everything but the kb calls is indexed.
    Work,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TurnRow {
    row: MessageRow,
    emitted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Turn {
    open: bool,
    fate: Fate,
    rows: Vec<TurnRow>,
    bytes: usize,
}

/// Running state; lives inside the session indexer's JSON state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extractor {
    bytes_fed: u64,
    next_seq: u64,
    turn: Turn,
    kb_calls: Vec<String>,
    /// True while this extractor started from byte 0 and therefore holds the
    /// complete set; the commit then replaces whatever the file had stored.
    #[serde(skip)]
    replace_all: bool,
    #[serde(skip)]
    ops: Vec<MsgOp>,
    /// Keep whole texts (still redacted): `--kb-show --full` re-reads a line.
    #[serde(skip)]
    uncapped: bool,
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

fn floor_boundary(s: &str, mut n: usize) -> usize {
    n = n.min(s.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

fn ceil_boundary(s: &str, mut n: usize) -> usize {
    while n < s.len() && !s.is_char_boundary(n) {
        n += 1;
    }
    n.min(s.len())
}

/// Head only; `(text, was_cut)`.
fn cap_head(raw: &str, limit: usize) -> (String, bool) {
    let window = &raw[..floor_boundary(raw, limit + REDACT_MARGIN)];
    let red = redact(window);
    let cut = floor_boundary(&red, limit);
    (
        red[..cut].to_string(),
        raw.len() > limit || red.len() > limit,
    )
}

/// Head and tail with ` … ` between; `(text, was_cut)`.
fn cap_head_tail(raw: &str, head: usize, tail: usize) -> (String, bool) {
    let cut_needed = raw.len() > head + tail + 8;
    let candidate = if cut_needed {
        let h = &raw[..floor_boundary(raw, head + REDACT_MARGIN)];
        let t = &raw[ceil_boundary(raw, raw.len().saturating_sub(tail + REDACT_MARGIN))..];
        format!("{h} … {t}")
    } else {
        raw.to_string()
    };
    let red = redact(&candidate).into_owned();
    if !cut_needed {
        return (red, false);
    }
    // Re-cut around the separator so head and tail keep their exact sizes.
    match red.find(" … ") {
        Some(sep) => {
            let h = &red[..floor_boundary(&red, head.min(sep))];
            let tail_start = ceil_boundary(&red, red.len().saturating_sub(tail));
            let t = &red[tail_start.max(sep + " … ".len())..];
            (format!("{h} … {t}"), true)
        }
        None => (red, true),
    }
}

static KB_COMMAND: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)(?:(?:agentkb|claude-code-history-viewer)\S*['"]?\s+--kb-(?:search|show)\b|skills/agentkb/SKILL\.md)"#,
    )
    .expect("valid kb command regex")
});

/// True for a shell command line that runs this program's search/show, or
/// that reads the skill file (how Codex loads a skill): both are the lookup
/// itself, not work.
pub fn is_kb_command(command: &str) -> bool {
    KB_COMMAND.is_match(command)
}

/// Selected fields of a tool's JSON input, flattened for display and search.
fn summarise_input(input: &Value) -> String {
    const KEYS: [&str; 10] = [
        "command",
        "cmd",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
        "old_string",
    ];
    match input {
        Value::Object(map) => {
            let parts: Vec<String> = KEYS
                .iter()
                .filter_map(|k| map.get(*k).and_then(Value::as_str))
                .map(|s| s[..floor_boundary(s, 400)].to_string())
                .collect();
            if parts.is_empty() {
                let json = input.to_string();
                json[..floor_boundary(&json, 300)].to_string()
            } else {
                parts.join(" ")
            }
        }
        other => {
            let json = other.to_string();
            json[..floor_boundary(&json, 300)].to_string()
        }
    }
}

fn blocks_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                let kind = b.get("type").and_then(Value::as_str).unwrap_or("");
                matches!(kind, "text" | "input_text" | "output_text")
                    .then(|| b.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Events: what the per-source parsers hand to the state machine
// ---------------------------------------------------------------------------

struct At<'a> {
    off: u64,
    ts: Option<&'a str>,
    key: Option<&'a str>,
}

enum Event<'a> {
    Prompt(&'a str),
    AssistantText(&'a str),
    ToolUse {
        call_id: Option<&'a str>,
        /// Display/search text: tool name + summarised input.
        text: &'a str,
        /// The raw command line (for kb detection).
        command: &'a str,
        skill_is_kb: bool,
    },
    ToolResult {
        call_id: Option<&'a str>,
        text: &'a str,
        is_error: bool,
    },
}

impl Extractor {
    /// A fresh extractor for a file that is being indexed from its first byte.
    pub fn new() -> Self {
        Self {
            replace_all: true,
            ..Self::default()
        }
    }

    /// An extractor that does not truncate, for re-reading one mirror line.
    pub fn uncapped() -> Self {
        Self {
            uncapped: true,
            ..Self::new()
        }
    }

    pub fn replace_all(&self) -> bool {
        self.replace_all
    }

    /// After a commit: the stored rows now match this extractor.
    pub fn committed(&mut self) {
        self.replace_all = false;
    }

    /// Force the next commit to replace the file's rows (state was rebuilt).
    pub fn mark_rebuilt(&mut self) {
        self.replace_all = true;
    }

    pub fn take_ops(&mut self) -> Vec<MsgOp> {
        std::mem::take(&mut self.ops)
    }

    pub fn bytes_fed(&self) -> u64 {
        self.bytes_fed
    }

    pub fn feed(&mut self, source: Source, chunk: &[u8]) {
        let mut start = 0usize;
        while start < chunk.len() {
            let end = memchr::memchr(b'\n', &chunk[start..]).map_or(chunk.len(), |p| start + p);
            let line = &chunk[start..end];
            let off = self.bytes_fed + start as u64;
            if !line.iter().all(u8::is_ascii_whitespace) {
                match source {
                    Source::Claude => self.claude_line(line, off),
                    Source::Codex => self.codex_line(line, off),
                }
            }
            start = end + 1;
        }
        self.bytes_fed += chunk.len() as u64;
    }

    // ----- Claude ---------------------------------------------------------

    fn claude_line(&mut self, raw: &[u8], off: u64) {
        // Cheap reject: only user/assistant lines matter.
        if memchr::memmem::find(raw, b"\"type\":\"user\"").is_none()
            && memchr::memmem::find(raw, b"\"type\":\"assistant\"").is_none()
        {
            return;
        }
        #[derive(Deserialize)]
        struct Msg {
            content: Option<Value>,
        }
        #[derive(Deserialize)]
        struct L {
            #[serde(rename = "type")]
            kind: Option<String>,
            uuid: Option<String>,
            timestamp: Option<String>,
            #[serde(rename = "isMeta")]
            is_meta: Option<bool>,
            message: Option<Msg>,
        }
        let Ok(line) = serde_json::from_slice::<L>(raw) else {
            return;
        };
        if line.is_meta == Some(true) {
            return;
        }
        let Some(content) = line.message.and_then(|m| m.content) else {
            return;
        };
        let at = At {
            off,
            ts: line.timestamp.as_deref(),
            key: line.uuid.as_deref(),
        };
        match (line.kind.as_deref(), &content) {
            (Some("user"), Value::String(text)) => self.prompt(&at, text),
            (Some("user"), Value::Array(blocks)) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                self.prompt(&at, text);
                            }
                        }
                        Some("tool_result") => {
                            let text = block.get("content").map(blocks_text).unwrap_or_default();
                            let is_error =
                                block.get("is_error").and_then(Value::as_bool) == Some(true);
                            self.event(
                                &at,
                                Event::ToolResult {
                                    call_id: block.get("tool_use_id").and_then(Value::as_str),
                                    text: &text,
                                    is_error,
                                },
                            );
                        }
                        _ => {}
                    }
                }
            }
            (Some("assistant"), Value::Array(blocks)) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                if !text.trim().is_empty() {
                                    self.event(&at, Event::AssistantText(text));
                                }
                            }
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            let command = input
                                .get("command")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let skill_is_kb = name == "Skill"
                                && input.get("skill").and_then(Value::as_str) == Some("agentkb");
                            let text = format!("{name} {}", summarise_input(&input));
                            self.event(
                                &at,
                                Event::ToolUse {
                                    call_id: block.get("id").and_then(Value::as_str),
                                    text: &text,
                                    command: &command,
                                    skill_is_kb,
                                },
                            );
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // ----- Codex ----------------------------------------------------------

    fn codex_line(&mut self, raw: &[u8], off: u64) {
        // `item_completed` and `compacted` are most of a Codex file and only
        // repeat content that is stored elsewhere.
        if memchr::memmem::find(raw, b"\"type\":\"response_item\"").is_none() {
            return;
        }
        #[derive(Deserialize)]
        struct L {
            timestamp: Option<String>,
            #[serde(rename = "type")]
            kind: Option<String>,
            payload: Option<Value>,
        }
        let Ok(line) = serde_json::from_slice::<L>(raw) else {
            return;
        };
        if line.kind.as_deref() != Some("response_item") {
            return;
        }
        let Some(payload) = line.payload else { return };
        let at = At {
            off,
            ts: line.timestamp.as_deref(),
            key: None,
        };
        let str_of = |key: &str| payload.get(key).and_then(Value::as_str);
        match str_of("type") {
            Some("message") => {
                let role = str_of("role").unwrap_or("");
                let mut text = payload.get("content").map(blocks_text).unwrap_or_default();
                match role {
                    "user" => {
                        // The VS Code extension prepends IDE context.
                        if let Some(i) = text.find("## My request for Codex:") {
                            text = text[i + "## My request for Codex:".len()..]
                                .trim()
                                .to_string();
                        }
                        self.prompt(&at, &text);
                    }
                    "assistant" if !text.trim().is_empty() => {
                        self.event(&at, Event::AssistantText(&text));
                    }
                    _ => {}
                }
            }
            Some(kind @ ("function_call" | "custom_tool_call")) => {
                let name = str_of("name").unwrap_or("");
                let (command, summary) = if kind == "function_call" {
                    let args = str_of("arguments").unwrap_or("");
                    match serde_json::from_str::<Value>(args) {
                        Ok(v) => (args.to_string(), summarise_input(&v)),
                        Err(_) => (
                            args.to_string(),
                            args[..floor_boundary(args, 300)].to_string(),
                        ),
                    }
                } else {
                    let input = str_of("input").unwrap_or("");
                    (input.to_string(), input.to_string())
                };
                let text = format!("{name} {summary}");
                self.event(
                    &at,
                    Event::ToolUse {
                        call_id: str_of("call_id"),
                        text: &text,
                        command: &command,
                        skill_is_kb: false,
                    },
                );
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                let output = match payload.get("output") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                let output = match output.find("\nOutput:\n") {
                    Some(i) if output.starts_with("Chunk ID") || output.contains("Wall time") => {
                        output[i + "\nOutput:\n".len()..].to_string()
                    }
                    _ => output,
                };
                self.event(
                    &at,
                    Event::ToolResult {
                        call_id: str_of("call_id"),
                        text: &output,
                        is_error: false,
                    },
                );
            }
            _ => {}
        }
    }

    // ----- state machine --------------------------------------------------

    fn prompt(&mut self, at: &At, text: &str) {
        if text.trim().is_empty() || is_injected_user_text(text) {
            return;
        }
        self.event(at, Event::Prompt(text));
    }

    fn make_row(&mut self, at: &At, kind: Kind, raw: &str) -> MessageRow {
        let (text, truncated) = if self.uncapped {
            (redact(raw).into_owned(), false)
        } else {
            match kind {
                Kind::User | Kind::Assistant => cap_head(raw, CAP_BODY),
                Kind::ToolUse => cap_head(raw, CAP_TOOL_USE),
                Kind::ToolResult => cap_head_tail(raw, RESULT_HEAD, RESULT_TAIL),
                Kind::ToolError => cap_head_tail(raw, ERROR_HEAD, RESULT_TAIL),
            }
        };
        MessageRow {
            seq: self.next_seq,
            kind,
            ts: at.ts.map(str::to_owned),
            byte_off: at.off,
            msg_key: at.key.map(str::to_owned),
            text,
            truncated,
        }
    }

    fn emit(&mut self, row: MessageRow) {
        self.ops.push(MsgOp::Insert(row));
    }

    /// Remember a prompt/answer row of the open turn (bounded), `emitted` says
    /// whether it is currently in the index.
    fn remember(&mut self, row: &MessageRow, emitted: bool) {
        if self.turn.fate == Fate::Work || !self.turn.open {
            return;
        }
        self.turn.bytes += row.text.len();
        self.turn.rows.push(TurnRow {
            row: row.clone(),
            emitted,
        });
        if self.turn.rows.len() > TURN_ROWS_MAX || self.turn.bytes > TURN_BYTES_MAX {
            // A turn this long is real work; nothing will need taking back.
            if self.turn.fate == Fate::KbOnly {
                self.release_held();
            }
            self.turn.fate = Fate::Work;
            self.turn.rows.clear();
            self.turn.bytes = 0;
        }
    }

    fn release_held(&mut self) {
        let held: Vec<MessageRow> = self
            .turn
            .rows
            .iter_mut()
            .filter(|r| !r.emitted)
            .map(|r| {
                r.emitted = true;
                r.row.clone()
            })
            .collect();
        for row in held {
            self.emit(row);
        }
    }

    fn event(&mut self, at: &At, event: Event) {
        self.next_seq += 1;
        match event {
            Event::Prompt(text) => {
                // A new turn: the previous one keeps whatever fate it had.
                self.turn = Turn {
                    open: true,
                    ..Turn::default()
                };
                let row = self.make_row(at, Kind::User, text);
                self.emit(row.clone());
                self.remember(&row, true);
            }
            Event::AssistantText(text) => {
                let row = self.make_row(at, Kind::Assistant, text);
                if self.turn.open && self.turn.fate == Fate::KbOnly {
                    self.remember(&row, false);
                } else {
                    self.emit(row.clone());
                    self.remember(&row, true);
                }
            }
            Event::ToolUse {
                call_id,
                text,
                command,
                skill_is_kb,
            } => {
                if skill_is_kb || is_kb_command(command) {
                    if let Some(id) = call_id {
                        if self.kb_calls.len() >= KB_CALLS_MAX {
                            self.kb_calls.remove(0);
                        }
                        self.kb_calls.push(id.to_string());
                    }
                    self.kb_seen();
                } else {
                    self.real_tool_seen();
                    let row = self.make_row(at, Kind::ToolUse, text);
                    self.emit(row);
                }
            }
            Event::ToolResult {
                call_id,
                text,
                is_error,
            } => {
                let known = call_id
                    .and_then(|id| self.kb_calls.iter().position(|c| c == id))
                    .map(|i| self.kb_calls.remove(i))
                    .is_some();
                if known {
                    return;
                }
                if text.contains(MARKER) {
                    self.kb_seen();
                    return;
                }
                if text.trim().is_empty() {
                    return;
                }
                let kind = if is_error {
                    Kind::ToolError
                } else {
                    Kind::ToolResult
                };
                let row = self.make_row(at, kind, text);
                self.emit(row);
            }
        }
    }

    /// A kb call or result was seen.
    fn kb_seen(&mut self) {
        if !(self.turn.open && self.turn.fate == Fate::Undecided) {
            return;
        }
        self.turn.fate = Fate::KbOnly;
        let removals: Vec<u64> = self
            .turn
            .rows
            .iter_mut()
            .filter(|r| r.emitted)
            .map(|r| {
                r.emitted = false;
                r.row.seq
            })
            .collect();
        for seq in removals {
            self.ops.push(MsgOp::Remove(seq));
        }
    }

    /// A tool call that is not a kb call: the turn did real work.
    fn real_tool_seen(&mut self) {
        if !self.turn.open {
            return;
        }
        if self.turn.fate == Fate::KbOnly {
            self.release_held();
        }
        self.turn.fate = Fate::Work;
        self.turn.rows.clear();
        self.turn.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claude_user(uuid: &str, text: &str) -> String {
        json!({"type":"user","uuid":uuid,"timestamp":"2026-01-01T00:00:00Z",
               "message":{"role":"user","content":text}})
        .to_string()
            + "\n"
    }

    fn claude_assistant(blocks: Value) -> String {
        json!({"type":"assistant","uuid":"a","timestamp":"2026-01-01T00:00:01Z",
               "message":{"role":"assistant","content":blocks}})
        .to_string()
            + "\n"
    }

    fn claude_result(id: &str, text: &str, is_error: bool) -> String {
        json!({"type":"user","uuid":"r","timestamp":"2026-01-01T00:00:02Z",
               "message":{"role":"user","content":[
                 {"type":"tool_result","tool_use_id":id,"content":text,"is_error":is_error}]}})
        .to_string()
            + "\n"
    }

    fn bash(id: &str, command: &str) -> Value {
        json!([{"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}])
    }

    const KB_CMD: &str =
        "/Applications/AgentKB.app/Contents/MacOS/AgentKB --kb-search \"deadlock\"";

    /// Feed `chunks` one after another (each its own `feed`, like separate
    /// syncs) and return the rows that remain in the "database".
    fn run(source: Source, chunks: &[String]) -> Vec<MessageRow> {
        let mut ex = Extractor::new();
        let mut db: Vec<MessageRow> = Vec::new();
        for chunk in chunks {
            ex.feed(source, chunk.as_bytes());
            for op in ex.take_ops() {
                match op {
                    MsgOp::Insert(row) => {
                        db.retain(|r| r.seq != row.seq);
                        db.push(row);
                    }
                    MsgOp::Remove(seq) => db.retain(|r| r.seq != seq),
                }
            }
        }
        db.sort_by_key(|r| r.seq);
        db
    }

    fn texts(rows: &[MessageRow]) -> Vec<String> {
        rows.iter()
            .map(|r| format!("{}:{}", r.kind.as_str(), r.text))
            .collect()
    }

    #[test]
    fn claude_basics_kinds_offsets_and_injected_text() {
        let a = claude_user("u1", "fix the login bug");
        let b = claude_user("u0", "<command-name>/init</command-name>");
        let c = claude_assistant(json!([
            {"type":"thinking","thinking":"hmm"},
            {"type":"text","text":"Looking at it"},
            {"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/x/a.rs"}}]));
        let d = claude_result("t1", "boom\nNoSuchMethodError", true);
        let rows = run(Source::Claude, &[format!("{a}{b}{c}{d}")]);
        assert_eq!(
            texts(&rows),
            vec![
                "user:fix the login bug",
                "assistant:Looking at it",
                "tool_use:Read /x/a.rs",
                "tool_error:boom\nNoSuchMethodError"
            ]
        );
        assert_eq!(rows[0].msg_key.as_deref(), Some("u1"));
        assert_eq!(rows[0].byte_off, 0);
        assert_eq!(rows[1].byte_off, (a.len() + b.len()) as u64);
        assert_eq!(
            rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn a_pure_question_and_answer_turn_about_history_is_not_indexed() {
        let chunk = [
            claude_user("u1", "我之前碰到过死锁吗"),
            claude_assistant(bash("k1", KB_CMD)),
            claude_result("k1", &format!("{{\"agentkb_marker\":\"{MARKER}\"}}"), false),
            claude_assistant(json!([{"type":"text","text":"找到了一次"}])),
            claude_user("u2", "再帮我看看限流"),
            claude_assistant(json!([{"type":"text","text":"好的"}])),
        ]
        .concat();
        let rows = run(Source::Claude, &[chunk]);
        assert_eq!(texts(&rows), vec!["user:再帮我看看限流", "assistant:好的"]);
        assert_eq!(rows[0].seq, 5, "skipped candidates keep their numbers");
    }

    #[test]
    fn a_turn_that_searched_and_then_did_work_keeps_the_work() {
        let chunk = [
            claude_user("u1", "之前那个问题怎么修的？照着修"),
            claude_assistant(json!([{"type":"text","text":"我先查一下"}])),
            claude_assistant(bash("k1", KB_CMD)),
            claude_result("k1", "kb output", false),
            claude_assistant(bash("b1", "git diff")),
            claude_result("b1", "diff output", false),
            claude_assistant(json!([{"type":"text","text":"已修复"}])),
        ]
        .concat();
        let rows = run(Source::Claude, &[chunk]);
        assert_eq!(
            texts(&rows),
            vec![
                "user:之前那个问题怎么修的？照着修",
                "assistant:我先查一下",
                "tool_use:Bash git diff",
                "tool_result:diff output",
                "assistant:已修复"
            ],
            "the kb call and its result are the only things missing"
        );
    }

    #[test]
    fn the_decision_survives_a_sync_boundary_in_the_middle_of_a_turn() {
        let first = [
            claude_user("u1", "查一下之前的限流"),
            claude_assistant(json!([{"type":"text","text":"搜索中"}])),
            claude_assistant(bash("k1", KB_CMD)),
            claude_result("k1", "kb output", false),
            claude_assistant(json!([{"type":"text","text":"结论如下"}])),
        ]
        .concat();
        let second = [
            claude_assistant(bash("b1", "cargo test")),
            claude_result("b1", "ok", false),
        ]
        .concat();
        // After the first sync the turn looks like pure Q&A…
        let after_first = run(Source::Claude, std::slice::from_ref(&first));
        assert!(after_first.is_empty(), "{:?}", texts(&after_first));
        // …and the second sync, which adds real work, brings the held rows back.
        let after_second = run(Source::Claude, &[first, second]);
        assert_eq!(
            texts(&after_second),
            vec![
                "user:查一下之前的限流",
                "assistant:搜索中",
                "assistant:结论如下",
                "tool_use:Bash cargo test",
                "tool_result:ok"
            ]
        );
    }

    #[test]
    fn incremental_and_one_shot_runs_give_identical_rows() {
        let parts = [
            claude_user("u1", "问一个问题"),
            claude_assistant(json!([{"type":"text","text":"答案"}])),
            claude_user("u2", "之前的死锁"),
            claude_assistant(bash("k1", KB_CMD)),
            claude_result("k1", "x", false),
            claude_assistant(json!([{"type":"text","text":"搜到了"}])),
            claude_user("u3", "继续"),
        ];
        let one_shot = run(Source::Claude, &[parts.concat()]);
        let each: Vec<String> = parts.to_vec();
        let incremental = run(Source::Claude, &each);
        assert_eq!(one_shot, incremental);
        assert_eq!(
            texts(&one_shot),
            vec!["user:问一个问题", "assistant:答案", "user:继续"]
        );
    }

    #[test]
    fn the_marker_alone_marks_a_result_as_the_kb_output() {
        let chunk = [
            claude_user("u1", "找找看"),
            claude_assistant(bash("s1", "some wrapper script")),
            claude_result("s1", &format!("{{\"agentkb_marker\":\"{MARKER}\"}}"), false),
            claude_assistant(json!([{"type":"text","text":"ok"}])),
        ]
        .concat();
        let rows = run(Source::Claude, &[chunk]);
        // The unrecognised wrapper counts as real work, so the turn is kept,
        // but the result carrying the marker is not.
        assert_eq!(
            texts(&rows),
            vec![
                "user:找找看",
                "tool_use:Bash some wrapper script",
                "assistant:ok"
            ]
        );
    }

    #[test]
    fn the_skill_call_counts_as_a_kb_call() {
        let skill = claude_assistant(
            json!([{"type":"tool_use","id":"s","name":"Skill","input":{"skill":"agentkb"}}]),
        );
        let chunk = [
            claude_user("u1", "之前怎么解决的"),
            skill,
            claude_assistant(json!([{"type":"text","text":"答"}])),
        ]
        .concat();
        assert!(run(Source::Claude, &[chunk]).is_empty());
    }

    #[test]
    fn codex_reading_the_skill_file_is_part_of_the_lookup_not_work() {
        let read_skill = "/bin/zsh -lc 'cat /Users/me/proj/.agents/skills/agentkb/SKILL.md'";
        assert!(is_kb_command(read_skill));
        assert!(is_kb_command("cat ~/.codex/skills/agentkb/SKILL.md"));
        assert!(!is_kb_command("cat ~/.codex/skills/other/SKILL.md"));
        let chunk = [
            codex_line("t1", json!({"type":"message","role":"user","content":[{"type":"input_text","text":"之前那个限流怎么解决的"}]})),
            codex_line("t2", json!({"type":"function_call","name":"exec_command","call_id":"c0",
                "arguments": json!({"cmd": read_skill}).to_string()})),
            codex_line("t3", json!({"type":"function_call_output","call_id":"c0","output":"---\nname: agentkb"})),
            codex_line("t4", json!({"type":"function_call","name":"exec_command","call_id":"c1",
                "arguments": json!({"cmd": KB_CMD}).to_string()})),
            codex_line("t5", json!({"type":"function_call_output","call_id":"c1","output":"{}"})),
            codex_line("t6", json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"找到了"}]})),
        ]
        .concat();
        assert!(
            run(Source::Codex, &[chunk]).is_empty(),
            "loading the skill and searching is still a pure Q&A turn"
        );
    }

    #[test]
    fn a_command_that_merely_mentions_the_flag_is_ordinary_work() {
        let chunk = [
            claude_user("u1", "grep for the flag"),
            claude_assistant(bash("g1", "grep -rn '--kb-search' src")),
            claude_result("g1", "src/cli.rs:12", false),
        ]
        .concat();
        let rows = run(Source::Claude, &[chunk]);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn secrets_are_redacted_before_truncation_and_never_stored() {
        let secret = "sk-ant-api03-AbCdEfGhIjKlMnOp1234";
        let chunk = [
            claude_user("u1", &format!("my key is {secret} please remember")),
            claude_assistant(bash("b1", "env")),
            claude_result("b1", &format!("DB_PASSWORD=hunter2\n{secret}"), false),
        ]
        .concat();
        let rows = run(Source::Claude, &[chunk]);
        for row in &rows {
            assert!(!row.text.contains(secret), "{}", row.text);
            assert!(!row.text.contains("hunter2"), "{}", row.text);
        }
        assert!(rows[0].text.contains("[REDACTED]"));
    }

    #[test]
    fn long_text_is_cut_and_flagged_and_results_keep_head_and_tail() {
        let long = "x".repeat(CAP_BODY + 100);
        let big_result = format!("HEAD{}TAIL", "m".repeat(5000));
        let chunk = [
            claude_user("u1", &long),
            claude_assistant(bash("b1", "run")),
            claude_result("b1", &big_result, false),
        ]
        .concat();
        let rows = run(Source::Claude, &[chunk]);
        assert_eq!(rows[0].text.len(), CAP_BODY);
        assert!(rows[0].truncated);
        let result = &rows[2].text;
        assert!(
            result.starts_with("HEAD") && result.ends_with("TAIL"),
            "{result}"
        );
        assert!(result.len() < 1500 && rows[2].truncated);
    }

    fn codex_line(ts: &str, payload: Value) -> String {
        json!({"timestamp":ts,"type":"response_item","payload":payload}).to_string() + "\n"
    }

    #[test]
    fn codex_messages_calls_and_outputs() {
        let chunk = [
            json!({"timestamp":"t","type":"event_msg","payload":{"type":"item_completed"}}).to_string() + "\n",
            codex_line("t1", json!({"type":"message","role":"user","content":[
                {"type":"input_text","text":"# Context from my IDE setup:\nfoo\n## My request for Codex:\n为什么限流不生效"}]})),
            codex_line("t2", json!({"type":"message","role":"developer","content":[{"type":"input_text","text":"rules"}]})),
            codex_line("t3", json!({"type":"function_call","name":"exec_command","call_id":"c1",
                "arguments":"{\"cmd\":\"rg limiter\",\"workdir\":\"/x\"}"})),
            codex_line("t4", json!({"type":"function_call_output","call_id":"c1",
                "output":"Chunk ID: 1\nWall time: 0.1 seconds\nOutput:\nsrc/limiter.rs"})),
            codex_line("t5", json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"配置里没开"}]})),
        ]
        .concat();
        let rows = run(Source::Codex, &[chunk]);
        assert_eq!(
            texts(&rows),
            vec![
                "user:为什么限流不生效",
                "tool_use:exec_command rg limiter",
                "tool_result:src/limiter.rs",
                "assistant:配置里没开"
            ]
        );
        assert_eq!(rows[0].msg_key, None);
        assert_eq!(rows[0].ts.as_deref(), Some("t1"));
    }

    #[test]
    fn codex_kb_calls_are_recognised_by_their_command() {
        let chunk = [
            codex_line("t1", json!({"type":"message","role":"user","content":[{"type":"input_text","text":"之前那个限流怎么解决的"}]})),
            codex_line("t2", json!({"type":"function_call","name":"exec_command","call_id":"c1",
                "arguments": json!({"cmd": KB_CMD}).to_string()})),
            codex_line("t3", json!({"type":"function_call_output","call_id":"c1","output":"{\"agentkb_marker\":\"x\"}"})),
            codex_line("t4", json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"找到"}]})),
        ]
        .concat();
        assert!(run(Source::Codex, &[chunk]).is_empty());
    }

    #[test]
    fn state_round_trips_through_json() {
        let mut ex = Extractor::new();
        ex.feed(
            Source::Claude,
            [
                claude_user("u1", "之前的"),
                claude_assistant(bash("k1", KB_CMD)),
            ]
            .concat()
            .as_bytes(),
        );
        let _ = ex.take_ops();
        let json = serde_json::to_string(&ex).unwrap();
        let mut restored: Extractor = serde_json::from_str(&json).unwrap();
        assert!(!restored.replace_all());
        restored.feed(
            Source::Claude,
            claude_assistant(bash("b1", "ls")).as_bytes(),
        );
        let ops = restored.take_ops();
        assert!(
            ops.iter()
                .any(|o| matches!(o, MsgOp::Insert(r) if r.kind == Kind::User)),
            "the held prompt came back: {ops:?}"
        );
    }
}
