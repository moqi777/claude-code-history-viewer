//! Tauri commands for the "knowledge base" panel. They call the same code the
//! headless commands do, so a click and a launchd run behave identically.

use serde::Serialize;

use super::config::KbConfig;
use super::schedule::{self, ScheduleStatus};
use super::status::{self, KbSession, KbStatus, SessionFilter};
use super::sync::{self, KbEnv, Summary};

fn env() -> Result<KbEnv, String> {
    Ok(KbEnv::from_config(&KbConfig::load()?))
}

#[derive(Debug, Serialize)]
pub struct KbOverview {
    #[serde(flatten)]
    pub status: KbStatus,
    pub schedule: ScheduleStatus,
    pub interval_secs: u64,
}

#[tauri::command]
pub async fn kb_get_status() -> Result<KbOverview, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let config = KbConfig::load()?;
        let env = KbEnv::from_config(&config);
        Ok(KbOverview {
            status: status::collect(&env)?,
            schedule: schedule::status_now()?,
            interval_secs: config.interval_secs,
        })
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// "Collect now". A sync that is already running yields `status: "skipped"`
/// (`reason: "locked"`) after the standard retry window; the panel shows that
/// as "collecting".
#[tauri::command]
pub async fn kb_sync_now() -> Result<Summary, String> {
    tauri::async_runtime::spawn_blocking(|| sync::run_sync(&env()?, "gui"))
        .await
        .map_err(|e| format!("Task join error: {e}"))?
}

#[tauri::command]
pub async fn kb_list_sessions(filter: Option<SessionFilter>) -> Result<Vec<KbSession>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        status::list_sessions(&env()?, &filter.unwrap_or_default())
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

#[tauri::command]
pub async fn kb_schedule_install() -> Result<ScheduleStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let config = KbConfig::load()?;
        schedule::install_now(&config)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

#[tauri::command]
pub async fn kb_schedule_uninstall() -> Result<ScheduleStatus, String> {
    tauri::async_runtime::spawn_blocking(schedule::uninstall_now)
        .await
        .map_err(|e| format!("Task join error: {e}"))?
}
