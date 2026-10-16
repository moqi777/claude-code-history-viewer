//! Tauri commands for export and import. Thin: the work is in
//! [`super::export`] and [`super::import`], which the command line runs too.
//! Errors reach the front end as `"<code>: <message>"`.

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::Emitter;

use super::commands::PROGRESS_EVENT;
use super::config::{self, ConfigState, KbConfig};
use super::export::{self, ExportReport};
use super::import::{self, ImportReport, InspectReport, Options, Prepared};
use super::migrate_dir;
use super::progress::ProgressSink;
use super::sync::KbEnv;
use super::transfer::Cancel;

type Res<T> = Result<T, String>;

/// The cancel switch of the transfer in progress (one at a time).
static CURRENT: Mutex<Option<Cancel>> = Mutex::new(None);

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

/// Take the "one transfer at a time" slot.
fn begin() -> Res<Cancel> {
    let mut slot = CURRENT
        .lock()
        .map_err(|_| "internal: lock poisoned".to_string())?;
    if slot.is_some() {
        return Err("busy: another export or import is running".into());
    }
    let cancel = Cancel::new();
    *slot = Some(cancel.clone());
    Ok(cancel)
}

fn end() {
    if let Ok(mut slot) = CURRENT.lock() {
        *slot = None;
    }
}

/// The configuration to import against: the saved one (or the defaults when
/// there is none yet), with the data folder the caller names, if any.
fn setup(data_dir: Option<&str>) -> Res<(KbConfig, KbEnv, PathBuf, bool)> {
    let file = config::config_path()?;
    let exists = matches!(KbConfig::state()?, ConfigState::Valid);
    let mut config = KbConfig::load()?;
    if let Some(dir) = data_dir.filter(|d| !d.is_empty()) {
        config.data_dir = migrate_dir::expand(dir);
    }
    let env = KbEnv::from_config(&config);
    Ok((config, env, file, exists))
}

#[tauri::command]
pub async fn kb_export_start(
    dest: String,
    overwrite: bool,
    app: tauri::AppHandle,
) -> Res<ExportReport> {
    blocking(move || {
        let cancel = begin()?;
        let result = (|| {
            let config = KbConfig::load()?;
            let env = KbEnv::from_config(&config);
            export::export(
                &env,
                &config,
                &migrate_dir::expand(&dest),
                overwrite,
                &cancel,
                &sink(&app),
            )
            .map_err(String::from)
        })();
        end();
        result
    })
    .await
}

/// Ask the running export or import to stop. Safe to call at any time.
#[tauri::command]
pub async fn kb_transfer_cancel() -> Res<()> {
    if let Ok(slot) = CURRENT.lock() {
        if let Some(cancel) = slot.as_ref() {
            cancel.cancel();
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn kb_import_inspect(path: String, data_dir: Option<String>) -> Res<InspectReport> {
    blocking(move || {
        let (_, env, _, _) = setup(data_dir.as_deref())?;
        import::inspect(&env, &migrate_dir::expand(&path)).map_err(String::from)
    })
    .await
}

#[tauri::command]
pub async fn kb_import_prepare(
    path: String,
    adopt_settings: bool,
    data_dir: Option<String>,
    app: tauri::AppHandle,
) -> Res<Prepared> {
    blocking(move || {
        let cancel = begin()?;
        let result = (|| {
            let (config, env, _, exists) = setup(data_dir.as_deref())?;
            let opts = Options {
                adopt_settings,
                write_config: false,
                config_exists: exists,
            };
            import::prepare(
                &env,
                &config,
                &opts,
                &migrate_dir::expand(&path),
                &cancel,
                &sink(&app),
            )
            .map_err(String::from)
        })();
        end();
        result
    })
    .await
}

/// `write_config` is off in the wizard, which fills its form from the result.
#[tauri::command]
pub async fn kb_import_commit(
    path: String,
    adopt_settings: bool,
    write_config: bool,
    data_dir: Option<String>,
    app: tauri::AppHandle,
) -> Res<ImportReport> {
    blocking(move || {
        let cancel = begin()?;
        let result = (|| {
            let (config, env, file, exists) = setup(data_dir.as_deref())?;
            let opts = Options {
                adopt_settings,
                write_config,
                config_exists: exists,
            };
            import::commit(
                &env,
                &config,
                &opts,
                &file,
                &migrate_dir::expand(&path),
                &cancel,
                &sink(&app),
            )
            .map_err(String::from)
        })();
        end();
        result
    })
    .await
}

/// The person said no after the preview: remove the unpacked copy.
#[tauri::command]
pub async fn kb_import_discard(path: String, data_dir: Option<String>) -> Res<()> {
    blocking(move || {
        let (_, env, _, _) = setup(data_dir.as_deref())?;
        import::discard(&env, &migrate_dir::expand(&path)).map_err(String::from)
    })
    .await
}
