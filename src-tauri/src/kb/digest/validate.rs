//! Checking what the AI answered, and merging it into the session's notes.
//!
//! The answer is untrusted text: it is parsed, bounds-checked, redacted again
//! and only then becomes part of a note file. Only the first eight entries
//! are kept; a retained entry that breaks a rule rejects the whole answer.

use serde::Deserialize;

use crate::kb::notes::{EngineInfo, Entry, Evidence, NotesFile};
use crate::kb::redact::redact;

pub const MAX_ENTRIES: usize = 8;
const MAX_TITLE: usize = 80;
const MAX_PROBLEM: usize = 800;
const MAX_CAUSE: usize = 800;
const MAX_SOLUTION: usize = 1200;
const MAX_CONCLUSION: usize = 400;
const MAX_KEYWORDS: usize = 8;
const MAX_KEYWORD: usize = 40;

#[derive(Debug, Deserialize)]
struct Answer {
    entries: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Raw {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    problem: String,
    #[serde(default)]
    cause: String,
    #[serde(default)]
    solution: String,
    #[serde(default)]
    conclusion: String,
    #[serde(default)]
    keywords: Vec<String>,
    seq_from: u64,
    seq_to: u64,
}

/// One entry that passed every check (text already redacted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// An existing entry this one replaces.
    pub replaces: Option<String>,
    pub title: String,
    pub problem: String,
    pub cause: String,
    pub solution: String,
    pub conclusion: String,
    pub keywords: Vec<String>,
    pub seq_from: u64,
    pub seq_to: u64,
}

/// The sequence numbers an answer may refer to, and the entries it may update.
pub struct Bounds<'a> {
    pub seq_min: u64,
    pub seq_max: u64,
    pub previous_ids: Vec<&'a str>,
}

/// The JSON object inside `text`, tolerating a code fence or a lead-in.
fn json_slice(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

fn limit(field: &str, text: &str, max: usize) -> Result<String, String> {
    let clean = redact(text.trim()).into_owned();
    if clean.chars().count() > max {
        return Err(format!("{field} is longer than {max} characters"));
    }
    Ok(clean)
}

pub fn check(answer: &str, bounds: &Bounds<'_>) -> Result<Vec<Checked>, String> {
    check_with_log(answer, bounds, |discarded| {
        log::warn!("digest answer: discarded {discarded} entries beyond {MAX_ENTRIES}");
    })
}

pub fn check_with_log(
    answer: &str,
    bounds: &Bounds<'_>,
    discarded: impl FnOnce(usize),
) -> Result<Vec<Checked>, String> {
    let json = json_slice(answer).ok_or("the answer holds no JSON object")?;
    let parsed: Answer = serde_json::from_str(json)
        .map_err(|e| format!("the answer is not the expected JSON: {e}"))?;
    if parsed.entries.len() > MAX_ENTRIES {
        discarded(parsed.entries.len() - MAX_ENTRIES);
    }
    let mut out = Vec::new();
    for (n, value) in parsed.entries.into_iter().take(MAX_ENTRIES).enumerate() {
        let at = |msg: String| format!("entry {}: {msg}", n + 1);
        let raw: Raw = serde_json::from_value(value)
            .map_err(|e| at(format!("not the expected entry JSON: {e}")))?;
        let title = limit("title", &raw.title, MAX_TITLE).map_err(at)?;
        if title.is_empty() {
            return Err(at("title is empty".into()));
        }
        let conclusion = limit("conclusion", &raw.conclusion, MAX_CONCLUSION).map_err(at)?;
        if conclusion.is_empty() {
            return Err(at("conclusion is empty".into()));
        }
        let problem = limit("problem", &raw.problem, MAX_PROBLEM).map_err(at)?;
        let cause = limit("cause", &raw.cause, MAX_CAUSE).map_err(at)?;
        let solution = limit("solution", &raw.solution, MAX_SOLUTION).map_err(at)?;
        let mut keywords = Vec::new();
        for keyword in &raw.keywords {
            let keyword = limit("a keyword", keyword, MAX_KEYWORD).map_err(at)?;
            if !keyword.is_empty() && !keywords.contains(&keyword) {
                keywords.push(keyword);
            }
        }
        if keywords.is_empty() || keywords.len() > MAX_KEYWORDS {
            return Err(at(format!("needs 1 to {MAX_KEYWORDS} keywords")));
        }
        if raw.seq_from > raw.seq_to || raw.seq_from < bounds.seq_min || raw.seq_to > bounds.seq_max
        {
            return Err(at(format!(
                "message range #{}-#{} is outside #{}-#{}",
                raw.seq_from, raw.seq_to, bounds.seq_min, bounds.seq_max
            )));
        }
        let replaces = match raw.id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
            None => None,
            Some(id) if bounds.previous_ids.contains(&id) => Some(id.to_string()),
            Some(id) => {
                return Err(at(format!(
                    "it updates {id:?}, which is not a previous note"
                )))
            }
        };
        out.push(Checked {
            replaces,
            title,
            problem,
            cause,
            solution,
            conclusion,
            keywords,
            seq_from: raw.seq_from,
            seq_to: raw.seq_to,
        });
    }
    let mut replaced: Vec<&str> = out.iter().filter_map(|c| c.replaces.as_deref()).collect();
    replaced.sort_unstable();
    if replaced.windows(2).any(|w| w[0] == w[1]) {
        return Err("two entries update the same previous note".to_string());
    }
    Ok(out)
}

/// What the opener needs to find a message again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub msg_key: Option<String>,
    pub at: Option<String>,
    pub kind: String,
}

/// Who wrote the entries and when, for the entries this call produced.
pub struct Authorship<'a> {
    pub round: &'a str,
    pub at: &'a str,
    pub engine: EngineInfo,
    /// The message at (or just after) a `seq`, for the entry's evidence.
    pub anchor_of: &'a dyn Fn(u64) -> Option<Anchor>,
}

/// Fold checked entries into `file`. Entries the AI did not mention stay as
/// they were; the AI cannot delete one.
pub fn merge(file: &mut NotesFile, checked: Vec<Checked>, who: &Authorship<'_>) {
    for item in checked {
        let id = item
            .replaces
            .clone()
            .unwrap_or_else(|| file.next_entry_id());
        let anchor = (who.anchor_of)(item.seq_from);
        let entry = Entry {
            id: id.clone(),
            title: item.title,
            problem: item.problem,
            cause: item.cause,
            solution: item.solution,
            conclusion: item.conclusion,
            keywords: item.keywords,
            evidence: Evidence {
                seq_from: item.seq_from,
                seq_to: item.seq_to,
                msg_key: anchor.as_ref().and_then(|a| a.msg_key.clone()),
                at: anchor.as_ref().and_then(|a| a.at.clone()),
                kind: anchor.map(|a| a.kind),
            },
            round: who.round.to_string(),
            digested_at: who.at.to_string(),
            engine: who.engine.clone(),
        };
        match file.entries.iter_mut().find(|e| e.id == id) {
            Some(slot) => *slot = entry,
            None => file.entries.push(entry),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds<'a>(ids: &[&'a str]) -> Bounds<'a> {
        Bounds {
            seq_min: 10,
            seq_max: 50,
            previous_ids: ids.to_vec(),
        }
    }

    fn entry(extra: &str) -> String {
        format!(
            r#"{{"entries":[{{"id":null,"title":"t","problem":"p","cause":"","solution":"s","conclusion":"c","keywords":["k"],"seq_from":12,"seq_to":20{extra}}}]}}"#
        )
    }

    #[test]
    fn a_good_answer_passes_and_code_fences_are_tolerated() {
        let plain = check(&entry(""), &bounds(&[])).unwrap();
        assert_eq!(plain.len(), 1);
        assert_eq!((plain[0].seq_from, plain[0].seq_to), (12, 20));
        let fenced = format!("Here you go:\n```json\n{}\n```", entry(""));
        assert_eq!(check(&fenced, &bounds(&[])).unwrap(), plain);
        assert_eq!(check(r#"{"entries":[]}"#, &bounds(&[])).unwrap().len(), 0);
    }

    #[test]
    fn nine_entries_keep_the_first_eight_unchanged_and_log_once() {
        let entries: Vec<serde_json::Value> = (0..9)
            .map(|i| {
                let mut value: serde_json::Value = serde_json::from_str(&entry("")).unwrap();
                value["entries"][0]["title"] = format!("topic {i}").into();
                value["entries"][0].clone()
            })
            .collect();
        let expected = check(
            &serde_json::json!({"entries": &entries[..8]}).to_string(),
            &bounds(&[]),
        )
        .unwrap();
        let logged = std::cell::Cell::new(0);
        let actual = check_with_log(
            &serde_json::json!({"entries": entries}).to_string(),
            &bounds(&[]),
            |n| {
                assert_eq!(n, 1);
                logged.set(logged.get() + 1);
            },
        )
        .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(logged.get(), 1);
        let mut discarded_invalid = serde_json::json!({"entries": entries});
        discarded_invalid["entries"][8] = serde_json::json!({"seq_from": "invalid"});
        assert_eq!(
            check(&discarded_invalid.to_string(), &bounds(&[])).unwrap(),
            expected
        );
        let mut invalid = serde_json::json!({"entries": entries});
        invalid["entries"][0]["seq_to"] = 999.into();
        assert!(check(&invalid.to_string(), &bounds(&[])).is_err());
    }

    #[test]
    fn every_rule_rejects_the_whole_answer() {
        let b = bounds(&["n1"]);
        for (what, answer) in [
            ("not json", "sorry, I cannot".to_string()),
            ("no entries key", r#"{"notes":[]}"#.to_string()),
            (
                "range below",
                entry("").replace("\"seq_from\":12", "\"seq_from\":2"),
            ),
            (
                "range above",
                entry("").replace("\"seq_to\":20", "\"seq_to\":51"),
            ),
            (
                "inverted",
                entry("").replace("\"seq_from\":12", "\"seq_from\":30"),
            ),
            ("no keywords", entry("").replace("[\"k\"]", "[]")),
            (
                "empty title",
                entry("").replace("\"title\":\"t\"", "\"title\":\" \""),
            ),
            (
                "empty conclusion",
                entry("").replace("\"conclusion\":\"c\"", "\"conclusion\":\"\""),
            ),
            (
                "unknown id",
                entry("").replace("\"id\":null", "\"id\":\"n7\""),
            ),
            (
                "title too long",
                entry("").replace(
                    "\"title\":\"t\"",
                    &format!("\"title\":\"{}\"", "x".repeat(81)),
                ),
            ),
        ] {
            assert!(check(&answer, &b).is_err(), "{what} should be rejected");
        }
        let dup = r#"{"entries":[{"id":"n1","title":"a","problem":"","cause":"","solution":"","conclusion":"c","keywords":["k"],"seq_from":12,"seq_to":13},{"id":"n1","title":"b","problem":"","cause":"","solution":"","conclusion":"c","keywords":["k"],"seq_from":14,"seq_to":15}]}"#;
        assert!(check(dup, &b).is_err());
    }

    #[test]
    fn secrets_in_the_answer_are_redacted_before_they_are_kept() {
        let answer = entry("").replace(
            "\"solution\":\"s\"",
            "\"solution\":\"set password=hunter2hunter2 now\"",
        );
        let checked = check(&answer, &bounds(&[])).unwrap();
        assert!(
            !checked[0].solution.contains("hunter2"),
            "{}",
            checked[0].solution
        );
    }

    #[test]
    fn merge_adds_new_entries_updates_named_ones_and_never_deletes() {
        let mut file = NotesFile::empty("claude", "s");
        let lookup = |seq: u64| {
            Some(Anchor {
                msg_key: Some(format!("key-{seq}")),
                at: Some("2026-10-16T00:00:00Z".into()),
                kind: "user".into(),
            })
        };
        let who = Authorship {
            round: "r1",
            at: "2026-10-16T03:00:00Z",
            engine: EngineInfo {
                cli: "claude".into(),
                cli_version: "1".into(),
                model: "m".into(),
            },
            anchor_of: &lookup,
        };
        let first = check(&entry(""), &bounds(&[])).unwrap();
        merge(&mut file, first, &who);
        merge(&mut file, check(&entry(""), &bounds(&[])).unwrap(), &who);
        assert_eq!(
            file.entries
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["n1", "n2"]
        );
        assert_eq!(file.entries[0].evidence.msg_key.as_deref(), Some("key-12"));

        let update = check(
            &entry("")
                .replace("\"id\":null", "\"id\":\"n1\"")
                .replace("\"title\":\"t\"", "\"title\":\"new\""),
            &bounds(&["n1", "n2"]),
        )
        .unwrap();
        merge(&mut file, update, &who);
        assert_eq!(file.entries.len(), 2, "an update replaces, never adds");
        assert_eq!(file.entries[0].title, "new");
        assert_eq!(
            file.entries[1].title, "t",
            "the entry it did not mention is untouched"
        );

        merge(&mut file, Vec::new(), &who);
        assert_eq!(file.entries.len(), 2, "an empty answer deletes nothing");
    }
}
