//! KB settings, stored at `<config_dir>/agent-kb/config.json` so the headless
//! commands and the GUI read the very same file through [`KbConfig::load`].
//!
//! The file is versioned (`version`). Older files are upgraded in memory on
//! load and only rewritten when the user saves; a file written by a newer
//! program is refused rather than overwritten.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::fsutil;
use crate::providers;

pub const CONFIG_VERSION: u32 = 1;
pub const DEFAULT_INTERVAL_SECS: u64 = 600;
pub const MIN_INTERVAL_SECS: u64 = 60;
pub const MAX_INTERVAL_SECS: u64 = 86_400;
const CONFIG_DIR_NAME: &str = "agent-kb";
const CONFIG_FILE_NAME: &str = "config.json";

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SourceConfig {
    pub enabled: bool,
    /// `None` = auto-detect.
    pub root: Option<PathBuf>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            root: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SourcesConfig {
    pub claude: SourceConfig,
    pub codex: SourceConfig,
}

/// Second copy on an external disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MirrorConfig {
    pub enabled: bool,
    pub target_dir: Option<PathBuf>,
    /// Recorded when the disk was chosen; the only thing that identifies it.
    pub volume_uuid: Option<String>,
    /// Display only.
    pub volume_name: Option<String>,
}

/// The daily digest (`--kb-digest`). Only the first five fields are exposed
/// in the settings page; the others are tuning knobs kept in `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DigestConfig {
    /// Off until the user turns it on: the digest spends their own CLI quota.
    pub enabled: bool,
    /// `claude` or `codex`.
    pub engine: String,
    /// Empty = the CLI's own default model.
    pub model: String,
    /// Absolute path of the CLI. Background tasks have no `PATH`, so the
    /// settings page resolves it once.
    pub cli_path: Option<PathBuf>,
    /// Local `HH:MM`: the cut-off between two rounds.
    pub time: String,
    pub max_sessions_per_round: usize,
    /// Automatic rounds only look at sessions active this many days back.
    pub recent_days: u32,
    /// A session with a message newer than this is left for the next round.
    pub quiet_minutes: u32,
    /// Fewest user + assistant messages a new session needs.
    pub min_messages: usize,
    pub max_chunks_per_session: usize,
    /// Codex only: its `model_reasoning_effort` (`low`, `medium`, `high`),
    /// empty = the CLI's own default. Not in the settings page.
    pub codex_reasoning: String,
}

impl Default for DigestConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: "claude".into(),
            model: String::new(),
            cli_path: None,
            time: "03:00".into(),
            max_sessions_per_round: 30,
            recent_days: 7,
            quiet_minutes: 30,
            min_messages: 4,
            max_chunks_per_session: 3,
            codex_reasoning: "low".into(),
        }
    }
}

impl DigestConfig {
    /// `(hour, minute)` of [`Self::time`].
    pub fn time_of_day(&self) -> Option<(u32, u32)> {
        let (h, m) = self.time.split_once(':')?;
        let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
        (h < 24 && m < 60 && self.time.len() == 5).then_some((h, m))
    }

    fn validate(&self) -> Result<(), String> {
        if !matches!(self.engine.as_str(), "claude" | "codex") {
            return Err(format!(
                "digest.engine must be claude or codex, got {:?}",
                self.engine
            ));
        }
        if self.time_of_day().is_none() {
            return Err(format!("digest.time must be HH:MM, got {:?}", self.time));
        }
        if !(1..=500).contains(&self.max_sessions_per_round) {
            return Err("digest.max_sessions_per_round must be between 1 and 500".into());
        }
        if !(1..=3650).contains(&self.recent_days) {
            return Err("digest.recent_days must be between 1 and 3650".into());
        }
        if self.max_chunks_per_session == 0 || self.max_chunks_per_session > 50 {
            return Err("digest.max_chunks_per_session must be between 1 and 50".into());
        }
        if !matches!(
            self.codex_reasoning.as_str(),
            "" | "minimal" | "low" | "medium" | "high"
        ) {
            return Err(
                "digest.codex_reasoning must be empty, minimal, low, medium or high".into(),
            );
        }
        if self.model.len() > 100 || self.model.chars().any(char::is_control) {
            return Err("digest.model is not a valid model name".into());
        }
        if let Some(path) = &self.cli_path {
            if !super::paths::is_absolute(path) {
                return Err("digest.cli_path must be an absolute path".into());
            }
        }
        Ok(())
    }
}

/// Semantic search (phase 6). Off until the user has downloaded the model,
/// tested it and switched it on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SemanticConfig {
    pub enabled: bool,
    /// Where "download model" fetches from: `modelscope` or `huggingface`.
    pub source: String,
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            source: "modelscope".into(),
        }
    }
}

impl SemanticConfig {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.source.as_str(), "modelscope" | "huggingface") {
            return Err("semantic.source must be modelscope or huggingface".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct KbConfig {
    pub version: u32,
    /// Where `raw/`, `notes/`, `kb.db` and `logs/` live. `~/` is expanded on load.
    pub data_dir: PathBuf,
    /// Collection interval for the scheduled task.
    pub interval_secs: u64,
    /// Whether the app maintains the launchd task.
    #[serde(default = "default_true")]
    pub schedule_enabled: bool,
    pub sources: SourcesConfig,
    /// Source paths starting with one of these are never collected.
    pub exclude_prefixes: Vec<String>,
    /// Sessions whose working directory is, or is below, one of these stay
    /// in the backup but are left out of search and the session list.
    pub exclude_cwd_prefixes: Vec<String>,
    pub mirror: MirrorConfig,
    pub digest: DigestConfig,
    pub semantic: SemanticConfig,
    /// Keys this version does not know, kept so a newer/older sibling program
    /// does not lose them when we save.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for KbConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            data_dir: default_data_dir(),
            interval_secs: DEFAULT_INTERVAL_SECS,
            schedule_enabled: true,
            sources: SourcesConfig::default(),
            exclude_prefixes: Vec::new(),
            exclude_cwd_prefixes: Vec::new(),
            mirror: MirrorConfig::default(),
            digest: DigestConfig::default(),
            semantic: SemanticConfig::default(),
            extra: Map::new(),
        }
    }
}

pub fn default_data_dir() -> PathBuf {
    crate::utils::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AgentKB")
}

pub fn config_path() -> Result<PathBuf, String> {
    crate::utils::config_dir()
        .map(|dir| dir.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
        .ok_or_else(|| "cannot determine the user config directory".to_string())
}

/// What the GUI needs to know before it decides to show the wizard.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConfigState {
    Missing,
    Valid,
    Invalid { message: String, path: String },
    Newer { file_version: u64, path: String },
}

enum Loaded {
    Missing,
    Ok(Box<KbConfig>),
    Invalid(String),
    Newer(u64),
}

fn read_config(path: &Path) -> Loaded {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return Loaded::Invalid(format!("cannot read {}: {e}", path.display())),
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => return Loaded::Invalid(format!("not valid JSON: {e}")),
    };
    let (value, file_version) = match upgrade(value) {
        Ok(pair) => pair,
        Err(message) => return Loaded::Invalid(message),
    };
    if file_version > u64::from(CONFIG_VERSION) {
        return Loaded::Newer(file_version);
    }
    let mut config: KbConfig = match serde_json::from_value(value) {
        Ok(config) => config,
        Err(e) => return Loaded::Invalid(format!("unexpected content: {e}")),
    };
    config.data_dir = expand_home(&config.data_dir);
    match config.validate() {
        Ok(()) => Loaded::Ok(Box::new(config)),
        Err(message) => Loaded::Invalid(message),
    }
}

/// Bring any known older layout up to [`CONFIG_VERSION`], in memory only.
/// Returns the file's own version (a newer one is passed through untouched).
fn upgrade(value: Value) -> Result<(Value, u64), String> {
    let Value::Object(mut map) = value else {
        return Err("the top level must be a JSON object".into());
    };
    let version = match map.get("version") {
        None => 0,
        Some(v) => v.as_u64().ok_or("`version` must be a whole number")?,
    };
    if version > u64::from(CONFIG_VERSION) {
        return Ok((Value::Object(map), version));
    }
    if version == 0 {
        migrate_v0_to_v1(&mut map);
    }
    Ok((Value::Object(map), version))
}

/// P1 wrote `claude_root` / `codex_root` at the top level.
fn migrate_v0_to_v1(map: &mut Map<String, Value>) {
    let mut sources = match map.remove("sources") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for (old, new) in [("claude_root", "claude"), ("codex_root", "codex")] {
        if let Some(root) = map.remove(old) {
            let entry = sources
                .entry(new.to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(entry) = entry {
                entry.entry("root".to_string()).or_insert(root);
            }
        }
    }
    if !sources.is_empty() {
        map.insert("sources".into(), Value::Object(sources));
    }
    map.insert("version".into(), Value::from(CONFIG_VERSION));
}

impl KbConfig {
    /// Missing file means defaults (the headless task keeps working for users
    /// who never opened the wizard); a file that exists but is invalid is an
    /// error, because silently falling back would send backups to the wrong place.
    pub fn load() -> Result<Self, String> {
        Self::load_from(&config_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        match read_config(path) {
            Loaded::Missing => Ok(Self::default()),
            Loaded::Ok(config) => Ok(*config),
            Loaded::Invalid(message) => {
                Err(format!("invalid config {}: {message}", path.display()))
            }
            Loaded::Newer(v) => Err(format!(
                "config {} was written by a newer AgentKB (version {v}); update the app",
                path.display()
            )),
        }
    }

    /// Like [`load`](Self::load) but distinguishes "no file" for the wizard.
    pub fn state() -> Result<ConfigState, String> {
        Ok(Self::state_of(&config_path()?))
    }

    pub fn state_of(path: &Path) -> ConfigState {
        let shown = path.display().to_string();
        match read_config(path) {
            Loaded::Missing => ConfigState::Missing,
            Loaded::Ok(_) => ConfigState::Valid,
            Loaded::Invalid(message) => ConfigState::Invalid {
                message,
                path: shown,
            },
            Loaded::Newer(file_version) => ConfigState::Newer {
                file_version,
                path: shown,
            },
        }
    }

    /// Validate, then write atomically. Never leaves a half-written file.
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&config_path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let mut to_write = self.clone();
        to_write.version = CONFIG_VERSION;
        // A disabled backup disk leaves no trace of whichever disk was once picked.
        if !to_write.mirror.enabled {
            to_write.mirror.target_dir = None;
            to_write.mirror.volume_uuid = None;
            to_write.mirror.volume_name = None;
        }
        let json = serde_json::to_string_pretty(&to_write).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        fsutil::write_atomic(path, json.as_bytes())
    }

    /// Set a broken or too-new file aside (never deleted) so the app can start
    /// over. Returns where it went.
    pub fn set_aside(path: &Path) -> Result<PathBuf, String> {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let mut target = path.with_file_name(format!("{CONFIG_FILE_NAME}.corrupt-{stamp}"));
        let mut n = 1;
        while target.exists() {
            target = path.with_file_name(format!("{CONFIG_FILE_NAME}.corrupt-{stamp}-{n}"));
            n += 1;
        }
        std::fs::rename(path, &target)
            .map_err(|e| format!("cannot move {} aside: {e}", path.display()))?;
        Ok(target)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&self.interval_secs) {
            return Err(format!(
                "interval_secs must be between {MIN_INTERVAL_SECS} and {MAX_INTERVAL_SECS}, got {}",
                self.interval_secs
            ));
        }
        if self.data_dir.as_os_str().is_empty() || !super::paths::is_absolute(&self.data_dir) {
            return Err(format!(
                "data_dir must be an absolute path, got {}",
                self.data_dir.display()
            ));
        }
        for prefix in &self.exclude_prefixes {
            if !super::paths::is_absolute(Path::new(prefix)) {
                return Err(format!("exclude path {prefix:?} must be absolute"));
            }
        }
        for prefix in &self.exclude_cwd_prefixes {
            if !super::paths::is_absolute(Path::new(prefix)) {
                return Err(format!(
                    "excluded working directory {prefix:?} must be absolute"
                ));
            }
        }
        self.digest.validate()?;
        self.semantic.validate()?;
        let roots = SourceRoots::resolve(self);
        for root in [&roots.claude, &roots.codex].into_iter().flatten() {
            if self.data_dir.starts_with(root) || root.starts_with(&self.data_dir) {
                return Err(format!(
                    "data_dir {} must not contain or sit inside the source folder {}",
                    self.data_dir.display(),
                    root.display()
                ));
            }
        }
        if self.mirror.enabled {
            let target = self
                .mirror
                .target_dir
                .as_ref()
                .ok_or("mirror.target_dir is required when the backup disk is enabled")?;
            if !super::paths::is_absolute(target) {
                return Err("mirror.target_dir must be an absolute path".into());
            }
            if self
                .mirror
                .volume_uuid
                .as_deref()
                .map_or(true, str::is_empty)
            {
                return Err(
                    "mirror.volume_uuid is required when the backup disk is enabled".into(),
                );
            }
            if target.starts_with(&self.data_dir) || self.data_dir.starts_with(target) {
                return Err("the backup folder must be outside the data directory".into());
            }
        }
        Ok(())
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
            .sources
            .claude
            .enabled
            .then(|| {
                config
                    .sources
                    .claude
                    .root
                    .clone()
                    .or_else(|| {
                        std::env::var_os("CLAUDE_CONFIG_DIR")
                            .map(PathBuf::from)
                            .filter(|p| p.is_dir())
                    })
                    .or_else(|| providers::claude::get_base_path().map(PathBuf::from))
            })
            .flatten();
        let codex = config
            .sources
            .codex
            .enabled
            .then(|| {
                config
                    .sources
                    .codex
                    .root
                    .clone()
                    .or_else(|| providers::codex::get_base_path().map(PathBuf::from))
            })
            .flatten();
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

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("config.json");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn missing_file_yields_defaults_and_partial_file_fills_in() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let defaults = KbConfig::load_from(&path).unwrap();
        assert_eq!(defaults.interval_secs, DEFAULT_INTERVAL_SECS);
        assert_eq!(KbConfig::state_of(&path), ConfigState::Missing);

        let path = write(
            dir.path(),
            r#"{"interval_secs": 60, "exclude_prefixes": ["/x"]}"#,
        );
        let loaded = KbConfig::load_from(&path).unwrap();
        assert_eq!(loaded.interval_secs, 60);
        assert_eq!(loaded.exclude_prefixes, vec!["/x".to_string()]);
        assert!(loaded.schedule_enabled && loaded.sources.claude.enabled);
    }

    #[test]
    fn v0_files_are_upgraded_in_memory_but_not_rewritten() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let text =
            r#"{"data_dir":"/data/kb","interval_secs":300,"claude_root":"/c","codex_root":"/x"}"#;
        let path = write(dir.path(), text);
        let loaded = KbConfig::load_from(&path).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.sources.claude.root, Some(PathBuf::from("/c")));
        assert_eq!(loaded.sources.codex.root, Some(PathBuf::from("/x")));
        assert!(loaded.extra.is_empty(), "old keys must not linger");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn unknown_keys_survive_a_save() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            r#"{"version":1,"data_dir":"/d","future_flag":{"a":1}}"#,
        );
        let mut loaded = KbConfig::load_from(&path).unwrap();
        loaded.interval_secs = 120;
        loaded.save_to(&path).unwrap();
        let again: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(again["future_flag"]["a"], 1);
        assert_eq!(again["interval_secs"], 120);
    }

    #[test]
    fn a_newer_file_is_refused_and_left_alone() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let text = r#"{"version":99,"data_dir":"/d"}"#;
        let path = write(dir.path(), text);
        assert!(KbConfig::load_from(&path).unwrap_err().contains("newer"));
        assert!(matches!(
            KbConfig::state_of(&path),
            ConfigState::Newer {
                file_version: 99,
                ..
            }
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn broken_or_out_of_range_files_are_errors_never_silent_defaults() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        for bad in [
            "{nope",
            "[]",
            r#"{"interval_secs": 0}"#,
            r#"{"interval_secs": 5}"#,
            r#"{"interval_secs": 999999}"#,
            r#"{"data_dir": "relative/dir"}"#,
            r#"{"exclude_prefixes": ["rel"]}"#,
            r#"{"mirror": {"enabled": true}}"#,
            r#"{"version": "one"}"#,
        ] {
            let path = write(dir.path(), bad);
            assert!(KbConfig::load_from(&path).is_err(), "{bad}");
            assert!(
                matches!(KbConfig::state_of(&path), ConfigState::Invalid { .. }),
                "{bad}"
            );
        }
    }

    #[test]
    fn data_dir_may_not_overlap_a_source_root() {
        let _home = sandbox_home();
        let mut config = KbConfig::default();
        config.sources.claude.root = Some(PathBuf::from("/src/claude"));
        config.data_dir = PathBuf::from("/src/claude/kb");
        assert!(config.validate().is_err());
        config.data_dir = PathBuf::from("/src");
        assert!(config.validate().is_err());
        config.data_dir = PathBuf::from("/elsewhere/kb");
        config.validate().unwrap();
    }

    #[test]
    fn mirror_needs_target_uuid_and_a_location_outside_the_data_dir() {
        let _home = sandbox_home();
        let mut config = KbConfig::default();
        config.data_dir = PathBuf::from("/data/kb");
        config.mirror.enabled = true;
        assert!(config.validate().is_err());
        config.mirror.target_dir = Some(PathBuf::from("/Volumes/B/AgentKB-backup"));
        assert!(config.validate().is_err(), "uuid missing");
        config.mirror.volume_uuid = Some("U-1".into());
        config.validate().unwrap();
        config.mirror.target_dir = Some(PathBuf::from("/data/kb/backup"));
        assert!(config.validate().is_err());
    }

    #[test]
    fn disabled_sources_resolve_to_none() {
        let _home = sandbox_home();
        let mut config = KbConfig::default();
        config.sources.claude.root = Some(PathBuf::from("/c"));
        config.sources.codex.root = Some(PathBuf::from("/x"));
        config.sources.codex.enabled = false;
        let roots = SourceRoots::resolve(&config);
        assert_eq!(roots.claude, Some(PathBuf::from("/c")));
        assert_eq!(roots.codex, None);
    }

    #[test]
    fn save_is_atomic_and_set_aside_keeps_the_broken_file() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.json");
        KbConfig::default().save_to(&path).unwrap();
        assert!(matches!(KbConfig::state_of(&path), ConfigState::Valid));
        std::fs::write(&path, "{broken").unwrap();
        let moved = KbConfig::set_aside(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(moved).unwrap(), "{broken");
        assert_eq!(KbConfig::state_of(&path), ConfigState::Missing);
    }

    #[test]
    fn a_disabled_backup_disk_is_saved_without_disk_details() {
        let _home = sandbox_home();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = KbConfig::load_from(&path).unwrap();
        config.mirror.enabled = false;
        config.mirror.target_dir = Some(PathBuf::from("/Volumes/Some Disk/AgentKB-backup"));
        config.mirror.volume_uuid = Some("UUID-1".into());
        config.mirror.volume_name = Some("Some Disk".into());
        config.save_to(&path).unwrap();
        let saved = KbConfig::load_from(&path).unwrap();
        assert!(!saved.mirror.enabled);
        assert_eq!(saved.mirror.target_dir, None);
        assert_eq!(saved.mirror.volume_uuid, None);
        assert_eq!(saved.mirror.volume_name, None);
    }
}
