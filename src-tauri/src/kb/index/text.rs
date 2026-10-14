//! Shared rules for telling real user input from text the tools inject
//! (`<environment_context>`, `# AGENTS.md instructions…`, `<system-reminder>`,
//! `<command-name>`…). Used for fallback titles and Codex message counts, and
//! meant to be reused when the message table arrives.

use serde_json::Value;

const FALLBACK_TITLE_CHARS: usize = 50;

/// True for text a tool injected into the conversation rather than the user.
pub fn is_injected_user_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('<') || trimmed.starts_with("# AGENTS.md")
}

/// First 50 characters of the first genuinely user-typed text, or `None` if
/// the text is empty or injected.
pub fn fallback_title(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || is_injected_user_text(trimmed) {
        return None;
    }
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(collapsed.chars().take(FALLBACK_TITLE_CHARS).collect())
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
    fn content_text_extraction() {
        assert_eq!(first_text_of_content(&json!("hi")).as_deref(), Some("hi"));
        let blocks = json!([{"type":"tool_result","content":"x"},{"type":"text","text":"real"}]);
        assert_eq!(first_text_of_content(&blocks).as_deref(), Some("real"));
        assert_eq!(first_text_of_content(&json!([{"type":"image"}])), None);
    }
}
