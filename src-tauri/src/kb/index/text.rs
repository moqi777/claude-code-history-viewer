//! Shared rules for telling real user input from text the tools inject
//! (`<environment_context>`, `# AGENTS.md instructions…`, `<system-reminder>`,
//! `<command-name>`…). Used for fallback titles and Codex message counts, and
//! meant to be reused when the message table arrives.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

const FALLBACK_TITLE_CHARS: usize = 50;

/// Placeholders Claude Code puts where a pasted image or long paste was:
/// `[Image #1]`, `[Pasted text #2 +18 lines]`, `[Image: source: …]`.
static PLACEHOLDER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\[(?:Image|Pasted (?:text|image))[^\]]*\]").expect("valid placeholder regex")
});

/// The `/status`-style block that VS Code's Codex integration pastes in front
/// of (or into) the first message: `Version:`, `Session name:`, `Session ID:`…
static STATUS_BLOCK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?m)^[ \t]*Version:[ \t]+\S+[ \t]*\n(?:[ \t]*(?:Session name|Session ID|Session kind|Peer address|cwd|Model|Account)[ \t]*:.*(?:\n|$))+",
    )
    .expect("valid status block regex")
});

/// The same block after Codex flattened the newlines of a thread title.
static FLAT_STATUS_BLOCK: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"Version:\s+\S+\s+Session name:").expect("valid flat status regex"));

/// True for a tool-made title that is really a pasted status block, which says
/// nothing about the conversation; the first real message titles it instead.
pub fn is_status_block_title(title: &str) -> bool {
    FLAT_STATUS_BLOCK.is_match(title)
}

/// `text` without pasted status blocks (only blocks that carry a session id).
fn strip_status_blocks(text: &str) -> String {
    STATUS_BLOCK
        .replace_all(text, |caps: &regex::Captures| {
            if caps[0].contains("Session ID:") {
                String::new()
            } else {
                caps[0].to_string()
            }
        })
        .into_owned()
}

/// True for text a tool injected into the conversation rather than the user.
pub fn is_injected_user_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('<') || trimmed.starts_with("# AGENTS.md")
}

/// First 50 characters of the first genuinely user-typed text, or `None` if
/// the text is empty, injected, or nothing but attachment placeholders.
pub fn fallback_title(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || is_injected_user_text(trimmed) {
        return None;
    }
    let collapsed = strip_placeholders(&strip_status_blocks(trimmed));
    (!collapsed.is_empty()).then(|| collapsed.chars().take(FALLBACK_TITLE_CHARS).collect())
}

/// `text` without attachment placeholders and with whitespace collapsed.
pub fn strip_placeholders(text: &str) -> String {
    PLACEHOLDER
        .replace_all(text, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A tool-provided title cleaned of placeholders; kept as given when nothing
/// else would be left.
pub fn clean_native_title(title: &str) -> String {
    let cleaned = strip_placeholders(title);
    if cleaned.is_empty() {
        title.trim().to_string()
    } else {
        cleaned
    }
}

/// Text of a message `content` that is either a plain string or an array of
/// blocks (`{"type":"text"|"input_text"|"output_text","text":…}`). Returns the
/// first text block; tool results and images yield `None`.
pub fn first_text_of_content(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => blocks.iter().find_map(|block| {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "text" | "input_text" | "output_text") {
                block.get("text").and_then(Value::as_str).map(str::to_owned)
            } else {
                None
            }
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_injected_text() {
        for injected in [
            "<environment_context>\n<cwd>/x</cwd>",
            "  <system-reminder>x",
            "<command-name>/init</command-name>",
            "# AGENTS.md instructions for /Users/me",
        ] {
            assert!(is_injected_user_text(injected), "{injected}");
        }
        assert!(!is_injected_user_text("fix the login bug"));
        assert!(!is_injected_user_text("how do I write a < b in rust"));
    }

    #[test]
    fn fallback_title_skips_injected_and_truncates_by_chars() {
        assert_eq!(fallback_title("<environment_context>x"), None);
        assert_eq!(fallback_title("   "), None);
        assert_eq!(
            fallback_title("  hello\n  world ").as_deref(),
            Some("hello world")
        );
        let long = "字".repeat(80);
        assert_eq!(fallback_title(&long).unwrap().chars().count(), 50);
    }

    #[test]
    fn fallback_title_skips_a_pasted_status_block() {
        let first = "Version:           2.1.296\n   Session name:      月付项目\n   Session ID:        52261b64\n   Session kind:      interactive\n   Peer address:      uds:/tmp/x.sock\n   cwd:               /Users/me/proj\n\n你看一下 Claude 在这个目录下面的会话";
        assert_eq!(
            fallback_title(first).as_deref(),
            Some("你看一下 Claude 在这个目录下面的会话")
        );
        let after_text = "看一下当前目录下的历史会话\n Version:           2.1.295\n   Session name:      x\n   Session ID:        3081\n\n\n协助我排查覆盖率";
        assert_eq!(
            fallback_title(after_text).as_deref(),
            Some("看一下当前目录下的历史会话 协助我排查覆盖率")
        );
        assert_eq!(
            fallback_title("Version: 2 is out\nwhat changed?").as_deref(),
            Some("Version: 2 is out what changed?"),
            "a lone Version line is ordinary text"
        );
        assert_eq!(
            fallback_title("Version: 1\nSession ID: 9\n"),
            None,
            "nothing but the block: try the next message"
        );
    }

    #[test]
    fn a_native_title_made_of_a_status_block_is_recognised() {
        assert!(is_status_block_title(
            "Version: 2.1.296 Session name: 月付项目 Session ID: 52261b64-a33d-43c1"
        ));
        assert!(is_status_block_title(
            "看一下当前目录 这个 Version: 2.1.295 Session name: 月付"
        ));
        assert!(!is_status_block_title("Upgrade to Version: 2 today"));
    }

    #[test]
    fn fallback_title_drops_attachment_placeholders() {
        assert_eq!(
            fallback_title("[Image #1]你看一下这个目录下的内容").as_deref(),
            Some("你看一下这个目录下的内容")
        );
        assert_eq!(
            fallback_title("see [Image #2] and [Pasted text #1 +18 lines] please").as_deref(),
            Some("see and please")
        );
        assert_eq!(
            fallback_title("[Image #1]"),
            None,
            "only a placeholder: try the next message"
        );
        assert_eq!(
            fallback_title("array[0] and [x]").as_deref(),
            Some("array[0] and [x]")
        );
    }

    #[test]
    fn native_titles_are_cleaned_but_never_emptied() {
        assert_eq!(
            clean_native_title("[Image #1]这几个有什么区别"),
            "这几个有什么区别"
        );
        assert_eq!(clean_native_title("[Image #1]"), "[Image #1]");
        assert_eq!(clean_native_title("  plain  title "), "plain title");
    }

    #[test]
    fn content_text_extraction() {
        assert_eq!(first_text_of_content(&json!("hi")).as_deref(), Some("hi"));
        let blocks = json!([{"type":"tool_result","content":"x"},{"type":"text","text":"real"}]);
        assert_eq!(first_text_of_content(&blocks).as_deref(), Some("real"));
        assert_eq!(first_text_of_content(&json!([{"type":"image"}])), None);
    }
}
