//! Removing secrets from text before it is indexed or handed to an AI.
//!
//! Search results end up in an agent's context, i.e. they are sent to
//! Anthropic or `OpenAI` — across agents: a key seen in a Claude session could
//! leave through a Codex search. So the text that is *stored* is already
//! redacted (the full-text index never sees the values either). The daily
//! digest of phase 4 reuses [`redact`].

use std::borrow::Cow;

use once_cell::sync::Lazy;
use regex::Regex;

pub const REDACTED: &str = "[REDACTED]";

struct Rule {
    re: Regex,
    /// Replacement template (`$1` etc. refer to the pattern's groups).
    with: &'static str,
}

fn rule(pattern: &str, with: &'static str) -> Rule {
    Rule {
        re: Regex::new(pattern).expect("valid redaction regex"),
        with,
    }
}

/// Sensitive words a key name must END with, so `token_count`, `max_tokens`
/// and `tokens` are left alone but `accessToken`, `DB_PASSWORD` and
/// `client-secret` are caught.
const KEY_WORDS: &str = "password|passwd|pwd|passphrase|secret|token|api[_-]?key|apikey|access[_-]?key|secret[_-]?key|private[_-]?key|auth[_-]?key|client[_-]?secret|credentials?";

static RULES: Lazy<Vec<Rule>> = Lazy::new(|| {
    vec![
        // PEM private keys, up to the END line or the end of the text.
        rule(
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
            REDACTED,
        ),
        // Authorization header values.
        rule(
            r"(?i)(authorization\s*[:=]\s*)(?:bearer\s+|basic\s+|token\s+)?[A-Za-z0-9._~+/=\-]{8,}",
            "${1}[REDACTED]",
        ),
        rule(
            r"(?i)\bbearer\s+[A-Za-z0-9._~+/=\-]{16,}",
            "Bearer [REDACTED]",
        ),
        // Well-known key shapes.
        rule(r"\bsk-[A-Za-z0-9_\-]{16,}", REDACTED),
        rule(r"\bgh[pousr]_[A-Za-z0-9]{20,}", REDACTED),
        rule(r"\bgithub_pat_[A-Za-z0-9_]{20,}", REDACTED),
        rule(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", REDACTED),
        rule(r"\bxox[baprs]-[A-Za-z0-9\-]{10,}", REDACTED),
        rule(r"\bAIza[0-9A-Za-z_\-]{35}", REDACTED),
        // JWT.
        rule(
            r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
            REDACTED,
        ),
        // user:password@host inside URLs.
        rule(
            r"([A-Za-z][A-Za-z0-9+.\-]*://)[^/\s:@]+:[^/\s@]+@",
            "${1}[REDACTED]@",
        ),
        // `--password hunter2`, `--api-key=abc`.
        rule(
            r"(?i)(--(?:password|passwd|token|secret|api-?key|access-?key|client-?secret)(?:=|\s+))(?:[^\s\-][^\s]*)",
            "${1}[REDACTED]",
        ),
        // key = value / "key": "value" (the key must end with a sensitive word).
        rule(
            &format!(
                r#"(?i)\b([A-Za-z0-9_.\-]*(?:{KEY_WORDS}))\b(\\?["']?\s*[:=]\s*(?:\\?["'])?)((?:[^\s"'\\&,;)\]}}\[]|\\[^"'\s])(?:[^\s"'\\&,;)\]}}]|\\[^"'\s]){{2,}})"#
            ),
            "${1}${2}[REDACTED]",
        ),
    ]
});

/// A sensitive key followed by `=`/`:` and the start of a JSON object or
/// array (possibly written with escaped quotes), e.g. `password={\"a\":1}`.
static BLOCK_START: Lazy<Regex> = Lazy::new(|| {
    Regex::new(&format!(
        r#"(?i)\b([A-Za-z0-9_.\-]*(?:{KEY_WORDS}))\b(\\?["']?\s*[:=]\s*)([{{\[])"#
    ))
    .expect("valid redaction regex")
});

/// Longest bracketed value that is swallowed in one go.
const MAX_BLOCK_BYTES: usize = 20_000;

/// Byte length of the bracketed value starting at `text[0]` (an opening `{`
/// or `[`): up to the matching close, or to the end of the text / the size cap
/// when it is cut off.
fn bracketed_len(text: &str) -> usize {
    let mut depth = 0usize;
    for (i, ch) in text.char_indices() {
        if i >= MAX_BLOCK_BYTES {
            return i;
        }
        match ch {
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

fn redact_blocks(text: &str) -> Cow<'_, str> {
    if !BLOCK_START.is_match(text) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(caps) = BLOCK_START.captures_at(text, cursor) {
        let whole = caps.get(0).expect("group 0");
        let opener = caps.get(3).expect("opener group");
        let end = opener.start() + bracketed_len(&text[opener.start()..]);
        out.push_str(&text[cursor..whole.start()]);
        out.push_str(&caps[1]);
        out.push_str(&caps[2]);
        out.push_str(REDACTED);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    Cow::Owned(out)
}

/// `text` with secrets replaced by `[REDACTED]`. Borrows when nothing matched.
pub fn redact(text: &str) -> Cow<'_, str> {
    let mut current: Cow<'_, str> = match redact_blocks(text) {
        Cow::Borrowed(_) => Cow::Borrowed(text),
        Cow::Owned(done) => Cow::Owned(done),
    };
    for rule in RULES.iter() {
        if !rule.re.is_match(&current) {
            continue;
        }
        let replaced = rule.re.replace_all(&current, rule.with).into_owned();
        current = Cow::Owned(replaced);
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str) -> String {
        redact(text).into_owned()
    }

    #[test]
    fn key_value_pairs_lose_their_value_but_keep_the_key() {
        assert_eq!(r("password=hunter2"), "password=[REDACTED]");
        assert_eq!(r("DB_PASSWORD: s3cr3t!"), "DB_PASSWORD: [REDACTED]");
        assert_eq!(r(r#"{"passwd": "abc123"}"#), r#"{"passwd": "[REDACTED]"}"#);
        assert_eq!(r("export API_KEY=abcdef"), "export API_KEY=[REDACTED]");
        assert_eq!(r("accessToken = 'xyz789'"), "accessToken = '[REDACTED]'");
        assert_eq!(r("client-secret=foo123"), "client-secret=[REDACTED]");
        assert_eq!(
            r("curl --password hunter2 https://x"),
            "curl --password [REDACTED] https://x"
        );
        assert_eq!(r("tool --api-key=abc123"), "tool --api-key=[REDACTED]");
    }

    #[test]
    fn authorization_headers_and_bearer_tokens() {
        assert_eq!(
            r("Authorization: Bearer abcdef1234567890xyz"),
            "Authorization: [REDACTED]"
        );
        assert_eq!(
            r("curl -H 'Authorization: Basic dXNlcjpwYXNz' x"),
            "curl -H 'Authorization: [REDACTED]' x"
        );
        assert_eq!(
            r("send Bearer abcdefghijklmnop1234 now"),
            "send Bearer [REDACTED] now"
        );
    }

    #[test]
    fn well_known_key_shapes() {
        assert_eq!(
            r("key sk-ant-api03-AbCdEfGhIjKlMnOp1234 end"),
            "key [REDACTED] end"
        );
        assert_eq!(r("ghp_abcdefghijklmnopqrstuvwx"), "[REDACTED]");
        assert_eq!(r("github_pat_11ABCDEFG0abcdefghijklmn"), "[REDACTED]");
        assert_eq!(r("id AKIAIOSFODNN7EXAMPLE."), "id [REDACTED].");
        assert_eq!(r("xoxb-1234567890-abcdefghij"), "[REDACTED]");
        assert_eq!(
            r(&format!("k={}", "AIza".to_string() + &"aB3_-".repeat(7))),
            "k=[REDACTED]"
        );
    }

    #[test]
    fn jwt_private_key_blocks_and_url_credentials() {
        assert_eq!(
            r("t=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEF123456_-xyz!"),
            "t=[REDACTED]!"
        );
        let pem = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(r(pem), "before\n[REDACTED]\nafter");
        // Cut off before the END line (the text was truncated).
        assert_eq!(r("x -----BEGIN PRIVATE KEY-----\nMIIE"), "x [REDACTED]");
        assert_eq!(
            r("git clone https://alice:pa55word@github.com/o/r.git"),
            "git clone https://[REDACTED]@github.com/o/r.git"
        );
    }

    #[test]
    fn bracketed_and_json_escaped_values_are_redacted() {
        // The shape found in a real session (fake data): an escaped JSON object.
        assert_eq!(
            r(r#"cfg password={\"kmsKeyId\":\"k1\",\"cipherText\":\"abc==\"} next"#),
            "cfg password=[REDACTED] next"
        );
        assert_eq!(
            r(r#"{"secret": {"a": [1, 2], "b": "x"}, "ok": 1}"#),
            r#"{"secret": [REDACTED], "ok": 1}"#
        );
        assert_eq!(r("token: [\"a\", \"b\"] end"), "token: [REDACTED] end");
        // Escaped quotes around an ordinary value.
        assert_eq!(
            r(r#"{\"password\":\"hunter22\",\"user\":\"bob\"}"#),
            r#"{\"password\":\"[REDACTED]\",\"user\":\"bob\"}"#
        );
        // Cut off before the closing bracket: everything after is dropped.
        assert_eq!(r("api_key={\"a\":\"b"), "api_key=[REDACTED]");
        // Two blocks in one text.
        assert_eq!(
            r("password={a} and secret=[b]"),
            "password=[REDACTED] and secret=[REDACTED]"
        );
    }

    #[test]
    fn look_alikes_are_left_alone() {
        for text in [
            "token_count: 1234",
            "token_counts = {\"a\": 1}",
            "max_tokens: [1, 2]",
            "max_tokens=4096",
            "the tokens were counted",
            "a secret garden",
            "password: ",
            "passwordless login",
            "see https://example.com/a/b?x=1",
            "sk-short",
            "user@example.com",
            "Version: 2.1.296",
        ] {
            assert_eq!(r(text), text, "{text}");
        }
    }

    #[test]
    fn text_without_secrets_is_borrowed_not_copied() {
        assert!(matches!(redact("nothing to see"), Cow::Borrowed(_)));
    }
}
