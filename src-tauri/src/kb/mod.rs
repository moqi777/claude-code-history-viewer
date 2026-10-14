//! `AgentKB`: background collection, verbatim backup and session index for
//! Claude Code and Codex history.
//!
//! Everything knowledge-base specific lives under this module so that merging
//! upstream changes touches as few base-project files as possible. The same
//! binary runs both with a window and headless (`--kb-sync`, `--kb-schedule`);
//! see [`cli::try_run`].

pub mod cli;
pub mod commands;
pub mod config;
pub mod db;
pub mod discover;
pub mod index;
pub mod lock;
pub mod log;
pub mod mirror;
pub mod paths;
pub mod schedule;
pub mod status;
pub mod sync;

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
