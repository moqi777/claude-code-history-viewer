//! KB settings, stored at `<config_dir>/agent-kb/config.json` so the headless
//! commands can read them without the GUI.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::providers;

pub const DEFAULT_INTERVAL_SECS: u64 = 600;
const CONFIG_DIR_NAME: &str = "agent-kb";
const CONFIG_FILE_NAME: &str = "config.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct KbConfig {
    /// Where `raw/`, `kb.db` and `logs/` live. `~/` is expanded on load.
    pub data_dir: PathBuf,
    /// Collection interval for the scheduled task.
    pub interval_secs: u64,
    /// Source paths starting with one of these are never collected.
    pub exclude_prefixes: Vec<String>,
    /// Overrides for the source roots; `None` means auto-detect.
    pub claude_root: Option<PathBuf>,
    pub codex_root: Option<PathBuf>,
}

impl Default for KbConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            interval_secs: DEFAULT_INTERVAL_SECS,
            exclude_prefixes: Vec::new(),
            claude_root: None,
            codex_root: None,
        }
    }
}

fn default_data_dir() -> PathBuf {
    crate::utils::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AgentKB")
}

pub fn config_path() -> Result<PathBuf, String> {
    crate::utils::config_dir()
        .map(|dir| dir.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
        .ok_or_else(|| "cannot determine the user config directory".to_string())
}

impl KbConfig {
    /// Missing file means defaults; a file that exists but is invalid is an
    /// error (silently falling back would send backups to the wrong place).
    pub fn load() -> Result<Self, String> {
        Self::load_from(&config_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let mut config: Self = serde_json::from_str(&text)
            .map_err(|e| format!("invalid config {}: {e}", path.display()))?;
        config.data_dir = expand_home(&config.data_dir);
        if config.interval_secs == 0 {
            config.interval_secs = DEFAULT_INTERVAL_SECS;
        }
        Ok(config)
    }
}

fn expand_home(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if text == "~" {
        return crate::utils::home_dir().unwrap_or_else(|| path.to_path_buf());
    }
    match text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        Some(rest) => crate::utils::home_dir().map_or_else(|| path.to_path_buf(), |h| h.join(rest)),
        None => path.to_path_buf(),
    }
}

/// Resolved source directories (`~/.claude`-like and `~/.codex`-like).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceRoots {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
}

impl SourceRoots {
    /// Claude: config override, then `CLAUDE_CONFIG_DIR` (headless mode has no
    /// frontend to register it), then the base default. Codex reuses the base
    /// provider logic, which already honours `CODEX_HOME`.
    pub fn resolve(config: &KbConfig) -> Self {
        let claude = config
            .claude_root
            .clone()
            .or_else(|| {
                std::env::var_os("CLAUDE_CONFIG_DIR")
                    .map(PathBuf::from)
                    .filter(|p| p.is_dir())
            })
            .or_else(|| providers::claude::get_base_path().map(PathBuf::from));
        let codex = config
            .codex_root
            .clone()
            .or_else(|| providers::codex::get_base_path().map(PathBuf::from));
        Self { claude, codex }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KbConfig::default()` resolves the home directory; the base crate's
    /// test guard demands an explicit sandbox for that (tests run with
    /// `--test-threads=1`, so the process-global variable is safe).
    fn sandbox_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("CCHV_TEST_HOME", home.path());
        home
    }

    #[test]
    fn missing_file_yields_defaults_and_partial_file_fills_in() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let defaults = KbConfig::load_from(&path).unwrap();
        assert_eq!(defaults.interval_secs, DEFAULT_INTERVAL_SECS);

        std::fs::write(
            &path,
            r#"{"interval_secs": 60, "exclude_prefixes": ["/x"]}"#,
        )
        .unwrap();
        let loaded = KbConfig::load_from(&path).unwrap();
        assert_eq!(loaded.interval_secs, 60);
        assert_eq!(loaded.exclude_prefixes, vec!["/x".to_string()]);
    }

    #[test]
    fn invalid_file_is_an_error_not_a_silent_default() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{nope").unwrap();
        assert!(KbConfig::load_from(&path).is_err());
    }

    #[test]
    fn zero_interval_falls_back() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"interval_secs": 0}"#).unwrap();
        assert_eq!(
            KbConfig::load_from(&path).unwrap().interval_secs,
            DEFAULT_INTERVAL_SECS
        );
    }
}
