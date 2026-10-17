//! Tauri commands of the settings page's "semantic search" group. Thin: the
//! work is in `model`, `build` and `store`, which the command line uses too.
//! Heavy things (building, testing the model) run in a child process of this
//! program so the window never holds the 2 GB the model needs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::Emitter;

use super::model::{self, ModelStatus, Source};
use super::state::{self, EmbedState};
use super::{build, store};
use crate::kb::config::KbConfig;
use crate::kb::paths::DataPaths;

pub const DOWNLOAD_EVENT: &str = "kb-semantic-download";
type Res<T> = Result<T, String>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Res<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Task join error: {e}"))?
}

fn data_paths() -> Res<DataPaths> {
    Ok(DataPaths::new(KbConfig::load()?.data_dir))
}

static DOWNLOAD: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

#[derive(Debug, Serialize)]
pub struct SourceInfo {
    pub id: &'static str,
    /// Host shown to the user.
    pub host: &'static str,
}

#[derive(Debug, Serialize)]
pub struct SemanticStatus {
    pub enabled: bool,
    pub source: String,
    pub sources: Vec<SourceInfo>,
    pub model_name: &'static str,
    pub model_bytes: u64,
    pub model: ModelStatus,
    pub downloading: bool,
    pub index: IndexStatus,
    pub vectors_bytes: u64,
    pub models_dir: String,
}

#[derive(Debug, Serialize)]
pub struct IndexStatus {
    #[serde(flatten)]
    pub state: EmbedState,
    pub indexed: usize,
    pub building: bool,
}

fn dir_size(path: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len())
        .sum()
}

fn status_of(config: &KbConfig) -> SemanticStatus {
    let data = DataPaths::new(config.data_dir.clone());
    let mut st = state::read(&data);
    let building = build::is_running(&data);
    if !building && st.status == "running" {
        // the builder died without saying so
        st.status = "idle".into();
    }
    let indexed = store::Store::open_ro(&store::path(&data), model::model_sha())
        .ok()
        .flatten()
        .map_or(0, |s| s.count());
    SemanticStatus {
        enabled: config.semantic.enabled,
        source: config.semantic.source.clone(),
        sources: vec![
            SourceInfo {
                id: "modelscope",
                host: "modelscope.cn",
            },
            SourceInfo {
                id: "huggingface",
                host: "huggingface.co",
            },
        ],
        model_name: "bge-m3 (int8)",
        model_bytes: model::SPEC.files.iter().map(|f| f.size).sum(),
        model: model::check(&data, &model::SPEC),
        downloading: DOWNLOAD.lock().is_ok_and(|s| s.is_some()),
        index: IndexStatus {
            state: st,
            indexed,
            building,
        },
        vectors_bytes: dir_size(&store::dir(&data)),
        models_dir: model::dir(&data).display().to_string(),
    }
}

#[tauri::command]
pub async fn kb_semantic_status() -> Res<SemanticStatus> {
    blocking(|| Ok(status_of(&KbConfig::load()?))).await
}

/// Switch it on or off, or pick the download source. Turning it on needs a model
/// that passes `kb_semantic_test` first; the page enforces that order, and
/// here a missing model is refused too.
#[tauri::command]
pub async fn kb_semantic_set(enabled: Option<bool>, source: Option<String>) -> Res<SemanticStatus> {
    blocking(move || {
        let mut config = KbConfig::load()?;
        if let Some(s) = source {
            if Source::parse(&s).is_none() {
                return Err("unknown download source".into());
            }
            config.semantic.source = s;
        }
        if let Some(on) = enabled {
            if on {
                let data = DataPaths::new(config.data_dir.clone());
                if model::check(&data, &model::SPEC) != ModelStatus::Ready {
                    return Err("the model is not ready: download it first".into());
                }
            }
            config.semantic.enabled = on;
        }
        config.save()?;
        let status = status_of(&config);
        if status.enabled {
            // start filling the index without waiting for the next collection
            let _ = start_builder(&DataPaths::new(config.data_dir), false);
        }
        Ok(status)
    })
    .await
}

/// Download the model (from the saved source) and tell the page how it goes
/// through `kb-semantic-download` (progress) and the returned result.
#[tauri::command]
pub async fn kb_semantic_download(app: tauri::AppHandle) -> Res<()> {
    blocking(move || {
        let config = KbConfig::load()?;
        let source = Source::parse(&config.semantic.source).ok_or("unknown download source")?;
        let data = DataPaths::new(config.data_dir);
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut slot = DOWNLOAD.lock().map_err(|_| "internal: lock poisoned")?;
            if slot.is_some() {
                return Err("a download is already running".into());
            }
            *slot = Some(Arc::clone(&cancel));
        }
        let result = model::download(&data, &model::SPEC, source, &cancel, &mut |p| {
            let _ = app.emit(DOWNLOAD_EVENT, p);
        });
        if let Ok(mut slot) = DOWNLOAD.lock() {
            *slot = None;
        }
        result
    })
    .await
}

#[tauri::command]
pub async fn kb_semantic_download_cancel() -> Res<()> {
    if let Ok(slot) = DOWNLOAD.lock() {
        if let Some(c) = slot.as_ref() {
            c.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn kb_semantic_import_folder(path: String) -> Res<SemanticStatus> {
    blocking(move || {
        let config = KbConfig::load()?;
        let data = DataPaths::new(config.data_dir.clone());
        model::import_folder(&data, &model::SPEC, &crate::kb::migrate_dir::expand(&path))?;
        Ok(status_of(&config))
    })
    .await
}

#[tauri::command]
pub async fn kb_semantic_delete_model() -> Res<SemanticStatus> {
    blocking(|| {
        let mut config = KbConfig::load()?;
        let data = DataPaths::new(config.data_dir.clone());
        if build::is_running(&data) {
            return Err("the index is being built right now: stop it first".into());
        }
        model::delete(&data)?;
        if config.semantic.enabled {
            config.semantic.enabled = false;
            config.save()?;
        }
        Ok(status_of(&config))
    })
    .await
}

/// The "test" button: a child process loads the model, embeds three sentences
/// and reports (so a crash or 2 GB never touches the window).
#[tauri::command]
pub async fn kb_semantic_test() -> Res<build::TestReport> {
    blocking(|| {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut command = std::process::Command::new(exe);
        crate::kb::platform::no_window(&mut command);
        let out = command
            .arg("--kb-embed")
            .arg("--test")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("cannot start the test: {e}"))?;
        serde_json::from_slice(&out.stdout).map_err(|_| {
            format!(
                "the test gave no answer: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    })
    .await
}

fn start_builder(data: &DataPaths, everything: bool) -> Res<()> {
    if build::is_running(data) {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    build::spawn_background(
        &exe,
        data,
        &["--kb-embed", if everything { "--all" } else { "--auto" }],
    )
}

/// "Build a batch now" (`everything = false`) or "build everything now".
#[tauri::command]
pub async fn kb_semantic_build(everything: bool) -> Res<()> {
    blocking(move || {
        let config = KbConfig::load()?;
        let data = DataPaths::new(config.data_dir);
        if model::check(&data, &model::SPEC) != ModelStatus::Ready {
            return Err("the model is not ready".into());
        }
        start_builder(&data, everything)
    })
    .await
}

#[tauri::command]
pub async fn kb_semantic_build_cancel() -> Res<bool> {
    blocking(|| Ok(build::request_stop(&data_paths()?))).await
}

/// Throw the vectors away and compute them again.
#[tauri::command]
pub async fn kb_semantic_rebuild() -> Res<()> {
    blocking(|| {
        let data = data_paths()?;
        if build::is_running(&data) {
            return Err("the index is being built right now: stop it first".into());
        }
        let file = store::path(&data);
        for suffix in ["", "-wal", "-shm"] {
            let p = std::path::PathBuf::from(format!("{}{suffix}", file.display()));
            if p.exists() {
                std::fs::remove_file(&p)
                    .map_err(|e| format!("cannot delete {}: {e}", p.display()))?;
            }
        }
        let _ = std::fs::remove_file(state::path(&data));
        start_builder(&data, false)
    })
    .await
}
