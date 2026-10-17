//! What the settings page and `--kb-embed --status` show: a small JSON file
//! next to the vectors, rewritten atomically by whoever builds.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::store;
use crate::kb::paths::DataPaths;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EmbedState {
    /// `idle`, `running`, `postponed_memory`, `paused_model`, `error`.
    pub status: String,
    pub total: usize,
    pub done: usize,
    pub updated_at: String,
    pub message: String,
    /// Rounds put off because the machine was short of memory, since the last
    /// time a round ran.
    pub postponed_rounds: u32,
    pub last_run: Option<RunReport>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunReport {
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub embedded: usize,
    pub seconds: f64,
    pub finished_at: String,
}

pub fn path(data: &DataPaths) -> PathBuf {
    store::dir(data).join("embed_state.json")
}

pub fn read(data: &DataPaths) -> EmbedState {
    std::fs::read_to_string(path(data))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write(data: &DataPaths, state: &EmbedState) {
    let p = path(data);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string_pretty(state) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
