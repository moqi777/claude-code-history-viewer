//! Cutting text into pieces a model can embed.
//!
//! The rules were measured on the real library (`docs/agent-kb/P6-design.md`
//! section 3): code blocks over 200 characters are dropped, paragraphs are
//! packed up to [`TARGET`] characters, an over-long paragraph is cut at
//! sentence ends, a message gives at most [`MAX_PER_MESSAGE`] pieces and a
//! piece under [`MIN_LEN`] characters is not worth a vector.
//! Tool calls and tool output are not embedded at all.

use sha2::{Digest, Sha256};

pub const TARGET: usize = 500;
pub const MAX_PER_MESSAGE: usize = 6;
pub const MIN_LEN: usize = 20;
const MAX_FENCE: usize = 200;
pub const MAX_TITLE: usize = 300;
pub const MAX_NOTE: usize = 1200;

/// One text to embed, with the key that finds it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub source: String,
    pub session_id: String,
    /// `t` (title), `n:<entry id>` (note) or `m:<seq>:<index>` (message piece).
    pub key: String,
    /// `title`, `note`, `user` or `assistant`.
    pub kind: &'static str,
    pub text: String,
    pub sha: String,
}

pub fn text_sha(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Remove fenced code blocks longer than [`MAX_FENCE`] characters (an
/// unterminated one runs to the end), keeping short ones.
fn strip_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let (block, next) = match after.find("```") {
            Some(end) => (&rest[start..start + 3 + end + 3], &after[end + 3..]),
            None => (&rest[start..], ""),
        };
        if block.chars().count() <= MAX_FENCE {
            out.push_str(block);
        }
        rest = next;
    }
    out.push_str(rest);
    out
}

fn collapse_blanks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_blank = false;
    for c in text.chars() {
        if c == ' ' || c == '\t' {
            if !prev_blank {
                out.push(' ');
            }
            prev_blank = true;
        } else {
            out.push(c);
            prev_blank = false;
        }
    }
    out
}

/// Paragraphs: blank lines separate them, and so does a line that starts a
/// list item, heading, table row or number.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let t = cur.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
        cur.clear();
    };
    for line in text.split('\n') {
        if line.trim().is_empty() {
            flush(&mut cur, &mut out);
            continue;
        }
        let starts_item = line
            .trim_start()
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '#' | '-' | '*' | '|') || c.is_ascii_digit());
        if starts_item {
            flush(&mut cur, &mut out);
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    flush(&mut cur, &mut out);
    out
}

/// Pieces ending at sentence punctuation (the punctuation stays).
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if matches!(c, '。' | '！' | '？' | '；' | '!' | '?' | ';' | '\n') {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn take_chars(s: &str, n: usize) -> (&str, &str) {
    match s.char_indices().nth(n) {
        Some((i, _)) => s.split_at(i),
        None => (s, ""),
    }
}

/// Cut one message body into pieces.
pub fn split_text(text: &str) -> Vec<String> {
    let cleaned = collapse_blanks(&strip_fences(text));
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    let push = |cur: &mut String, cur_len: &mut usize, chunks: &mut Vec<String>| {
        let t = cur.trim();
        if !t.is_empty() {
            chunks.push(t.to_string());
        }
        cur.clear();
        *cur_len = 0;
    };
    for para in paragraphs(&cleaned) {
        let parts = if para.chars().count() <= TARGET {
            vec![para]
        } else {
            sentences(&para)
        };
        for part in parts {
            let mut s = part.as_str();
            while s.chars().count() > TARGET {
                push(&mut cur, &mut cur_len, &mut chunks);
                let (head, tail) = take_chars(s, TARGET);
                chunks.push(head.to_string());
                s = tail;
            }
            let n = s.chars().count();
            if cur_len + n + 1 > TARGET {
                push(&mut cur, &mut cur_len, &mut chunks);
            }
            if !cur.is_empty() {
                cur.push('\n');
                cur_len += 1;
            }
            cur.push_str(s);
            cur_len += n;
        }
    }
    push(&mut cur, &mut cur_len, &mut chunks);
    chunks.truncate(MAX_PER_MESSAGE);
    chunks
}

/// The pieces of one user or assistant message, ready to embed.
pub fn message_chunks(
    source: &str,
    session_id: &str,
    seq: i64,
    kind: &'static str,
    text: &str,
) -> Vec<Chunk> {
    if text.trim().chars().count() < MIN_LEN {
        return Vec::new();
    }
    split_text(text)
        .into_iter()
        .filter(|c| c.chars().count() >= MIN_LEN)
        .enumerate()
        .map(|(i, c)| Chunk {
            source: source.to_string(),
            session_id: session_id.to_string(),
            key: format!("m:{seq}:{i}"),
            kind,
            sha: text_sha(&c),
            text: c,
        })
        .collect()
}

pub fn title_chunk(source: &str, session_id: &str, title: &str) -> Option<Chunk> {
    let title = title.trim();
    if title.chars().count() < 4 {
        return None;
    }
    let (text, _) = take_chars(title, MAX_TITLE);
    Some(Chunk {
        source: source.to_string(),
        session_id: session_id.to_string(),
        key: "t".into(),
        kind: "title",
        sha: text_sha(text),
        text: text.to_string(),
    })
}

pub struct NoteText<'a> {
    pub entry_id: &'a str,
    pub title: &'a str,
    pub problem: &'a str,
    pub cause: &'a str,
    pub solution: &'a str,
    pub conclusion: &'a str,
    pub keywords: &'a str,
}

/// One note is one piece. The text goes through `redact` like everything else
/// that is indexed.
pub fn note_chunk(source: &str, session_id: &str, note: &NoteText<'_>) -> Chunk {
    let full = format!(
        "{}\n问题：{}\n原因：{}\n解决：{}\n结论：{}\n关键词：{}",
        note.title, note.problem, note.cause, note.solution, note.conclusion, note.keywords
    );
    let redacted = crate::kb::redact::redact(&full);
    let (text, _) = take_chars(&redacted, MAX_NOTE);
    Chunk {
        source: source.to_string(),
        session_id: session_id.to_string(),
        key: format!("n:{}", note.entry_id),
        kind: "note",
        sha: text_sha(text),
        text: text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_piece() {
        assert_eq!(
            split_text("你好，这是一段正常长度的文字。"),
            vec!["你好，这是一段正常长度的文字。"]
        );
    }

    #[test]
    fn long_fence_is_dropped_short_one_kept() {
        let long = format!("前文\n```rust\n{}\n```\n后文", "x".repeat(300));
        let out = split_text(&long).join("\n");
        assert!(out.contains("前文") && out.contains("后文") && !out.contains("xxx"));
        let short = "前文\n```\nls\n```\n后文";
        assert!(split_text(short).join("\n").contains("ls"));
        // an unterminated fence runs to the end
        let open = format!("说明文字\n```\n{}", "y".repeat(400));
        assert!(!split_text(&open).join("").contains("yyy"));
    }

    #[test]
    fn packs_paragraphs_and_cuts_long_ones() {
        let p = "短句一。".repeat(60); // 240 chars, no newlines
        let text = format!("{p}\n\n{p}\n\n{p}");
        let pieces = split_text(&text);
        assert!(pieces.len() >= 2);
        assert!(pieces.iter().all(|c| c.chars().count() <= TARGET));
        let giant = "无标点".repeat(400); // one 1200-char run
        assert!(split_text(&giant)
            .iter()
            .all(|c| c.chars().count() <= TARGET));
    }

    #[test]
    fn at_most_six_pieces_per_message() {
        let text = (0..40)
            .map(|i| format!("第{i}段：{}", "字".repeat(300)))
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(split_text(&text).len(), MAX_PER_MESSAGE);
    }

    #[test]
    fn list_items_start_new_paragraphs() {
        let ps = paragraphs("说明\n- 一\n- 二\n3. 三");
        assert_eq!(ps, vec!["说明", "- 一", "- 二", "3. 三"]);
    }

    #[test]
    fn short_messages_and_titles_give_nothing() {
        assert_eq!(message_chunks("codex", "s", 1, "user", "好的").len(), 0);
        assert!(title_chunk("codex", "s", "好").is_none());
        assert!(title_chunk("codex", "s", "电池掉电快问题排查").is_some());
    }

    #[test]
    fn keys_are_stable_and_hash_follows_text() {
        let a = message_chunks(
            "claude",
            "s1",
            7,
            "assistant",
            "这是一条足够长的回答，用来测试切段的键。",
        );
        let b = message_chunks(
            "claude",
            "s1",
            7,
            "assistant",
            "这是一条足够长的回答，用来测试切段的键。",
        );
        assert_eq!(a, b);
        assert_eq!(a[0].key, "m:7:0");
        let c = message_chunks(
            "claude",
            "s1",
            7,
            "assistant",
            "这是另一条足够长的回答，用来测试切段的键。",
        );
        assert_ne!(a[0].sha, c[0].sha);
    }

    #[test]
    fn note_is_redacted() {
        let note = NoteText {
            entry_id: "n1",
            title: "t",
            problem: "密钥 sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA 泄漏",
            cause: "c",
            solution: "s",
            conclusion: "x",
            keywords: "k",
        };
        let ch = note_chunk("claude", "s1", &note);
        assert!(!ch.text.contains("AAAAAAAAAAAA"), "{}", ch.text);
        assert_eq!(ch.key, "n:n1");
    }
}
