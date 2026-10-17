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
| --kb-show <source:session-id> [--around <seq>] [options] | --kb-skill-print claude|codex \
| --kb-digest --auto|--all|--session <source:session-id>|--test \
| --kb-embed --auto|--all|--status|--test \
| --kb-export <file.agentkb> [--overwrite] \
| --kb-import <file.agentkb> [--inspect|--plan] [--no-sync] [--no-settings] [--adopt-settings]";

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
        "--kb-digest" => run_digest_command(&args[2..]),
        "--kb-embed" => run_embed_command(&args[2..]),
        "--kb-backup" => run_backup_command(),
        "--kb-export" => run_export_command(&args[2..]),
        "--kb-import" => run_import_command(&args[2..]),
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
            if trigger != "cli" && matches!(summary.status.as_str(), "ok" | "partial") {
                start_digest_if_due(&config);
                start_embed_if_enabled(&config);
            }
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

/// After a collection: when semantic search is on, let the vector builder
/// catch up (a separate process; it exits at once when there is nothing to do).
fn start_embed_if_enabled(config: &KbConfig) {
    if !config.semantic.enabled {
        return;
    }
    let data = super::paths::DataPaths::new(config.data_dir.clone());
    if super::semantic::build::is_running(&data) {
        return;
    }
    if let Err(e) = std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|exe| {
            super::semantic::build::spawn_background(&exe, &data, &["--kb-embed", "--auto"])
        })
    {
        eprintln!("vector builder not started: {e}");
    }
}

/// After a collection: if a digest round is due (or unfinished and its retry
/// time has come), start `--kb-digest --auto` as a separate process and move
/// on. The collection never waits for the AI, and a problem here never
/// changes the collection's own result.
fn start_digest_if_due(config: &KbConfig) {
    let tz = *chrono::Local::now().offset();
    match super::digest::due_check(config, chrono::Utc::now(), tz) {
        Ok(false) => {}
        Ok(true) => {
            let started = std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| {
                    super::digest::spawn_background(
                        &exe,
                        &super::paths::DataPaths::new(config.data_dir.clone()),
                        &["--kb-digest", "--auto"],
                        &[],
                    )
                });
            if let Err(e) = started {
                eprintln!("digest not started: {e}");
            }
        }
        Err(e) => eprintln!("digest check failed: {e}"),
    }
}

/// `--kb-embed --auto | --all | --status | --test`: the vector builder.
fn run_embed_command(args: &[String]) -> i32 {
    use super::semantic::{build, state};
    let flags = match parse_flags(args, &[], &["auto", "all", "status", "test"]) {
        Ok(flags) => flags,
        Err(e) => return usage_error(&e),
    };
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    let data = super::paths::DataPaths::new(config.data_dir.clone());
    if flags.switches.contains("status") {
        let mut st = state::read(&data);
        if st.status == "running" && !build::is_running(&data) {
            st.status = "idle".into();
        }
        print_json(&st);
        return 0;
    }
    if flags.switches.contains("test") {
        let report = build::test_model(&data);
        print_json(&report);
        return i32::from(!report.ok);
    }
    let opts = if flags.switches.contains("all") {
        build::Options::everything()
    } else if flags.switches.contains("auto") {
        if !config.semantic.enabled {
            return 0;
        }
        build::Options::automatic()
    } else {
        return usage_error("--kb-embed needs --auto, --all, --status or --test");
    };
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    match build::run_real(&data, &opts, &cancel) {
        Ok(outcome) => {
            print_json(
                &serde_json::json!({"outcome": format!("{outcome:?}"), "state": state::read(&data)}),
            );
            0
        }
        Err(e) => {
            eprintln!("{e}");
            let mut st = state::read(&data);
            st.status = "error".into();
            st.message.clone_from(&e);
            st.pid = None;
            state::write(&data, &st);
            print_json(&serde_json::json!({"outcome": "error", "message": e}));
            1
        }
    }
}

/// `--kb-digest --auto | --all | --session <source:id> | --test`.
fn run_digest_command(args: &[String]) -> i32 {
    use super::digest::{self, DigestEnv, Scope, Trigger};
    let flags = match parse_flags(args, &["session"], &["auto", "all", "test"]) {
        Ok(flags) => flags,
        Err(e) => return usage_error(&e),
    };
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    let env = DigestEnv::from_config(&config);
    if flags.switches.contains("test") {
        let report = digest::status::test_call(&env);
        print_json(&report);
        return i32::from(!report.ok);
    }
    let (trigger, scope) = if flags.switches.contains("auto") {
        (Trigger::Auto, Scope::Auto)
    } else if flags.switches.contains("all") {
        (manual_trigger(), Scope::All)
    } else if let Some(target) = flags.values.get("session") {
        let Some((source, session_id)) = target.split_once(':') else {
            return usage_error("--session needs <source>:<session id>, e.g. claude:8e6f1373-…");
        };
        if super::Source::parse(source).is_none() || !super::paths::is_safe_id(session_id) {
            return usage_error("--session needs claude:<id> or codex:<id>");
        }
        (
            manual_trigger(),
            Scope::Session {
                source: source.to_string(),
                session_id: session_id.to_string(),
            },
        )
    } else {
        return usage_error("--kb-digest needs --auto, --all, --session <id> or --test");
    };
    digest::stop_on_sigterm(&env.cancel);
    match digest::run(&env, trigger, &scope) {
        Ok(summary) => {
            print_json(&summary);
            0
        }
        Err(e) => {
            eprintln!("{e}");
            print_json(&serde_json::json!({"status": "error", "message": e}));
            1
        }
    }
}

/// Who started a manual run: the app sets the variable, a person at a
/// terminal does not.
fn manual_trigger() -> super::digest::Trigger {
    match std::env::var(TRIGGER_ENV).as_deref() {
        Ok("gui") => super::digest::Trigger::Gui,
        _ => super::digest::Trigger::Cli,
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
            "notes",
        ],
        &["no-subagents", "include-subagents", "verbose", "no-notes"],
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
            expand_subagents: flags.switches.contains("include-subagents").then_some(true),
            mode: flags.values.get("mode").cloned(),
            kinds: flags.list("kinds"),
            notes: if flags.switches.contains("no-notes") {
                Some(0)
            } else {
                flags.number("notes")?
            },
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
            let verbose = flags.switches.contains("verbose");
            print_output(
                format,
                &super::search::agent_json(&response, verbose),
                || super::search::format_search_text(&response),
            );
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
        &["full", "verbose"],
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
            let verbose = flags.switches.contains("verbose");
            print_output(
                format,
                &super::search::agent_json(&response, verbose),
                || super::search::format_show_text(&response),
            );
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

/// Phase changes and every tenth of the bytes, on stderr.
fn transfer_progress() -> super::progress::ProgressSink {
    let last = std::sync::Mutex::new((String::new(), 11u64));
    super::progress::ProgressSink::new(move |p| {
        let tenth = (p.bytes_done * 10).checked_div(p.bytes_total).unwrap_or(0);
        let Ok(mut last) = last.lock() else { return };
        if last.0 != p.phase || last.1 != tenth {
            *last = (p.phase.clone(), tenth);
            eprintln!(
                "{} {}: {}/{} files, {}/{} bytes",
                p.op, p.phase, p.done, p.total, p.bytes_done, p.bytes_total
            );
        }
    })
}

/// `~` expanded and a relative path taken from the current directory.
fn user_path(text: &str) -> std::path::PathBuf {
    let path = super::migrate_dir::expand(text);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
    }
}

fn transfer_failure(e: &super::transfer::TransferError) -> i32 {
    eprintln!("{e}");
    print_json(&serde_json::json!({ "error": e.code, "message": e.message }));
    1
}

fn run_export_command(args: &[String]) -> i32 {
    let flags = match parse_flags(args, &[], &["overwrite"]) {
        Ok(flags) => flags,
        Err(message) => return usage_error(&message),
    };
    let [dest] = flags.positional.as_slice() else {
        return usage_error("--kb-export needs the file to write");
    };
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    let env = KbEnv::from_config(&config);
    let dest = user_path(dest);
    match super::export::export(
        &env,
        &config,
        &dest,
        flags.switches.contains("overwrite"),
        &super::transfer::Cancel::new(),
        &transfer_progress(),
    ) {
        Ok(report) => {
            print_json(&report);
            0
        }
        Err(e) => transfer_failure(&e),
    }
}

fn run_import_command(args: &[String]) -> i32 {
    let flags = match parse_flags(
        args,
        &[],
        &[
            "inspect",
            "plan",
            "no-sync",
            "no-settings",
            "adopt-settings",
        ],
    ) {
        Ok(flags) => flags,
        Err(message) => return usage_error(&message),
    };
    let [package] = flags.positional.as_slice() else {
        return usage_error("--kb-import needs the file to read");
    };
    let config_file = match super::config::config_path() {
        Ok(file) => file,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    let package = user_path(package);
    let env = KbEnv::from_config(&config);
    if flags.switches.contains("inspect") {
        return match super::import::inspect(&env, &package) {
            Ok(report) => {
                print_json(&report);
                i32::from(report.problem.is_some())
            }
            Err(e) => transfer_failure(&e),
        };
    }
    let opts = super::import::Options {
        adopt_settings: flags.switches.contains("adopt-settings"),
        write_config: !flags.switches.contains("no-settings"),
        config_exists: config_file.is_file(),
    };
    let cancel = super::transfer::Cancel::new();
    let progress = transfer_progress();
    if flags.switches.contains("plan") {
        let prepared = super::import::prepare(&env, &config, &opts, &package, &cancel, &progress);
        let _ = super::import::discard(&env, &package);
        return match prepared {
            Ok(prepared) => {
                print_json(&prepared);
                0
            }
            Err(e) => transfer_failure(&e),
        };
    }
    let report = match super::import::import(
        &env,
        &config,
        &opts,
        &config_file,
        &package,
        &cancel,
        &progress,
    ) {
        Ok(report) => report,
        Err(e) => return transfer_failure(&e),
    };
    if report.config_written && !opts.config_exists {
        eprintln!(
            "settings saved; the scheduled collection is not installed yet: \
             run --kb-schedule install (or open the app)"
        );
    }
    // Recognise sources that exist here, apply this machine's exclusions and
    // bring an out-of-date index up to date.
    let synced = if flags.switches.contains("no-sync") {
        None
    } else {
        let env = KbEnv::from_config(&report.config).with_progress(progress);
        Some(sync::run_sync(&env, "cli"))
    };
    let code = match &synced {
        Some(Ok(summary)) if summary.status == "error" => 1,
        Some(Err(_)) => 1,
        _ => 0,
    };
    print_json(&serde_json::json!({
        "import": report,
        "sync": synced.map(|s| match s {
            Ok(summary) => serde_json::to_value(summary).unwrap_or_default(),
            Err(e) => serde_json::json!({ "error": e }),
        }),
    }));
    code
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
