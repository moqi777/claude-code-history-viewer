//! Incremental session indexing: byte chunks of complete JSONL lines go in, a
//! [`SessionRecord`] comes out. Indexers serialise their running state so the
//! next sync resumes from the cursor instead of re-reading the file.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use memchr::memrchr;

use super::Source;

pub mod claude;
pub mod codex;
pub mod codex_titles;
pub mod messages;
pub mod text;
pub mod tokenize;

/// Tolerant field decoding: a record whose field has an unexpected type must
/// not make the whole line unparseable (the tools add and change fields).
pub(crate) fn lenient_string<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(value.as_str().map(str::to_owned))
}

pub(crate) fn lenient_bool<'de, D>(d: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(value.as_bool())
}

/// What the database stores per session (title/client/etc. may be refined
/// later, e.g. Codex's native titles).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: String,
    pub client: Option<String>,
    pub cwd: Option<String>,
    pub project_name: Option<String>,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub started_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub message_count: u64,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
}

/// Facts derived from the source path alone (not from file content).
#[derive(Debug, Clone, Default)]
pub struct FileMeta {
    /// Filename-derived session id; the fallback when content has none.
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
}

impl FileMeta {
    pub fn from_path(source: Source, path: &Path) -> Self {
        match source {
            Source::Claude => claude::file_meta(path),
            Source::Codex => codex::file_meta(path),
        }
    }
}

pub type UnknownTypes = BTreeMap<String, u64>;

enum Inner {
    Claude(claude::ClaudeIndexer),
    Codex(codex::CodexIndexer),
}

/// Session facts plus the message rows for full-text search, fed from the
/// same bytes. The message extractor's state is stored next to the session
/// indexer's, under the `msgs` key of the same JSON document.
pub struct Indexer {
    source: Source,
    inner: Inner,
    messages: messages::Extractor,
}

impl Indexer {
    pub fn new(source: Source, meta: FileMeta) -> Self {
        let inner = match source {
            Source::Claude => Inner::Claude(claude::ClaudeIndexer::new(meta)),
            Source::Codex => Inner::Codex(codex::CodexIndexer::new(meta)),
        };
        Self {
            source,
            inner,
            // Starting from byte 0: this indexer holds the complete set.
            messages: messages::Extractor::new(),
        }
    }

    /// Resume from stored state; unreadable state restarts from scratch (the
    /// caller then re-indexes from offset 0, see `sync`). State written
    /// before messages were indexed has no `msgs` and counts as unreadable.
    pub fn restore(source: Source, meta: FileMeta, state: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(state).ok()?;
        let messages: messages::Extractor =
            serde_json::from_value(value.get("msgs")?.clone()).ok()?;
        let inner = match source {
            Source::Claude => claude::ClaudeIndexer::restore(meta, state).map(Inner::Claude)?,
            Source::Codex => codex::CodexIndexer::restore(meta, state).map(Inner::Codex)?,
        };
        Some(Self {
            source,
            inner,
            messages,
        })
    }

    /// `chunk` holds whole lines (it may lack a trailing newline only for the
    /// final line of a compressed file).
    pub fn feed(&mut self, chunk: &[u8]) {
        match &mut self.inner {
            Inner::Claude(i) => i.feed(chunk),
            Inner::Codex(i) => i.feed(chunk),
        }
        self.messages.feed(self.source, chunk);
    }

    pub fn record(&self) -> SessionRecord {
        match &self.inner {
            Inner::Claude(i) => i.record(),
            Inner::Codex(i) => i.record(),
        }
    }

    pub fn state_json(&self) -> String {
        let inner = match &self.inner {
            Inner::Claude(i) => serde_json::to_value(i),
            Inner::Codex(i) => serde_json::to_value(i),
        };
        let mut value = inner.unwrap_or_else(|_| serde_json::json!({}));
        if let (Some(map), Ok(msgs)) = (value.as_object_mut(), serde_json::to_value(&self.messages))
        {
            map.insert("msgs".to_string(), msgs);
        }
        value.to_string()
    }

    pub fn unknown_types(&self) -> &UnknownTypes {
        match &self.inner {
            Inner::Claude(i) => &i.unknown,
            Inner::Codex(i) => &i.unknown,
        }
    }

    pub fn bad_lines(&self) -> u64 {
        match &self.inner {
            Inner::Claude(i) => i.bad_lines,
            Inner::Codex(i) => i.bad_lines,
        }
    }

    /// Message changes since the last call, and whether they replace every
    /// stored row of the file.
    pub fn take_message_ops(&mut self) -> (bool, Vec<messages::MsgOp>) {
        (self.messages.replace_all(), self.messages.take_ops())
    }

    /// Bytes of the mirror stream consumed so far (the next line's offset).
    pub fn bytes_fed(&self) -> u64 {
        self.messages.bytes_fed()
    }

    /// Call after the ops were committed.
    pub fn messages_committed(&mut self) {
        self.messages.committed();
    }

    /// The state was rebuilt by replaying the mirror: the stored rows must be
    /// replaced by what the replay produced.
    pub fn mark_rebuilt(&mut self) {
        self.messages.mark_rebuilt();
    }
}

/// Split `chunk` into lines (tolerating `\r\n`), skipping blanks.
pub(crate) fn lines(chunk: &[u8]) -> impl Iterator<Item = &[u8]> {
    chunk
        .split(|b| *b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
}

pub(crate) fn project_name(cwd: &str) -> Option<String> {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|leaf| !leaf.is_empty())
        .map(str::to_owned)
}

/// Result of [`for_each_line_chunk`].
#[derive(Debug, Default)]
pub struct ChunkTail {
    /// Bytes handed to the callback (always ends on a newline).
    pub consumed: u64,
    /// Bytes after the last newline: an unfinished line.
    pub tail: Vec<u8>,
    pub longest_line: u64,
}

const READ_BUF: usize = 4 * 1024 * 1024;

/// Read `reader` to EOF, calling `f` with blocks that end exactly on a newline.
/// Memory stays at one read buffer plus the longest line.
pub fn for_each_line_chunk<R: Read>(
    mut reader: R,
    mut f: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<ChunkTail, String> {
    let mut scratch = vec![0u8; READ_BUF];
    let mut carry: Vec<u8> = Vec::new();
    let mut out = ChunkTail::default();
    loop {
        let n = reader
            .read(&mut scratch)
            .map_err(|e| format!("read failed: {e}"))?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&scratch[..n]);
        if let Some(pos) = memrchr(b'\n', &carry) {
            let end = pos + 1;
            let mut start = 0usize;
            for nl in memchr::memchr_iter(b'\n', &carry[..end]) {
                out.longest_line = out.longest_line.max((nl - start) as u64);
                start = nl + 1;
            }
            f(&carry[..end])?;
            out.consumed += end as u64;
            carry.drain(..end);
        }
    }
    out.longest_line = out.longest_line.max(carry.len() as u64);
    out.tail = carry;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunker_splits_on_newlines_and_keeps_the_unfinished_tail() {
        let data = b"aaa\nbb\ncc";
        let mut seen: Vec<u8> = Vec::new();
        let tail = for_each_line_chunk(&data[..], |c| {
            seen.extend_from_slice(c);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, b"aaa\nbb\n");
        assert_eq!(tail.consumed, 7);
        assert_eq!(tail.tail, b"cc");
        assert_eq!(tail.longest_line, 3);
    }

    #[test]
    fn chunker_handles_a_line_larger_than_the_read_buffer() {
        let mut data = vec![b'x'; READ_BUF * 2 + 17];
        data.push(b'\n');
        data.extend_from_slice(b"y\n");
        let mut chunks = 0;
        let tail = for_each_line_chunk(&data[..], |_| {
            chunks += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(tail.consumed, data.len() as u64);
        assert_eq!(tail.tail.len(), 0);
        assert_eq!(tail.longest_line, (READ_BUF * 2 + 17) as u64);
        assert!(chunks >= 1);
    }

    #[test]
    fn lines_skip_blanks_and_carriage_returns() {
        let got: Vec<&[u8]> = lines(b"a\r\n\n  \nb\n").collect();
        assert_eq!(got, vec![&b"a"[..], &b"b"[..]]);
    }

    #[test]
    fn project_name_is_the_last_path_component() {
        assert_eq!(project_name("/Users/me/proj").as_deref(), Some("proj"));
        assert_eq!(
            project_name("C:\\Users\\me\\proj\\").as_deref(),
            Some("proj")
        );
        assert_eq!(project_name("/"), None);
    }
}
