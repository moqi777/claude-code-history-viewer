//! Progress reporting for long operations (collect, backup, migration).
//!
//! The core code only knows a [`ProgressSink`]; the GUI commands plug in one
//! that emits the `kb-progress` event, headless runs plug in nothing.

use std::sync::Arc;

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Progress {
    /// `sync`, `backup` or `migrate`.
    pub op: String,
    /// Human-meaningful stage inside the operation.
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

type Callback = dyn Fn(&Progress) + Send + Sync;

#[derive(Clone, Default)]
pub struct ProgressSink(Option<Arc<Callback>>);

impl ProgressSink {
    pub fn new(callback: impl Fn(&Progress) + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(callback)))
    }

    pub fn emit(&self, progress: &Progress) {
        if let Some(callback) = &self.0 {
            callback(progress);
        }
    }
}

impl std::fmt::Debug for ProgressSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ProgressSink(set)"
        } else {
            "ProgressSink(none)"
        })
    }
}
