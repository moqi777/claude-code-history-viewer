//! `AgentKB`: background collection, verbatim backup and session index for
//! Claude Code and Codex history.
//!
//! Everything knowledge-base specific lives under this module so that merging
//! upstream changes touches as few base-project files as possible. The same
//! binary runs both with a window and headless (`--kb-sync`, `--kb-schedule`);
//! see [`cli::try_run`].

pub mod backup;
pub mod cli;
pub mod commands;
pub mod config;
pub mod db;
pub mod digest;
pub mod discover;
pub mod export;
pub mod fsutil;
pub mod import;
pub mod index;
pub mod legacy;
pub mod lock;
pub mod log;
pub mod merge;
pub mod migrate_dir;
pub mod mirror;
pub mod notes;
pub mod pack;
pub mod paths;
pub mod probe;
pub mod progress;
pub mod redact;
pub mod schedule;
pub mod search;
pub mod semantic;
pub mod skill;
pub mod status;
pub mod sync;
pub mod transfer;
pub mod transfer_commands;
pub mod volume;

/// `AgentKB`'s own version, independent of the base viewer's `package.json`
/// version (which stays in step with upstream). Phase N ships `0.N.0`.
pub const AGENTKB_VERSION: &str = "0.6.0";

/// Where a session file came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Claude,
    Codex,
}

impl Source {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[cfg(test)]
mod import_tests;
