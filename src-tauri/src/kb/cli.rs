//! Headless entry points: `--kb-sync` and `--kb-schedule install|uninstall|status`.
//!
//! Dispatched from `lib.rs::run` before any Tauri code runs, exactly like
//! `--export`, so no window opens and the single-instance plugin (registered
//! only inside `run_tauri`) is never involved.

use std::io::Write;

use serde::Serialize;

use super::config::KbConfig;
use super::schedule::{self, TRIGGER_ENV};
use super::sync::{self, KbEnv};

const USAGE: &str = "usage: --kb-sync | --kb-schedule install|uninstall|status";

/// `Some(exit code)` when `args` is a KB command, `None` to let the app start
/// normally. Only `argv[1]` is inspected, so a stray `--kb-…` value elsewhere
/// (a path, a `--session` hint) can never hijack a normal launch.
pub fn try_run(args: &[String]) -> Option<i32> {
    let command = args.get(1)?;
    if !command.starts_with("--kb-") {
        return None;
    }
    Some(match command.as_str() {
        "--kb-sync" => run_sync_command(),
        "--kb-schedule" => run_schedule_command(args.get(2).map(String::as_str)),
        other => {
            eprintln!("unknown KB command {other}\n{USAGE}");
            2
        }
    })
}

fn print_json(value: &impl Serialize) {
    let line = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    let _ = writeln!(std::io::stdout(), "{line}");
}

fn load_config() -> Result<KbConfig, i32> {
    KbConfig::load().map_err(|e| {
        eprintln!("{e}");
        1
    })
}

fn run_sync_command() -> i32 {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    let trigger = std::env::var(TRIGGER_ENV)
        .ok()
        .filter(|t| matches!(t.as_str(), "schedule" | "gui" | "cli"))
        .unwrap_or_else(|| "cli".to_string());
    match sync::run_sync(&KbEnv::from_config(&config), &trigger) {
        Ok(summary) => {
            print_json(&summary);
            // A held lock is a normal outcome, not a failure launchd should log.
            i32::from(summary.status == "error" || summary.status == "partial")
        }
        Err(e) => {
            eprintln!("{e}");
            print_json(
                &serde_json::json!({"status": "error", "errors": [{"stage": "sync", "message": e}]}),
            );
            1
        }
    }
}

fn run_schedule_command(action: Option<&str>) -> i32 {
    if !matches!(action, Some("install" | "uninstall" | "status")) {
        eprintln!("{USAGE}");
        return 2;
    }
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    let result = match action {
        Some("install") => schedule::install_now(&config),
        Some("uninstall") => schedule::uninstall_now(),
        _ => schedule::status_now(),
    };
    match result {
        Ok(status) => {
            print_json(&status);
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn only_the_first_argument_selects_a_kb_command() {
        assert_eq!(try_run(&args(&["app"])), None);
        assert_eq!(try_run(&args(&["app", "--session", "--kb-sync"])), None);
        assert_eq!(try_run(&args(&["app", "--export", "x", "--kb-sync"])), None);
        assert_eq!(try_run(&args(&["app", "--kb-bogus"])), Some(2));
        assert_eq!(try_run(&args(&["app", "--kb-schedule"])), Some(2));
    }
}
