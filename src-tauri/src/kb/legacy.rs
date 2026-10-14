//! One-time carry-over from the original app's identifier.
//!
//! `tauri-plugin-store` keeps `settings.json` (theme, language, layout…) under
//! `<app data>/<identifier>/`. Renaming the identifier would otherwise start
//! `AgentKB` with factory settings, so the first launch copies the file across.
//! It is a copy, not a move: the original app keeps working untouched.

use std::path::Path;

use super::fsutil;

pub const APP_IDENTIFIER: &str = "io.github.moqi777.agentkb";
pub const LEGACY_IDENTIFIER: &str = "com.claude.history-viewer";
const STORE_FILE: &str = "settings.json";

/// `app_data_root` is the directory that holds one folder per identifier
/// (`~/Library/Application Support`). Returns whether a file was copied.
pub fn migrate_store_settings(app_data_root: &Path) -> Result<bool, String> {
    let from = app_data_root.join(LEGACY_IDENTIFIER).join(STORE_FILE);
    let to = app_data_root.join(APP_IDENTIFIER).join(STORE_FILE);
    if to.exists() || !from.is_file() {
        return Ok(false);
    }
    fsutil::copy_atomic(&from, &to)?;
    Ok(true)
}

/// Best effort at startup: a failure is logged and never blocks the app.
pub fn migrate_store_settings_at_startup() {
    let Some(root) = crate::utils::config_dir() else {
        return;
    };
    match migrate_store_settings(&root) {
        Ok(true) => log::info!("copied settings.json from the original app's data folder"),
        Ok(false) => {}
        Err(e) => log::warn!("could not copy the original app's settings: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_match_tauri_conf() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], APP_IDENTIFIER);
    }

    #[test]
    fn settings_are_copied_once_and_the_original_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join(LEGACY_IDENTIFIER);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join(STORE_FILE), br#"{"language":"zh-CN"}"#).unwrap();

        assert!(migrate_store_settings(root.path()).unwrap());
        let new = root.path().join(APP_IDENTIFIER).join(STORE_FILE);
        assert_eq!(std::fs::read(&new).unwrap(), br#"{"language":"zh-CN"}"#);
        assert!(old.join(STORE_FILE).is_file(), "copy, not move");

        // The user changed a setting in AgentKB; a later launch must not undo it.
        std::fs::write(&new, br#"{"language":"en"}"#).unwrap();
        assert!(!migrate_store_settings(root.path()).unwrap());
        assert_eq!(std::fs::read(&new).unwrap(), br#"{"language":"en"}"#);
    }

    #[test]
    fn nothing_to_do_without_an_original_file() {
        let root = tempfile::tempdir().unwrap();
        assert!(!migrate_store_settings(root.path()).unwrap());
        assert!(!root.path().join(APP_IDENTIFIER).exists());
    }
}
