//! Semantic (vector) search, phase 6.
//!
//! Everything runs on this machine. The model is downloaded once, only when
//! the user asks for it (`model`), and checked against a SHA-256 written into
//! the program. Text is cut into chunks (`chunk`), turned into vectors by
//! ONNX Runtime (`embed`), kept in a file of its own next to `kb.db`
//! (`store`) and built a little at a time after each collection (`build`).
//! Searching merges the vector hits with the keyword hits (`rank`) and falls
//! back to the keyword result whenever anything is missing.
//!
//! The design and its measurements: `docs/agent-kb/P6-design.md`.

pub mod build;
pub mod chunk;
pub mod commands;
pub mod corpus;
pub mod embed;
pub mod model;
pub mod rank;
pub mod state;
pub mod store;
pub mod sys;

use serde::Serialize;

/// Names the vector file (`vectors/<MODEL_ID>.sqlite3`). Bump when the model
/// or the way vectors are computed changes: old vectors then stop being used.
pub const MODEL_ID: &str = "bge-m3-int8-v1";
/// Folder of the model files under `<data dir>/models/`.
pub const MODEL_DIR: &str = "bge-m3-int8";
pub const DIM: usize = 1024;
/// Bump when [`chunk`] cuts differently: every vector is then recomputed.
pub const CHUNKER_VERSION: u32 = 1;
/// Longest input in tokens.
pub const MAX_TOKENS: usize = 256;

/// Why a search did not use the vectors, or that it did. Always in the
/// `semantic` key of a search response.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UseState {
    Used,
    Off,
    ModelMissing,
    ModelCorrupt,
    IndexBuilding,
    IndexEmpty,
    Skipped,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SemanticInfo {
    pub state: UseState,
    pub indexed: usize,
    pub total: usize,
    pub note: String,
}

impl SemanticInfo {
    pub fn off(note: &str) -> Self {
        Self {
            state: UseState::Off,
            indexed: 0,
            total: 0,
            note: note.to_string(),
        }
    }
}
