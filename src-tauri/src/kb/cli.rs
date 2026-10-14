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

const USAGE: &str = "usage: --kb-sync | --kb-reindex | --kb-backup | --kb-move-data <new dir> \
| --kb-schedule install|uninstall|status|ensure | --kb-search \"<words>\" [options] \
| --kb-show <source:session-id> [--around <seq>] [options] | --kb-skill-print claude|codex";

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
        "--kb-reindex" => run_reindex_command(),
        "--kb-search" => run_search_command(&args[2..]),
        "--kb-show" => run_show_command(&args[2..]),
        "--kb-skill-print" => run_skill_print_command(args.get(2).map(String::as_str)),
        "--kb-backup" => run_backup_command(),
        "--kb-move-data" => run_move_data_command(args.get(2).map(String::as_str)),
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

/// Rebuild the search index (and every other derived row) from `raw/`.
fn run_reindex_command() -> i32 {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    let mut env = KbEnv::from_config(&config);
    env.force_reindex = true;
    match sync::run_sync(&env, "cli") {
        Ok(summary) => {
            print_json(&summary);
            i32::from(summary.status == "error" || summary.status == "partial")
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `--flag value` / `--flag=value` / bare switches, plus positionals.
#[derive(Default)]
struct Flags {
    positional: Vec<String>,
    values: std::collections::HashMap<String, String>,
    switches: std::collections::HashSet<String>,
}

fn parse_flags(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Flags, String> {
    let mut flags = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(rest) = arg.strip_prefix("--") {
            let (name, inline) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (rest, None),
            };
            if switches.contains(&name) {
                flags.switches.insert(name.to_string());
            } else if with_value.contains(&name) {
                let value = if let Some(v) = inline {
                    v
                } else {
                    i += 1;
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| format!("--{name} needs a value"))?
                };
                flags.values.insert(name.to_string(), value);
            } else {
                return Err(format!("unknown option --{name}"));
            }
        } else {
            flags.positional.push(arg.clone());
        }
        i += 1;
    }
    Ok(flags)
}

impl Flags {
    fn number(&self, name: &str) -> Result<Option<usize>, String> {
        self.values
            .get(name)
            .map(|v| {
                v.parse::<usize>()
                    .map_err(|_| format!("--{name} needs a number, got {v:?}"))
            })
            .transpose()
    }

    fn when(&self, name: &str) -> Result<Option<String>, String> {
        self.values
            .get(name)
            .map(|v| super::search::parse_when(v))
            .transpose()
    }

    fn list(&self, name: &str) -> Option<Vec<String>> {
        self.values
            .get(name)
            .map(|v| v.split(',').map(|k| k.trim().to_string()).collect())
    }
}

/// Failure output shared by the read-only commands: one JSON object on
/// stdout (so an agent can read it) and the message on stderr.
fn report_error(error: &super::search::SearchError) -> i32 {
    eprintln!("{}", error.message());
    print_json(&serde_json::json!({
        "agentkb_marker": super::index::messages::MARKER,
        "error": error.message(),
    }));
    error.exit_code()
}

fn usage_error(message: &str) -> i32 {
    eprintln!("{message}\n{USAGE}");
    2
}

/// Where `--kb-search` / `--kb-show` read. `AGENTKB_DATA_DIR` points them at
/// another data folder (a restored backup, or a test copy) without touching
/// the configuration; the commands only ever read it.
fn data_paths() -> Result<super::paths::DataPaths, i32> {
    if let Some(dir) = std::env::var_os("AGENTKB_DATA_DIR").filter(|d| !d.is_empty()) {
        return Ok(super::paths::DataPaths::new(dir));
    }
    let config = load_config()?;
    Ok(super::paths::DataPaths::new(config.data_dir))
}

/// `--kb-skill-print claude|codex`: the skill text this program would install.
fn run_skill_print_command(target: Option<&str>) -> i32 {
    let Some(target) = target.and_then(super::skill::Target::parse) else {
        return usage_error("--kb-skill-print needs claude or codex");
    };
    match std::env::current_exe() {
        Ok(exe) => {
            let _ = write!(
                std::io::stdout(),
                "{}",
                super::skill::render(target, &exe.to_string_lossy())
            );
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn print_output<T: Serialize>(format: &str, value: &T, text: impl FnOnce() -> String) {
    if format == "text" {
        let _ = write!(std::io::stdout(), "{}", text());
    } else {
        print_json(value);
    }
}

/// `--kb-search "<words>"`: read-only; never takes the sync lock.
fn run_search_command(args: &[String]) -> i32 {
    let flags = match parse_flags(
        args,
        &[
            "source",
            "client",
            "project",
            "since",
            "until",
            "limit",
            "per-session",
            "mode",
            "format",
            "kinds",
        ],
        &["no-subagents"],
    ) {
        Ok(flags) => flags,
        Err(e) => return usage_error(&e),
    };
    let query = flags.positional.join(" ");
    if query.trim().is_empty() {
        return usage_error("--kb-search needs the words to look for");
    }
    let params = (|| -> Result<super::search::SearchParams, String> {
        Ok(super::search::SearchParams {
            query: query.clone(),
            source: flags.values.get("source").cloned(),
            client: flags.values.get("client").cloned(),
            project: flags.values.get("project").cloned(),
            since: flags.when("since")?,
            until: flags.when("until")?,
            limit: flags.number("limit")?,
            per_session: flags.number("per-session")?,
            include_subagents: flags.switches.contains("no-subagents").then_some(false),
            mode: flags.values.get("mode").cloned(),
            kinds: flags.list("kinds"),
        })
    })();
    let params = match params {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    let format = flags.values.get("format").map_or("json", String::as_str);
    let data = match data_paths() {
        Ok(d) => d,
        Err(code) => return code,
    };
    match super::search::search_in(&data, &params) {
        Ok(response) => {
            print_output(format, &response, || {
                super::search::format_search_text(&response)
            });
            0
        }
        Err(e) => report_error(&e),
    }
}

/// `--kb-show <session>`: the messages around a hit.
fn run_show_command(args: &[String]) -> i32 {
    let flags = match parse_flags(
        args,
        &[
            "around",
            "context",
            "head",
            "tail",
            "kinds",
            "max-chars",
            "format",
        ],
        &["full"],
    ) {
        Ok(flags) => flags,
        Err(e) => return usage_error(&e),
    };
    let Some(session) = flags.positional.first().cloned() else {
        return usage_error("--kb-show needs a session, e.g. codex:<session id>");
    };
    type Numbers = (
        Option<usize>,
        Option<usize>,
        Option<usize>,
        Option<usize>,
        Option<usize>,
    );
    let numbers = (|| -> Result<Numbers, String> {
        Ok((
            flags.number("around")?,
            flags.number("context")?,
            flags.number("head")?,
            flags.number("tail")?,
            flags.number("max-chars")?,
        ))
    })();
    let (around, context, head, tail, max_chars) = match numbers {
        Ok(n) => n,
        Err(e) => return usage_error(&e),
    };
    let params = super::search::ShowParams {
        session,
        around: around.map(|n| n as u64),
        context: context.map(|n| n as u64),
        head: head.map(|n| n as u64),
        tail: tail.map(|n| n as u64),
        kinds: flags.list("kinds"),
        max_chars,
        full: flags.switches.contains("full").then_some(true),
    };
    let format = flags.values.get("format").map_or("json", String::as_str);
    let data = match data_paths() {
        Ok(d) => d,
        Err(code) => return code,
    };
    match super::search::show_in(&data, &params) {
        Ok(response) => {
            print_output(format, &response, || {
                super::search::format_show_text(&response)
            });
            0
        }
        Err(e) => report_error(&e),
    }
}

/// Back up to the external disk now (the same pass a collection ends with).
fn run_backup_command() -> i32 {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    match super::backup::run_standalone(&KbEnv::from_config(&config), &super::volume::MacQuery) {
        Ok(summary) => {
            print_json(&summary);
            i32::from(summary.status == "error")
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Move the data directory (copy, verify byte for byte, then switch the config).
/// The same code the settings page runs.
fn run_move_data_command(new_dir: Option<&str>) -> i32 {
    let Some(new_dir) = new_dir else {
        eprintln!("{USAGE}");
        return 2;
    };
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    let file = match super::config::config_path() {
        Ok(file) => file,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let target = super::migrate_dir::expand(new_dir);
    let progress = super::progress::ProgressSink::new(|p| {
        if p.done == p.total || p.done % 200 == 0 {
            eprintln!("{} {}: {}/{}", p.op, p.phase, p.done, p.total);
        }
    });
    match super::migrate_dir::apply(&file, &config, &target, &progress) {
        Ok(report) => {
            print_json(&report);
            // The task's log paths live in the data directory.
            if config.schedule_enabled {
                if let Ok(updated) = KbConfig::load() {
                    if let Err(e) = schedule::install_now(&updated) {
                        eprintln!("moved, but the scheduled task could not be updated: {e}");
                        return 1;
                    }
                }
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn run_schedule_command(action: Option<&str>) -> i32 {
    if !matches!(action, Some("install" | "uninstall" | "status" | "ensure")) {
        eprintln!("{USAGE}");
        return 2;
    }
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    if action == Some("ensure") {
        return match schedule::ensure_current_now() {
            Ok(repair) => {
                print_json(&repair);
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
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
        assert_eq!(try_run(&args(&["app", "--kb-search"])), Some(2));
        assert_eq!(try_run(&args(&["app", "--kb-show"])), Some(2));
    }

    #[test]
    fn flags_accept_both_spellings_and_reject_strangers() {
        let flags = parse_flags(
            &args(&[
                "死锁 排查",
                "--limit=3",
                "--source",
                "codex",
                "--no-subagents",
            ]),
            &["limit", "source"],
            &["no-subagents"],
        )
        .unwrap();
        assert_eq!(flags.positional, vec!["死锁 排查"]);
        assert_eq!(flags.number("limit").unwrap(), Some(3));
        assert_eq!(
            flags.values.get("source").map(String::as_str),
            Some("codex")
        );
        assert!(flags.switches.contains("no-subagents"));
        assert!(parse_flags(&args(&["--bogus"]), &[], &[]).is_err());
        assert!(parse_flags(&args(&["--limit"]), &["limit"], &[]).is_err());
        assert!(parse_flags(&args(&["--limit", "x"]), &["limit"], &[])
            .unwrap()
            .number("limit")
            .is_err());
    }
}
