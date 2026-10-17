//! Word segmentation shared by indexing and querying (they MUST agree).
//!
//! FTS5 receives text that is already cut into words and joined with spaces,
//! so its plain `unicode61` tokenizer only has to split on whitespace:
//! * CJK runs go through jieba's search-engine mode (`連接池` → `連接 連接池 池`
//!   style sub-words), minus punctuation and spoken-language stop words;
//! * Latin/digit runs keep the lower-cased whole word and, for camelCase,
//!   `snake_case` and letter/digit joins, also the parts
//!   (`NoSuchMethodError` → `nosuchmethoderror no such method error`);
//! * everything else (`.`, `/`, `-`, `:` …) is a separator, so package names,
//!   paths and exception names fall apart into searchable pieces.

use std::collections::HashSet;

use jieba_rs::Jieba;
use once_cell::sync::Lazy;

/// Longest Latin word kept; longer runs are hashes or base64.
const MAX_WORD: usize = 64;

/// Spoken-language filler that carries no meaning for a search.
const STOP_WORDS: &str = "的 了 是 在 我 你 他 她 它 们 这 那 有 和 与 或 就 都 也 要 会 可 能 吗 呢 吧 啊 哦 嗯 把 被 给 对 向 从 到 为 以 及 个 之 其 而 但 并 么 什么 怎么 怎样 如何 为什么 之前 上次 那个 这个 一个 一下 一些 请 帮 我们 你们 他们 自己 还是 或者 然后 因为 所以 如果 可以 需要 是否 有没有 当时 记得 是怎么 怎么样 哪个 哪些 现在 已经 还有 问题 当初 以前 以后 曾经 是不是 有没 碰到 遇到 做过 搞过";

static STOP: Lazy<HashSet<&'static str>> = Lazy::new(|| STOP_WORDS.split_whitespace().collect());

/// Built on first use: about 80 ms, and only when a text actually has CJK.
static JIEBA: Lazy<Jieba> = Lazy::new(Jieba::new);

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

/// Boundaries: lower→Upper, letter↔digit, and `HTTPServer` → `HTTP` `Server`.
fn camel_parts(piece: &str) -> Vec<String> {
    let chars: Vec<char> = piece.chars().collect();
    let mut parts = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && !current.is_empty() {
            let prev = chars[i - 1];
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_ascii_digit() != c.is_ascii_digit())
                || (prev.is_uppercase()
                    && c.is_uppercase()
                    && chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
            if boundary {
                parts.push(std::mem::take(&mut current).to_lowercase());
            }
        }
        current.push(c);
    }
    if !current.is_empty() {
        parts.push(current.to_lowercase());
    }
    parts
}

/// A maximal run of Latin/digit/underscore characters → its tokens.
fn latin_tokens(word: &str, out: &mut Vec<String>) {
    let word = word.trim_matches('_');
    if word.is_empty() || word.len() > MAX_WORD {
        return;
    }
    let whole = word.to_lowercase();
    let mut parts: Vec<String> = Vec::new();
    for piece in word.split('_') {
        parts.extend(camel_parts(piece));
    }
    out.push(whole.clone());
    if parts.len() > 1 {
        for part in parts {
            if part.len() >= 2 && part != whole {
                out.push(part);
            }
        }
    }
}

fn cjk_tokens(run: &str, out: &mut Vec<String>) {
    for word in JIEBA.cut_for_search(run, true) {
        let word = word.trim();
        if word.is_empty() || !word.chars().any(char::is_alphanumeric) || STOP.contains(word) {
            continue;
        }
        out.push(word.to_string());
    }
}

/// Tokens of `text`, in order, duplicates kept (positions matter for FTS5).
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut run_is_cjk = false;
    let flush = |run: &mut String, is_cjk: bool, out: &mut Vec<String>| {
        if run.is_empty() {
            return;
        }
        if is_cjk {
            cjk_tokens(run, out);
        } else {
            let mut start = None;
            for (i, c) in run.char_indices() {
                let word_char = c.is_ascii_alphanumeric() || c == '_';
                match (word_char, start) {
                    (true, None) => start = Some(i),
                    (false, Some(s)) => {
                        latin_tokens(&run[s..i], out);
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                latin_tokens(&run[s..], out);
            }
        }
        run.clear();
    };
    for c in text.chars() {
        let cjk = is_cjk(c);
        if cjk != run_is_cjk {
            flush(&mut run, run_is_cjk, &mut out);
            run_is_cjk = cjk;
        }
        run.push(c);
    }
    flush(&mut run, run_is_cjk, &mut out);
    out
}

/// The string handed to FTS5 for `text`.
pub fn index_text(text: &str) -> String {
    tokens(text).join(" ")
}

/// Query words grouped the way the user typed them: one group per whitespace
/// separated word (or per CJK run), each group a list of alternatives
/// `[whole, part, part…]`. For a Latin word the group matches when the whole
/// word is present OR all of its parts are; a CJK run's tokens are all
/// required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryGroup {
    /// Single term.
    Term(String),
    /// `whole OR (part AND part …)`.
    Compound { whole: String, parts: Vec<String> },
}

impl QueryGroup {
    /// Every token this group would put in the index (for snippets/coverage).
    pub fn all_tokens(&self) -> Vec<&str> {
        match self {
            Self::Term(t) => vec![t.as_str()],
            Self::Compound { whole, parts } => {
                let mut v = vec![whole.as_str()];
                v.extend(parts.iter().map(String::as_str));
                v
            }
        }
    }
}

pub fn query_groups(query: &str) -> Vec<QueryGroup> {
    let mut groups: Vec<QueryGroup> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |group: QueryGroup, groups: &mut Vec<QueryGroup>| {
        let key = format!("{:?}", group.all_tokens());
        if seen.insert(key) {
            groups.push(group);
        }
    };
    let mut run = String::new();
    let mut run_is_cjk = false;
    let mut flush = |run: &mut String, is_cjk: bool, groups: &mut Vec<QueryGroup>| {
        if run.is_empty() {
            return;
        }
        if is_cjk {
            let mut toks = Vec::new();
            cjk_tokens(run, &mut toks);
            for t in toks {
                push(QueryGroup::Term(t), groups);
            }
        } else {
            let mut start = None;
            let mut words: Vec<&str> = Vec::new();
            for (i, c) in run.char_indices() {
                let word_char = c.is_ascii_alphanumeric() || c == '_';
                match (word_char, start) {
                    (true, None) => start = Some(i),
                    (false, Some(s)) => {
                        words.push(&run[s..i]);
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                words.push(&run[s..]);
            }
            for word in words {
                let mut toks = Vec::new();
                latin_tokens(word, &mut toks);
                let mut it = toks.into_iter();
                if let Some(whole) = it.next() {
                    let parts: Vec<String> = it.collect();
                    if parts.is_empty() {
                        push(QueryGroup::Term(whole), groups);
                    } else {
                        push(QueryGroup::Compound { whole, parts }, groups);
                    }
                }
            }
        }
        run.clear();
    };
    for c in query.chars() {
        let cjk = is_cjk(c);
        if cjk != run_is_cjk {
            flush(&mut run, run_is_cjk, &mut groups);
            run_is_cjk = cjk;
        }
        run.push(c);
    }
    flush(&mut run, run_is_cjk, &mut groups);
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Vec<String> {
        tokens(text)
    }

    #[test]
    fn chinese_is_cut_into_words_and_stop_words_dropped() {
        assert_eq!(t("死锁"), vec!["死锁"]);
        assert_eq!(t("限流"), vec!["限流"]);
        assert_eq!(t("连接池耗尽"), vec!["连接", "连接池", "耗尽"]);
        let q = t("之前那个MQ消息丢失的问题是怎么解决的");
        assert!(q.contains(&"mq".to_string()));
        assert!(q.contains(&"消息".to_string()));
        assert!(q.contains(&"丢失".to_string()));
        for stop in ["的", "怎么", "之前", "那个", "问题"] {
            assert!(
                !q.contains(&stop.to_string()),
                "{stop} must be dropped: {q:?}"
            );
        }
    }

    #[test]
    fn class_names_keep_the_whole_word_and_the_parts() {
        assert_eq!(
            t("WechatAgreementRepository"),
            vec![
                "wechatagreementrepository",
                "wechat",
                "agreement",
                "repository"
            ]
        );
        assert_eq!(t("HTTPServer2"), vec!["httpserver2", "http", "server"]);
        assert_eq!(
            t("snake_case_name"),
            vec!["snake_case_name", "snake", "case", "name"]
        );
    }

    #[test]
    fn exceptions_packages_and_paths_fall_apart_on_punctuation() {
        assert_eq!(
            t("java.lang.NoSuchMethodError"),
            vec![
                "java",
                "lang",
                "nosuchmethoderror",
                "no",
                "such",
                "method",
                "error"
            ]
        );
        assert_eq!(
            t("/Users/me/a_b.txt"),
            vec!["users", "me", "a_b", "txt"],
            "one-letter parts are dropped"
        );
        assert_eq!(t("10min"), vec!["10min", "10", "min"]);
    }

    #[test]
    fn mixed_text_and_long_runs() {
        assert_eq!(t("MQ消息"), vec!["mq", "消息"]);
        assert_eq!(t(&"a".repeat(100)).len(), 0, "hash-like runs are dropped");
        assert_eq!(t("").len(), 0);
        assert_eq!(t("，。！ ,.!").len(), 0);
    }

    #[test]
    fn index_text_joins_with_spaces() {
        assert_eq!(index_text("连接池耗尽 Redis"), "连接 连接池 耗尽 redis");
    }

    #[test]
    fn query_groups_keep_compound_words_together() {
        assert_eq!(
            query_groups("NoSuchMethodError"),
            vec![QueryGroup::Compound {
                whole: "nosuchmethoderror".into(),
                parts: vec!["no".into(), "such".into(), "method".into(), "error".into()],
            }]
        );
        assert_eq!(
            query_groups("死锁 redis"),
            vec![
                QueryGroup::Term("死锁".into()),
                QueryGroup::Term("redis".into())
            ]
        );
        assert_eq!(query_groups("redis redis").len(), 1, "duplicates collapse");
        assert_eq!(query_groups("的 了").len(), 0);
    }
}
