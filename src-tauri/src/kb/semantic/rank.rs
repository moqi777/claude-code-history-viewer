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
/// … and only those within this much (cosine) of the best one.
pub const MARGIN: f32 = 0.1;
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

/// Per session: best piece + a quarter of the second best; best sessions
/// first, at most [`TOP_SESSIONS`], none further than [`MARGIN`] from the top.
pub fn aggregate(matrix: &Matrix, scores: &[f32]) -> Vec<VecSession> {
    let mut best: HashMap<(&str, &str), (f32, f32, usize)> = HashMap::new();
    for (i, s) in scores.iter().enumerate() {
        let (src, id, _, _) = &matrix.owner[i];
        let e = best
            .entry((src.as_str(), id.as_str()))
            .or_insert((f32::MIN, f32::MIN, i));
        if *s > e.0 {
            e.1 = e.0;
            e.0 = *s;
            e.2 = i;
        } else if *s > e.1 {
            e.1 = *s;
        }
    }
    let mut v: Vec<VecSession> = best
        .into_iter()
        .map(|((src, id), (b, second, at))| VecSession {
            source: src.to_string(),
            session_id: id.to_string(),
            score: b + if second > f32::MIN {
                SECOND_WEIGHT * second
            } else {
                0.0
            },
            best: (matrix.owner[at].2.clone(), matrix.owner[at].3.clone()),
        })
        .collect();
    v.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    let top = v.first().map_or(0.0, |s| s.score);
    v.retain(|s| s.score >= top - MARGIN);
    v.truncate(TOP_SESSIONS);
    v
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
        let (m, s) = matrix(&[("c", "a", "t", 0.8), ("c", "b", "t", 0.5)]);
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
        assert!(input.sessions.is_empty());
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
}
