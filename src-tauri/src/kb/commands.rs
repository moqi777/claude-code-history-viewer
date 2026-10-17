//! Tauri commands behind the knowledge-base page, settings and wizard. They
//! call the same code the headless commands do, so a click and a launchd run
//! behave identically; the front end never reads `config.json` itself.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::Emitter;

use super::backup::{self, BackupSummary};
use super::config::{self, ConfigState, KbConfig, SourceRoots};
use super::migrate_dir::{self, MigrateCheck, MigrationReport};
use super::paths::DataPaths;
use super::probe::{self, DataDirReport, MirrorReport, ScheduleEnvReport, SourceProbe};
use super::progress::ProgressSink;
use super::schedule::{self, ScheduleStatus};
use super::status::{self, Facets, KbSession, KbStatus, SessionFilter};
use super::sync::{self, KbEnv, Summary};
use super::volume::{self, MacQuery, Presence, VolumeInfo};
use super::{db, AGENTKB_VERSION};
use super::{search, skill};

pub const PROGRESS_EVENT: &str = "kb-progress";
pub const BACKUP_DONE_EVENT: &str = "kb-backup-done";
/// The viewer version this fork is built on (kept equal to upstream's `package.json`).
pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

type Res<T> = Result<T, String>;

fn env() -> Res<KbEnv> {
    Ok(KbEnv::from_config(&KbConfig::load()?))
}

fn sink(app: &tauri::AppHandle) -> ProgressSink {
    let app = app.clone();
    ProgressSink::new(move |p| {
        let _ = app.emit(PROGRESS_EVENT, p);
    })
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Res<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Task join error: {e}"))?
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct KbOverview {
    #[serde(flatten)]
    pub status: KbStatus,
    pub schedule: ScheduleStatus,
    pub interval_secs: u64,
    pub schedule_enabled: bool,
    pub exe_stability: schedule::ExeStability,
    pub agentkb_version: &'static str,
    pub base_version: &'static str,
    pub mirror: MirrorSettings,
}

#[derive(Debug, Serialize)]
pub struct MirrorSettings {
    pub enabled: bool,
    pub target_dir: Option<String>,
    pub volume_name: Option<String>,
}

#[tauri::command]
pub async fn kb_get_status() -> Res<KbOverview> {
    blocking(|| {
        let config = KbConfig::load()?;
        let env = KbEnv::from_config(&config);
        Ok(KbOverview {
            status: status::collect(&env)?,
            schedule: schedule::status_now()?,
            interval_secs: config.interval_secs,
            schedule_enabled: config.schedule_enabled,
            exe_stability: schedule::classify_exe(&std::env::current_exe().unwrap_or_default()),
            agentkb_version: AGENTKB_VERSION,
            base_version: BASE_VERSION,
            mirror: MirrorSettings {
                enabled: config.mirror.enabled,
                target_dir: config.mirror.target_dir.map(|p| p.display().to_string()),
                volume_name: config.mirror.volume_name,
            },
        })
    })
    .await
}

/// Whether the configured backup disk is plugged in right now (asks the OS, so
/// it is separate from the cheap status poll).
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BackupPresence {
    Disabled,
    Connected { volume: VolumeInfo },
    NotConnected { reason: String },
    Unsupported { reason: volume::Unsupported },
}

#[tauri::command]
pub async fn kb_get_backup_presence() -> Res<BackupPresence> {
    blocking(|| {
        let config = KbConfig::load()?;
        let (true, Some(target), Some(uuid)) = (
            config.mirror.enabled,
            config.mirror.target_dir.as_deref(),
            config.mirror.volume_uuid.as_deref(),
        ) else {
            return Ok(BackupPresence::Disabled);
        };
        Ok(match volume::presence(target, uuid, &MacQuery) {
            Presence::Connected { volume } => BackupPresence::Connected { volume },
            Presence::NotConnected { reason } => BackupPresence::NotConnected { reason },
            Presence::Unsupported { reason } => BackupPresence::Unsupported { reason },
        })
    })
    .await
}

#[tauri::command]
pub async fn kb_list_sessions(filter: Option<SessionFilter>) -> Res<Vec<KbSession>> {
    blocking(move || status::list_sessions(&env()?, &filter.unwrap_or_default())).await
}

#[tauri::command]
pub async fn kb_session_facets() -> Res<Facets> {
    blocking(|| status::facets(&env()?)).await
}

#[tauri::command]
pub async fn kb_dismiss_notice(key: String) -> Res<()> {
    blocking(move || {
        let env = env()?;
        if key == "duplicates_cleaned" {
            return super::dedupe::dismiss(&db::open(&env.data.db())?);
        }
        let keys: &[&str] = match key.as_str() {
            "schedule_repaired" => &[
                schedule::META_REPAIRED_AT,
                schedule::META_REPAIRED_FROM,
                schedule::META_REPAIRED_TO,
                schedule::META_REPAIRED_REASON,
            ],
            other => return Err(format!("unknown notice {other}")),
        };
        let conn = db::open(&env.data.db())?;
        for k in keys {
            db::delete_meta(&conn, k)?;
        }
        Ok(())
    })
    .await
}

// ---------------------------------------------------------------------------
// Collect and back up
// ---------------------------------------------------------------------------

/// "Collect now". A sync that is already running yields `status: "skipped"`
/// (`reason: "locked"`) after the standard retry window.
#[tauri::command]
pub async fn kb_sync_now(app: tauri::AppHandle) -> Res<Summary> {
    blocking(move || sync::run_sync(&env()?.with_progress(sink(&app)), "gui")).await
}

#[tauri::command]
pub async fn kb_backup_now(app: tauri::AppHandle) -> Res<BackupSummary> {
    blocking(move || {
        let env = env()?.with_progress(sink(&app));
        backup::run_standalone(&env, &MacQuery)
    })
    .await
}

// ---------------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn kb_schedule_install() -> Res<ScheduleStatus> {
    blocking(|| schedule::install_now(&KbConfig::load()?)).await
}

#[tauri::command]
pub async fn kb_schedule_uninstall() -> Res<ScheduleStatus> {
    blocking(schedule::uninstall_now).await
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ConfigStateInfo {
    #[serde(flatten)]
    pub state: ConfigState,
    /// The saved configuration when valid, otherwise the defaults (a starting
    /// point for the wizard).
    pub config: KbConfig,
    pub config_path: String,
}

#[tauri::command]
pub async fn kb_get_config_state() -> Res<ConfigStateInfo> {
    blocking(|| {
        let path = config::config_path()?;
        let state = KbConfig::state_of(&path);
        let config = match state {
            ConfigState::Valid => KbConfig::load_from(&path)?,
            _ => KbConfig::default(),
        };
        Ok(ConfigStateInfo {
            state,
            config,
            config_path: path.display().to_string(),
        })
    })
    .await
}

#[tauri::command]
pub async fn kb_get_config() -> Res<KbConfig> {
    blocking(KbConfig::load).await
}

#[derive(Debug, Serialize)]
pub struct SaveOutcome {
    pub config: KbConfig,
    pub schedule: Option<ScheduleStatus>,
    /// The settings were saved but the scheduled task could not be updated.
    pub schedule_error: Option<String>,
}

/// Save from the settings page. The data directory is not changed here (that
/// needs a migration), and a backup disk must pass its test before it is enabled.
#[tauri::command]
pub async fn kb_save_config(config: KbConfig) -> Res<SaveOutcome> {
    blocking(move || save_config(&config_file()?, config, &MacQuery, &SystemSchedule)).await
}

fn config_file() -> Res<PathBuf> {
    config::config_path()
}

/// Seam for tests: what happens to the launchd task after a save.
pub trait ScheduleApply {
    fn install(&self, config: &KbConfig) -> Res<ScheduleStatus>;
    fn uninstall(&self) -> Res<ScheduleStatus>;
}

pub struct SystemSchedule;

impl ScheduleApply for SystemSchedule {
    fn install(&self, config: &KbConfig) -> Res<ScheduleStatus> {
        schedule::install_now(config)
    }
    fn uninstall(&self) -> Res<ScheduleStatus> {
        schedule::uninstall_now()
    }
}

pub fn save_config(
    file: &Path,
    mut next: KbConfig,
    query: &dyn volume::SystemQuery,
    scheduler: &dyn ScheduleApply,
) -> Res<SaveOutcome> {
    let previous = match KbConfig::state_of(file) {
        ConfigState::Valid => Some(KbConfig::load_from(file)?),
        _ => None,
    };
    if let Some(old) = &previous {
        if next.data_dir != old.data_dir {
            return Err(
                "the data directory is changed with 'Move data directory', not by saving".into(),
            );
        }
    }
    // Semantic search has commands of its own (they download and test first);
    // a settings page opened before one of them ran must not undo it.
    if let Some(old) = &previous {
        next.semantic = old.semantic.clone();
    }
    next.validate()?;
    // Enabling (or re-pointing) the backup disk requires the test to pass.
    let mirror_changed = previous.as_ref().map_or(true, |old| {
        !old.mirror.enabled
            || old.mirror.target_dir != next.mirror.target_dir
            || old.mirror.volume_uuid != next.mirror.volume_uuid
    });
    if next.mirror.enabled && mirror_changed {
        let target = next
            .mirror
            .target_dir
            .clone()
            .ok_or("backup folder missing")?;
        match probe::test_mirror(&target, next.mirror.volume_uuid.as_deref(), query) {
            MirrorReport::Connected { .. } => {}
            other => return Err(format!("the backup disk test did not pass: {other:?}")),
        }
    }
    next.save_to(file)?;

    let schedule_changed = previous.as_ref().map_or(true, |old| {
        old.interval_secs != next.interval_secs || old.schedule_enabled != next.schedule_enabled
    });
    let mut outcome = SaveOutcome {
        config: next.clone(),
        schedule: None,
        schedule_error: None,
    };
    if schedule_changed {
        let result = if next.schedule_enabled {
            scheduler.install(&next)
        } else {
            scheduler.uninstall()
        };
        match result {
            Ok(status) => outcome.schedule = Some(status),
            Err(e) => outcome.schedule_error = Some(e),
        }
    }
    Ok(outcome)
}

/// A broken or too-new `config.json` is set aside (never deleted); the app then
/// starts over from the wizard.
#[tauri::command]
pub async fn kb_restore_default_config() -> Res<String> {
    blocking(|| {
        let path = config::config_path()?;
        if matches!(KbConfig::state_of(&path), ConfigState::Missing) {
            return Ok(String::new());
        }
        Ok(KbConfig::set_aside(&path)?.display().to_string())
    })
    .await
}

// ---------------------------------------------------------------------------
// Test buttons
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn kb_probe_sources(config: Option<KbConfig>) -> Res<Vec<SourceProbe>> {
    blocking(move || {
        let config = match config {
            Some(c) => c,
            None => KbConfig::load()?,
        };
        Ok(probe::probe_sources(&config))
    })
    .await
}

#[tauri::command]
pub async fn kb_test_data_dir(path: String) -> Res<DataDirReport> {
    blocking(move || {
        let roots = SourceRoots::resolve(&KbConfig::default());
        Ok(probe::test_data_dir(&migrate_dir::expand(&path), &roots))
    })
    .await
}

#[tauri::command]
pub async fn kb_test_schedule_env() -> Res<ScheduleEnvReport> {
    blocking(|| Ok(probe::test_schedule_env())).await
}

#[tauri::command]
pub async fn kb_list_external_volumes() -> Res<Vec<VolumeInfo>> {
    blocking(|| Ok(volume::list_external(&MacQuery))).await
}

#[tauri::command]
pub async fn kb_test_mirror(
    target_dir: String,
    expected_uuid: Option<String>,
) -> Res<MirrorReport> {
    blocking(move || {
        Ok(probe::test_mirror(
            &PathBuf::from(target_dir),
            expected_uuid.as_deref(),
            &MacQuery,
        ))
    })
    .await
}

// ---------------------------------------------------------------------------
// Wizard
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct WizardResult {
    pub sync: Summary,
    pub schedule: Option<ScheduleStatus>,
    pub schedule_error: Option<String>,
    /// The first backup to the external disk runs on in the background.
    pub backup_started: bool,
}

/// Finish the wizard: first collection → write `config.json` → install the
/// task. Nothing is written to disk until the collection has succeeded, so
/// quitting at any earlier point leaves no half configuration behind.
#[tauri::command]
pub async fn kb_wizard_commit(draft: KbConfig, app: tauri::AppHandle) -> Res<WizardResult> {
    blocking(move || {
        let file = config_file()?;
        wizard_commit(
            &file,
            draft,
            &sink(&app),
            &MacQuery,
            &SystemSchedule,
            &|| {
                let app = app.clone();
                std::thread::spawn(move || {
                    let outcome = env()
                        .map(|e| e.with_progress(sink(&app)))
                        .and_then(|e| backup::run_standalone(&e, &MacQuery));
                    let _ = app.emit(BACKUP_DONE_EVENT, outcome);
                });
            },
        )
    })
    .await
}

pub fn wizard_commit(
    file: &Path,
    draft: KbConfig,
    progress: &ProgressSink,
    query: &dyn volume::SystemQuery,
    scheduler: &dyn ScheduleApply,
    start_backup: &dyn Fn(),
) -> Res<WizardResult> {
    draft.validate()?;
    if let ConfigState::Valid = KbConfig::state_of(file) {
        let existing = KbConfig::load_from(file)?;
        if existing.data_dir != draft.data_dir {
            return Err(
                "the data directory of a configured app is changed with 'Move data directory'"
                    .into(),
            );
        }
    }
    if draft.mirror.enabled {
        let target = draft
            .mirror
            .target_dir
            .clone()
            .ok_or("backup folder missing")?;
        match probe::test_mirror(&target, draft.mirror.volume_uuid.as_deref(), query) {
            MirrorReport::Connected { .. } => {}
            other => return Err(format!("the backup disk test did not pass: {other:?}")),
        }
    }

    // 1. First collection, from the draft. The backup follows asynchronously.
    let mut env = KbEnv::from_config(&draft).with_progress(progress.clone());
    env.mirror = None;
    let summary = sync::run_sync(&env, "gui")?;
    if summary.status == "error" {
        let first = summary
            .errors
            .first()
            .map_or("unknown error", |e| e.message.as_str());
        return Err(format!("the first collection failed: {first}"));
    }
    if summary.status == "skipped" {
        return Err("another collection is running; try again in a moment".into());
    }

    // 2. Only now does the configuration exist.
    draft.save_to(file)?;

    // 3. The scheduled task. The configuration is already valid, so a failure
    //    here is reported but not rolled back.
    let (schedule, schedule_error) = if draft.schedule_enabled {
        match scheduler.install(&draft) {
            Ok(status) => (Some(status), None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, None)
    };

    // 4. First backup, in the background.
    let backup_started = draft.mirror.enabled;
    if backup_started {
        start_backup();
    }
    Ok(WizardResult {
        sync: summary,
        schedule,
        schedule_error,
        backup_started,
    })
}

// ---------------------------------------------------------------------------
// Moving the data directory
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn kb_change_data_dir_check(new_dir: String) -> Res<MigrateCheck> {
    blocking(move || {
        let config = KbConfig::load()?;
        let roots = SourceRoots::resolve(&config);
        Ok(migrate_dir::check(
            &config.data_dir,
            &migrate_dir::expand(&new_dir),
            &roots,
        ))
    })
    .await
}

#[derive(Debug, Serialize)]
pub struct MoveOutcome {
    pub report: Option<MigrationReport>,
    pub schedule_error: Option<String>,
}

#[tauri::command]
pub async fn kb_change_data_dir_run(
    new_dir: String,
    use_existing: bool,
    app: tauri::AppHandle,
) -> Res<MoveOutcome> {
    blocking(move || {
        let file = config_file()?;
        let config = KbConfig::load()?;
        let target = migrate_dir::expand(&new_dir);
        let report = if use_existing {
            migrate_dir::adopt_existing(&file, &config, &target)?;
            None
        } else {
            Some(migrate_dir::apply(&file, &config, &target, &sink(&app))?)
        };
        // The task's log paths live in the data directory: reinstall it.
        let schedule_error = if config.schedule_enabled {
            schedule::install_now(&KbConfig::load()?).err()
        } else {
            None
        };
        Ok(MoveOutcome {
            report,
            schedule_error,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::volume::fake::{volume as fake_volume, FakeQuery};
    use std::cell::{Cell, RefCell};

    struct FakeSchedule {
        installs: Cell<u32>,
        uninstalls: Cell<u32>,
        fail: bool,
        seen: RefCell<Vec<u64>>,
    }

    impl FakeSchedule {
        fn new(fail: bool) -> Self {
            Self {
                installs: Cell::new(0),
                uninstalls: Cell::new(0),
                fail,
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl ScheduleApply for FakeSchedule {
        fn install(&self, config: &KbConfig) -> Res<ScheduleStatus> {
            self.installs.set(self.installs.get() + 1);
            self.seen.borrow_mut().push(config.interval_secs);
            if self.fail {
                Err("launchctl exploded".into())
            } else {
                Ok(ScheduleStatus::default())
            }
        }
        fn uninstall(&self) -> Res<ScheduleStatus> {
            self.uninstalls.set(self.uninstalls.get() + 1);
            Ok(ScheduleStatus::default())
        }
    }

    struct Rig {
        tmp: tempfile::TempDir,
        file: PathBuf,
        data: PathBuf,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CCHV_TEST_HOME", tmp.path());
        // Empty source folders so collection has something valid to scan.
        std::fs::create_dir_all(tmp.path().join("claude/projects")).unwrap();
        let data = tmp.path().join("kb-data");
        Rig {
            file: tmp.path().join("cfg/config.json"),
            data,
            tmp,
        }
    }

    fn draft(rig: &Rig) -> KbConfig {
        let mut config = KbConfig {
            data_dir: rig.data.clone(),
            ..KbConfig::default()
        };
        config.sources.claude.root = Some(rig.tmp.path().join("claude"));
        config.sources.codex.enabled = false;
        config
    }

    #[test]
    fn wizard_collects_first_then_writes_the_config_then_installs_the_task() {
        let rig = rig();
        let sched = FakeSchedule::new(false);
        let started = Cell::new(false);
        let result = wizard_commit(
            &rig.file,
            draft(&rig),
            &ProgressSink::default(),
            &FakeQuery::default(),
            &sched,
            &|| started.set(true),
        )
        .unwrap();
        assert_eq!(result.sync.status, "ok");
        assert!(rig.data.join("kb.db").is_file(), "first collection ran");
        assert!(matches!(KbConfig::state_of(&rig.file), ConfigState::Valid));
        assert_eq!(sched.installs.get(), 1);
        assert!(!started.get(), "no disk configured, no backup");
    }

    #[test]
    fn a_failed_first_collection_leaves_no_config_behind() {
        let rig = rig();
        // Make the data directory impossible: a file where the folder should be.
        std::fs::write(&rig.data, b"a file, not a folder").unwrap();
        let sched = FakeSchedule::new(false);
        let err = wizard_commit(
            &rig.file,
            draft(&rig),
            &ProgressSink::default(),
            &FakeQuery::default(),
            &sched,
            &|| {},
        );
        assert!(err.is_err());
        assert!(!rig.file.exists(), "nothing half-written");
        assert_eq!(sched.installs.get(), 0);
    }

    #[test]
    fn a_failing_task_install_keeps_the_valid_config_and_reports_the_problem() {
        let rig = rig();
        let sched = FakeSchedule::new(true);
        let result = wizard_commit(
            &rig.file,
            draft(&rig),
            &ProgressSink::default(),
            &FakeQuery::default(),
            &sched,
            &|| {},
        )
        .unwrap();
        assert_eq!(result.schedule_error.as_deref(), Some("launchctl exploded"));
        assert!(matches!(KbConfig::state_of(&rig.file), ConfigState::Valid));
    }

    #[test]
    fn the_wizard_refuses_an_unplugged_backup_disk_and_writes_nothing() {
        let rig = rig();
        let mut config = draft(&rig);
        config.mirror.enabled = true;
        config.mirror.target_dir = Some(rig.tmp.path().join("Volumes/Gone/AgentKB-backup"));
        config.mirror.volume_uuid = Some("GONE".into());
        let err = wizard_commit(
            &rig.file,
            config,
            &ProgressSink::default(),
            &FakeQuery::default(),
            &FakeSchedule::new(false),
            &|| {},
        );
        assert!(err.unwrap_err().contains("backup disk test"));
        assert!(!rig.file.exists());
        assert!(!rig.data.exists(), "not even a collection happened");
    }

    #[test]
    fn the_wizard_starts_the_first_backup_after_everything_else_when_a_disk_is_set() {
        let rig = rig();
        let disk = rig.tmp.path().join("disk");
        std::fs::create_dir_all(&disk).unwrap();
        let query = FakeQuery::with([VolumeInfo {
            mount_point: disk.clone(),
            ..fake_volume("DISK", "/x", false, "exfat")
        }]);
        let mut config = draft(&rig);
        config.mirror.enabled = true;
        config.mirror.target_dir = Some(disk.join("AgentKB-backup"));
        config.mirror.volume_uuid = Some("DISK".into());
        let started = Cell::new(false);
        let result = wizard_commit(
            &rig.file,
            config,
            &ProgressSink::default(),
            &query,
            &FakeSchedule::new(false),
            &|| started.set(true),
        )
        .unwrap();
        assert!(result.backup_started && started.get());
    }

    #[test]
    fn saving_a_new_interval_reinstalls_the_task_and_other_changes_do_not() {
        let rig = rig();
        let mut config = draft(&rig);
        config.save_to(&rig.file).unwrap();
        let sched = FakeSchedule::new(false);

        config.interval_secs = 300;
        let out = save_config(&rig.file, config.clone(), &FakeQuery::default(), &sched).unwrap();
        assert_eq!(sched.installs.get(), 1);
        assert_eq!(*sched.seen.borrow(), vec![300]);
        assert!(out.schedule.is_some());

        config.exclude_prefixes = vec!["/tmp/x".into()];
        save_config(&rig.file, config.clone(), &FakeQuery::default(), &sched).unwrap();
        assert_eq!(sched.installs.get(), 1, "unrelated change: no reinstall");

        config.schedule_enabled = false;
        save_config(&rig.file, config, &FakeQuery::default(), &sched).unwrap();
        assert_eq!(sched.uninstalls.get(), 1);
    }

    #[test]
    fn a_settings_page_opened_earlier_cannot_undo_semantic_search() {
        let rig = rig();
        let mut config = draft(&rig);
        config.semantic.enabled = true;
        config.semantic.source = "huggingface".into();
        config.save_to(&rig.file).unwrap();
        // the page was opened before it was switched on: its copy says off
        let mut stale = config.clone();
        stale.semantic.enabled = false;
        stale.semantic.source = "modelscope".into();
        stale.interval_secs = 900;
        save_config(
            &rig.file,
            stale,
            &FakeQuery::default(),
            &FakeSchedule::new(false),
        )
        .unwrap();
        let now = KbConfig::load_from(&rig.file).unwrap();
        assert_eq!(now.interval_secs, 900, "the page's own settings are saved");
        assert!(now.semantic.enabled);
        assert_eq!(now.semantic.source, "huggingface");
    }

    #[test]
    fn saving_cannot_move_the_data_directory_or_enable_an_untested_disk() {
        let rig = rig();
        let mut config = draft(&rig);
        config.save_to(&rig.file).unwrap();
        let sched = FakeSchedule::new(false);

        let mut moved = config.clone();
        moved.data_dir = rig.tmp.path().join("elsewhere");
        assert!(save_config(&rig.file, moved, &FakeQuery::default(), &sched).is_err());

        config.mirror.enabled = true;
        config.mirror.target_dir = Some(rig.tmp.path().join("nowhere/backup"));
        config.mirror.volume_uuid = Some("U".into());
        assert!(save_config(&rig.file, config, &FakeQuery::default(), &sched).is_err());
        // The saved file is untouched by the rejected attempts.
        assert!(!KbConfig::load_from(&rig.file).unwrap().mirror.enabled);
    }

    #[test]
    fn a_schedule_failure_after_a_save_is_reported_not_fatal() {
        let rig = rig();
        let mut config = draft(&rig);
        config.save_to(&rig.file).unwrap();
        config.interval_secs = 120;
        let out = save_config(
            &rig.file,
            config,
            &FakeQuery::default(),
            &FakeSchedule::new(true),
        )
        .unwrap();
        assert_eq!(out.schedule_error.as_deref(), Some("launchctl exploded"));
        assert_eq!(KbConfig::load_from(&rig.file).unwrap().interval_secs, 120);
    }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SearchOutcome {
    Ok {
        response: Box<search::SearchResponse>,
    },
    /// The index is still being built.
    NotReady { message: String },
    /// No knowledge base yet.
    NoData { message: String },
}

#[tauri::command]
pub async fn kb_search(params: search::SearchParams) -> Res<SearchOutcome> {
    blocking(move || {
        let data = DataPaths::new(KbConfig::load()?.data_dir);
        Ok(match search::search_in(&data, &params) {
            Ok(response) => SearchOutcome::Ok {
                response: Box::new(response),
            },
            Err(search::SearchError::NotReady(message)) => SearchOutcome::NotReady { message },
            Err(search::SearchError::NoData(message)) => SearchOutcome::NoData { message },
            Err(search::SearchError::Other(message)) => return Err(message),
        })
    })
    .await
}

// ---------------------------------------------------------------------------
// Agent skill
// ---------------------------------------------------------------------------

fn skill_roots() -> Res<SourceRoots> {
    Ok(skill::roots_for(&KbConfig::load()?))
}

fn skill_root(roots: &SourceRoots, target: skill::Target) -> Option<PathBuf> {
    match target {
        skill::Target::Claude => roots.claude.clone(),
        skill::Target::Codex => roots.codex.clone(),
    }
}

fn skill_statuses() -> Res<Vec<skill::SkillStatus>> {
    let roots = skill_roots()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(skill::Target::ALL
        .iter()
        .map(|t| skill::status(*t, skill_root(&roots, *t).as_deref(), &exe))
        .collect())
}

#[tauri::command]
pub async fn kb_skill_status() -> Res<Vec<skill::SkillStatus>> {
    blocking(skill_statuses).await
}

fn parse_target(target: &str) -> Res<skill::Target> {
    skill::Target::parse(target).ok_or_else(|| format!("unknown tool {target:?}"))
}

#[tauri::command]
pub async fn kb_skill_install(target: String) -> Res<Vec<skill::SkillStatus>> {
    blocking(move || {
        let target = parse_target(&target)?;
        let roots = skill_roots()?;
        let root = skill_root(&roots, target).ok_or("that tool's folder is not configured")?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        skill::install(target, &root, &exe)?;
        skill_statuses()
    })
    .await
}

#[tauri::command]
pub async fn kb_skill_uninstall(target: String) -> Res<Vec<skill::SkillStatus>> {
    blocking(move || {
        let target = parse_target(&target)?;
        let roots = skill_roots()?;
        let root = skill_root(&roots, target).ok_or("that tool's folder is not configured")?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        skill::uninstall(target, &root, &exe)?;
        skill_statuses()
    })
    .await
}

/// Runs a real `--kb-search` with the program the installed skill points at
/// (the running program when nothing is installed).
#[tauri::command]
pub async fn kb_skill_test() -> Res<skill::SkillTest> {
    blocking(|| {
        let config = KbConfig::load()?;
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        let statuses = skill_statuses()?;
        let exe = statuses
            .iter()
            .find_map(|s| s.installed_exe.clone())
            .map_or(current, PathBuf::from);
        // Any word that is in the index: one from the newest session's title.
        let data = DataPaths::new(config.data_dir);
        let word = db::open_readonly(&data.db())?
            .and_then(|conn| {
                conn.query_row(
                    "SELECT title FROM sessions WHERE title IS NOT NULL \
                     ORDER BY last_activity_at DESC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
            .and_then(|title| {
                super::index::tokenize::tokens(&title)
                    .into_iter()
                    .find(|t| t.chars().count() >= 2)
            })
            .unwrap_or_else(|| "agentkb".to_string());
        Ok(skill::test(&exe, &word))
    })
    .await
}
