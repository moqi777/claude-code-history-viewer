//! Vector search and the merge with the keyword result.
//!
//! The constants below were chosen on the P6 test set during the design and
//! are FIXED from here on (leader's review of the design): changing them to
//! chase the test set would only measure how well they were tuned to it.

use std::collections::HashMap;
use std::path::Path;

use super::embed::{Embedder, OrtEmbedder};
use super::store::{Matrix, Store};
use super::{model, state, SemanticInfo, UseState};
use crate::kb::paths::DataPaths;

/// Sessions kept from the vector side.
pub const TOP_SESSIONS: usize = 10;
/// … and only those within this much of the best one (after the size correction below). The
/// result no longer changes once this is 0.2 (a copy of the library) or 0.35 (a later one, whose
/// best session scored higher); 0.5 sits well on that plateau and still drops the hopeless.
pub const MARGIN: f32 = 0.5;
/// Longer sessions have more pieces, so some piece is close to any question by chance. Over 70
/// questions and every session the score grew by 0.034 per unit of ln(pieces) whatever was asked
/// (P6.1-design.md §3.1); three quarters of that is taken off, not all of it, because a long
/// session also really has more to match. Only the order is corrected, the reported score is not.
pub const SIZE_PENALTY: f32 = 0.025;
/// A session's score: its best piece plus this much of its second best.
pub const SECOND_WEIGHT: f32 = 0.25;
/// Reciprocal rank fusion: `weight / (RRF_K + rank)`.
pub const RRF_K: f64 = 10.0;
/// Keyword hits taken into the merge.
pub const KEYWORD_DEPTH: usize = 20;
/// Weight of the keyword list when every query word matched …
pub const W_KEYWORD_ALL: f64 = 1.0;
/// … and when only some did (a looser, noisier result).
pub const W_KEYWORD_ANY: f64 = 0.5;
pub const W_VECTOR: f64 = 1.0;
/// A query loads the model (about 2 GB); with less than this to spare it is skipped.
pub const MIN_QUERY_BYTES: u64 = 2560 * 1024 * 1024;
/// How a query gets its embedder (a stand-in in tests).
pub type MakeEmbedder<'a> = dyn Fn(&Path) -> Result<Box<dyn Embedder>, String> + 'a;
/// Threads for embedding one query.
const QUERY_THREADS: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct VecSession {
    pub source: String,
    pub session_id: String,
    pub score: f32,
    /// `(kind, key)` of the piece that matched best.
    pub best: (String, String),
}

#[derive(Debug, Clone)]
pub struct SemanticInput {
    pub info: SemanticInfo,
    pub sessions: Vec<VecSession>,
}

impl SemanticInput {
    pub fn unused(state: UseState, note: &str) -> Self {
        Self {
            info: SemanticInfo {
                state,
                indexed: 0,
                total: 0,
                note: note.to_string(),
            },
            sessions: Vec::new(),
        }
    }
    pub fn off() -> Self {
        Self::unused(UseState::Off, "semantic search is off")
    }
}

/// Per session: best piece + a quarter of the second best, less [`SIZE_PENALTY`] × ln(pieces the
/// session has) for the order; best sessions first, at most [`TOP_SESSIONS`], none further than
/// [`MARGIN`] from the top.
pub fn aggregate(matrix: &Matrix, scores: &[f32]) -> Vec<VecSession> {
    let mut best: HashMap<(&str, &str), (f32, f32, usize, u32)> = HashMap::new();
    for (i, s) in scores.iter().enumerate() {
        let (src, id, _, _) = &matrix.owner[i];
        let e = best
            .entry((src.as_str(), id.as_str()))
            .or_insert((f32::MIN, f32::MIN, i, 0));
        e.3 += 1;
        if *s > e.0 {
            e.1 = e.0;
            e.0 = *s;
            e.2 = i;
        } else if *s > e.1 {
            e.1 = *s;
        }
    }
    // (session, the score the order and the margin use)
    let mut v: Vec<(VecSession, f32)> = best
        .into_iter()
        .map(|((src, id), (b, second, at, pieces))| {
            let score = b + if second > f32::MIN {
                SECOND_WEIGHT * second
            } else {
                0.0
            };
            let session = VecSession {
                source: src.to_string(),
                session_id: id.to_string(),
                score,
                best: (matrix.owner[at].2.clone(), matrix.owner[at].3.clone()),
            };
            (session, score - SIZE_PENALTY * (pieces as f32).ln())
        })
        .collect();
    v.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.session_id.cmp(&b.0.session_id))
    });
    let top = v.first().map_or(0.0, |s| s.1);
    v.retain(|s| s.1 >= top - MARGIN);
    v.truncate(TOP_SESSIONS);
    v.into_iter().map(|(session, _)| session).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fused {
    pub source: String,
    pub session_id: String,
    pub score: f64,
    pub in_keyword: bool,
    pub in_vector: bool,
}

/// Weighted reciprocal rank fusion of the two lists (both best first).
pub fn fuse(
    keyword: &[(String, String)],
    keyword_all: bool,
    vector: &[(String, String)],
) -> Vec<Fused> {
    let wk = if keyword_all {
        W_KEYWORD_ALL
    } else {
        W_KEYWORD_ANY
    };
    let mut by_key: HashMap<(String, String), Fused> = HashMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    for (rank, key) in keyword.iter().take(KEYWORD_DEPTH).enumerate() {
        let e = by_key.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            Fused {
                source: key.0.clone(),
                session_id: key.1.clone(),
                score: 0.0,
                in_keyword: false,
                in_vector: false,
            }
        });
        e.score += wk / (RRF_K + rank as f64 + 1.0);
        e.in_keyword = true;
    }
    for (rank, key) in vector.iter().enumerate() {
        let e = by_key.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            Fused {
                source: key.0.clone(),
                session_id: key.1.clone(),
                score: 0.0,
                in_keyword: false,
                in_vector: false,
            }
        });
        e.score += W_VECTOR / (RRF_K + rank as f64 + 1.0);
        e.in_vector = true;
    }
    // `order` is the first-seen order, the tie-break (keyword first).
    let mut out: Vec<Fused> = order
        .into_iter()
        .filter_map(|k| by_key.remove(&k))
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// What the query needs from the vector side. Never fails: anything missing
/// becomes a [`SemanticInput`] with no sessions and the reason.
pub fn prepare_with(
    data: &DataPaths,
    spec: &model::ModelSpec,
    enabled: bool,
    kinds_filtered: bool,
    query: &str,
    make: &MakeEmbedder<'_>,
) -> SemanticInput {
    if !enabled {
        return SemanticInput::off();
    }
    if kinds_filtered {
        return SemanticInput::unused(
            UseState::Skipped,
            "the kinds filter applies to messages only; keywords used",
        );
    }
    match model::check(data, spec) {
        model::ModelStatus::Ready => {}
        model::ModelStatus::Missing => {
            return SemanticInput::unused(
                UseState::ModelMissing,
                "the model is not downloaded; keywords used",
            )
        }
        model::ModelStatus::Corrupt { file, reason } => {
            return SemanticInput::unused(
                UseState::ModelCorrupt,
                &format!("model file {file}: {reason}; keywords used"),
            )
        }
    }
    let store = match Store::open_ro(&super::store::path(data), model::model_sha()) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return SemanticInput::unused(
                UseState::IndexEmpty,
                "no vector index yet; keywords used",
            )
        }
        Err(e) => return SemanticInput::unused(UseState::Error, &format!("{e}; keywords used")),
    };
    let matrix = match store.load() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => {
            return SemanticInput::unused(
                UseState::IndexEmpty,
                "the vector index is empty; keywords used",
            )
        }
        Err(e) => return SemanticInput::unused(UseState::Error, &format!("{e}; keywords used")),
    };
    let st = state::read(data);
    let indexed = matrix.len();
    let total = st.total.max(indexed);
    let query_vector = match make(&model::dir(data)).and_then(|mut e| e.embed_query(query)) {
        Ok(v) => v,
        Err(e) => return SemanticInput::unused(UseState::Error, &format!("{e}; keywords used")),
    };
    let scores = matrix.scores(&query_vector);
    let building = indexed < total;
    SemanticInput {
        info: SemanticInfo {
            state: if building {
                UseState::IndexBuilding
            } else {
                UseState::Used
            },
            indexed,
            total,
            note: if building {
                format!(
                    "the semantic index is {}% built; results may be incomplete",
                    indexed * 100 / total.max(1)
                )
            } else {
                String::new()
            },
        },
        sessions: aggregate(&matrix, &scores),
    }
}

/// `prepare_with` for the real program: reads the setting, loads the real model.
pub fn prepare(
    data: &DataPaths,
    enabled: bool,
    kinds_filtered: bool,
    query: &str,
) -> SemanticInput {
    if enabled {
        if let Some(free) = super::sys::available_memory().filter(|f| *f < MIN_QUERY_BYTES) {
            return SemanticInput::unused(
                UseState::Skipped,
                &format!(
                    "only {} MB of memory is free; keywords used",
                    free / 1_048_576
                ),
            );
        }
    }
    prepare_with(data, &model::SPEC, enabled, kinds_filtered, query, &|dir| {
        OrtEmbedder::load(dir, QUERY_THREADS).map(|e| Box::new(e) as Box<dyn Embedder>)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::semantic::DIM;

    fn matrix(rows: &[(&str, &str, &str, f32)]) -> (Matrix, Vec<f32>) {
        let mut m = Matrix::default();
        let mut s = Vec::new();
        for (src, id, key, score) in rows {
            m.owner.push((
                (*src).to_string(),
                (*id).to_string(),
                "user".into(),
                (*key).to_string(),
            ));
            m.scale.push(1.0);
            m.data.extend(std::iter::repeat(0i8).take(DIM));
            s.push(*score);
        }
        (m, s)
    }

    #[test]
    fn sessions_score_best_plus_a_quarter_of_second() {
        let (m, s) = matrix(&[
            ("c", "a", "m:1:0", 0.8),
            ("c", "a", "m:2:0", 0.6),
            ("c", "b", "m:1:0", 0.9),
        ]);
        let v = aggregate(&m, &s);
        // a: 0.8 + 0.25 * 0.6 = 0.95 beats b: 0.9 alone
        assert_eq!(
            v.iter().map(|x| x.session_id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!((v[0].score - (0.8 + 0.25 * 0.6)).abs() < 1e-6);
        assert_eq!(v[0].best.1, "m:1:0");
    }

    #[test]
    fn far_behind_sessions_are_not_candidates() {
        let (m, s) = matrix(&[("c", "a", "t", 0.9), ("c", "b", "t", 0.3)]);
        let v = aggregate(&m, &s);
        assert_eq!(v.len(), 1);
        let rows: Vec<(&str, &str, &str, f32)> = (0..30)
            .map(|i| {
                (
                    "c",
                    Box::leak(format!("s{i}").into_boxed_str()) as &str,
                    "t",
                    0.8,
                )
            })
            .collect();
        let (m, s) = matrix(&rows);
        assert_eq!(aggregate(&m, &s).len(), TOP_SESSIONS);
    }

    #[test]
    fn a_session_with_many_pieces_does_not_win_on_size_alone() {
        // 'big' has 200 pieces, the best of them a little closer than the only piece of 'small'
        let mut rows: Vec<(&str, &str, &str, f32)> = vec![("c", "big", "m:0:0", 0.78)];
        let keys: Vec<String> = (1..200).map(|i| format!("m:{i}:0")).collect();
        for key in &keys {
            rows.push(("c", "big", key.as_str(), 0.4));
        }
        rows.push(("c", "small", "m:1:0", 0.76));
        let (m, s) = matrix(&rows);
        let v = aggregate(&m, &s);
        assert_eq!(
            v.iter().map(|x| x.session_id.as_str()).collect::<Vec<_>>(),
            ["small", "big"],
            "ln(200) * 0.025 = 0.13 is taken off big"
        );
        let big = v.iter().find(|x| x.session_id == "big").unwrap();
        assert!(
            (big.score - (0.78 + 0.25 * 0.4)).abs() < 1e-6,
            "the reported score is the plain one"
        );
    }

    #[test]
    fn the_margin_is_measured_after_the_correction() {
        let (m, s) = matrix(&[("c", "a", "t", 0.9), ("c", "b", "t", 0.41)]);
        assert_eq!(
            aggregate(&m, &s).len(),
            2,
            "0.49 behind is still a candidate"
        );
        let (m, s) = matrix(&[("c", "a", "t", 0.9), ("c", "b", "t", 0.39)]);
        assert_eq!(aggregate(&m, &s).len(), 1, "0.51 behind is not");
    }

    fn k(id: &str) -> (String, String) {
        ("c".to_string(), id.to_string())
    }

    #[test]
    fn a_session_in_both_lists_beats_one_in_either() {
        let f = fuse(&[k("a"), k("b")], true, &[k("c"), k("b")]);
        assert_eq!(f[0].session_id, "b");
        assert!(f[0].in_keyword && f[0].in_vector);
        assert_eq!(f.len(), 3);
    }

    #[test]
    fn loose_keyword_results_count_half() {
        // vector top-1 against keyword top-1: a tie when every word matched, a win for the vector when not
        let strict = fuse(&[k("kw")], true, &[k("vec")]);
        assert_eq!(strict[0].session_id, "kw");
        let loose = fuse(&[k("kw")], false, &[k("vec")]);
        assert_eq!(loose[0].session_id, "vec");
    }

    #[test]
    fn keyword_list_is_cut_at_depth() {
        let kw: Vec<_> = (0..30).map(|i| k(&format!("k{i}"))).collect();
        assert_eq!(fuse(&kw, true, &[]).len(), KEYWORD_DEPTH);
    }

    use crate::kb::semantic::embed::fake::FakeEmbedder;

    fn unusable(input: &SemanticInput, state: UseState) {
        assert_eq!(input.info.state, state, "{:?}", input.info);
        assert_eq!(input.sessions.len(), 0);
    }

    #[test]
    fn every_missing_piece_falls_back_with_a_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataPaths::new(tmp.path());
        let fake: &MakeEmbedder<'_> = &|_| Ok(Box::new(FakeEmbedder::default()));
        unusable(
            &prepare_with(&data, &model::SPEC, false, false, "q", fake),
            UseState::Off,
        );
        unusable(
            &prepare_with(&data, &model::SPEC, true, true, "q", fake),
            UseState::Skipped,
        );
        unusable(
            &prepare_with(&data, &model::SPEC, true, false, "q", fake),
            UseState::ModelMissing,
        );
    }

    // ---- the whole path with a stand-in embedder -----------------------

    use crate::kb::semantic::chunk::{text_sha, Chunk};
    use crate::kb::semantic::embed::fake::vector_of;

    const MODEL: &[u8] = b"model bytes";

    fn data_with_model() -> (tempfile::TempDir, DataPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataPaths::new(tmp.path());
        std::fs::create_dir_all(model::dir(&data)).unwrap();
        std::fs::write(model::dir(&data).join("model.onnx"), MODEL).unwrap();
        (tmp, data)
    }

    fn fill(data: &DataPaths, rows: &[(&str, &str, &str)]) {
        let mut store = Store::open(&super::super::store::path(data), model::model_sha()).unwrap();
        let chunks: Vec<Chunk> = rows
            .iter()
            .map(|(session, key, text)| Chunk {
                source: "claude".into(),
                session_id: (*session).into(),
                key: (*key).into(),
                kind: "user",
                text: (*text).into(),
                sha: text_sha(text),
            })
            .collect();
        let pairs: Vec<(&Chunk, Vec<f32>)> =
            chunks.iter().map(|c| (c, vector_of(&c.text))).collect();
        store.put(&pairs).unwrap();
    }

    fn ask(data: &DataPaths, query: &str) -> SemanticInput {
        let fake: &MakeEmbedder<'_> = &|_| Ok(Box::new(FakeEmbedder::default()));
        prepare_with(data, model::testing::spec(MODEL), true, false, query, fake)
    }

    #[test]
    fn a_damaged_model_an_empty_index_and_a_failing_embedder_each_fall_back() {
        let (_t, data) = data_with_model();
        unusable(&ask(&data, "q"), UseState::IndexEmpty);
        fill(&data, &[("s1", "m:1:0", "battery drains fast overnight")]);
        let broken: &MakeEmbedder<'_> = &|_| Err("cannot load".into());
        let input = prepare_with(&data, model::testing::spec(MODEL), true, false, "q", broken);
        unusable(&input, UseState::Error);
        assert!(input.info.note.contains("keywords used"));
        std::fs::write(model::dir(&data).join("model.onnx"), b"model bytez").unwrap();
        unusable(&ask(&data, "q"), UseState::ModelCorrupt);
    }

    #[test]
    fn a_complete_index_finds_the_session_whose_words_match() {
        let (_t, data) = data_with_model();
        fill(
            &data,
            &[
                ("s1", "m:1:0", "battery drains fast overnight"),
                ("s2", "m:1:0", "nginx returns a bad gateway"),
                ("s3", "t", "terminal opens in a given folder"),
            ],
        );
        crate::kb::semantic::state::write(
            &data,
            &crate::kb::semantic::state::EmbedState {
                total: 3,
                done: 3,
                ..Default::default()
            },
        );
        let input = ask(&data, "battery drains");
        assert_eq!(input.info.state, UseState::Used);
        assert_eq!((input.info.indexed, input.info.total), (3, 3));
        assert_eq!(input.sessions[0].session_id, "s1");
        assert_eq!(
            input.sessions[0].best,
            ("user".to_string(), "m:1:0".to_string())
        );
    }

    #[test]
    fn a_half_built_index_is_used_and_says_so() {
        let (_t, data) = data_with_model();
        fill(&data, &[("s1", "m:1:0", "battery drains fast overnight")]);
        crate::kb::semantic::state::write(
            &data,
            &crate::kb::semantic::state::EmbedState {
                total: 4,
                done: 1,
                ..Default::default()
            },
        );
        let input = ask(&data, "battery drains");
        assert_eq!(input.info.state, UseState::IndexBuilding);
        assert_eq!((input.info.indexed, input.info.total), (1, 4));
        assert!(input.info.note.contains("25%"), "{}", input.info.note);
        assert_eq!(input.sessions.len(), 1);
    }

    #[test]
    fn the_state_is_written_as_snake_case_words() {
        let v =
            serde_json::to_value(SemanticInput::unused(UseState::ModelMissing, "x").info).unwrap();
        assert_eq!(v["state"], "model_missing");
    }
}
