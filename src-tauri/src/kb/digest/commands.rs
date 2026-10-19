//! Tauri commands for the digest: the home page card, the settings page, the
//! notes page and the session's notes panel. Like the rest of `kb::commands`
//! they run the same code the command line does, and a click that starts a
//! digest starts the very process `--kb-sync` would (`--kb-digest`), so
//! closing the app never loses work.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::engine::{self, Engine};
use super::plan::Estimate;
use super::status::{self, StatusReport, TestReport};
use super::DigestEnv;
use crate::kb::config::{DigestConfig, KbConfig};
use crate::kb::paths::DataPaths;
use crate::kb::search::{self, NotesFilter, NotesPage, SearchParams};
use crate::kb::{db, notes};

type Res<T> = Result<T, String>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Res<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Task join error: {e}"))?
}

fn tz() -> chrono::FixedOffset {
    *chrono::Local::now().offset()
}

#[tauri::command]
pub async fn kb_digest_status() -> Res<StatusReport> {
    blocking(|| status::status(&KbConfig::load()?, Utc::now(), tz())).await
}

/// "整理全部未整理的": how many sessions and tokens, before the user confirms.
#[tauri::command]
pub async fn kb_digest_estimate_all() -> Res<EstimateReport> {
    blocking(|| {
        let config = KbConfig::load()?;
        Ok(EstimateReport {
            estimate: status::estimate_all(&config, Utc::now())?,
            engine: config.digest.engine,
            model: config.digest.model,
        })
    })
    .await
}

#[derive(Debug, Serialize)]
pub struct EstimateReport {
    #[serde(flatten)]
    pub estimate: Estimate,
    pub engine: String,
    pub model: String,
}

/// Settings page: "about X tokens a day, going by the last 7 days".
#[tauri::command]
pub async fn kb_digest_daily_tokens() -> Res<Option<u64>> {
    blocking(|| status::daily_tokens(&KbConfig::load()?, Utc::now(), tz())).await
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunRequest {
    All,
    Session { source: String, session_id: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunStarted {
    Started,
    /// Another digest holds the lock ("正在整理中").
    Busy,
    /// Nothing to do, with the reason.
    Nothing {
        /// Stable code the page translates: `no_kb`, `not_indexed`, `up_to_date`.
        reason: String,
        message: String,
    },
}

/// Start a manual digest in its own process and return at once; the page
/// follows progress through `kb_digest_status`.
#[tauri::command]
pub async fn kb_digest_run(request: RunRequest) -> Res<RunStarted> {
    blocking(move || {
        let config = KbConfig::load()?;
        let data = DataPaths::new(config.data_dir.clone());
        if super::running_pid(&data).is_some() {
            return Ok(RunStarted::Busy);
        }
        let args: Vec<String> = match &request {
            RunRequest::All => vec!["--kb-digest".into(), "--all".into()],
            RunRequest::Session { source, session_id } => {
                if !crate::kb::paths::is_safe_id(session_id)
                    || crate::kb::Source::parse(source).is_none()
                {
                    return Err("not a session".into());
                }
                if let Some((reason, message)) = session_blocker(&config, source, session_id)? {
                    return Ok(RunStarted::Nothing {
                        reason: reason.into(),
                        message: message.into(),
                    });
                }
                vec![
                    "--kb-digest".into(),
                    "--session".into(),
                    format!("{source}:{session_id}"),
                ]
            }
        };
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        super::spawn_background(
            &exe,
            &data,
            &borrowed,
            &[(crate::kb::schedule::TRIGGER_ENV, "gui")],
        )?;
        Ok(RunStarted::Started)
    })
    .await
}

/// Why "digest this one now" would do nothing, checked before a process is started.
pub(super) fn session_blocker(
    config: &KbConfig,
    source: &str,
    session_id: &str,
) -> Res<Option<(&'static str, &'static str)>> {
    let data = DataPaths::new(config.data_dir.clone());
    let Some(conn) = db::open_readonly(&data.db())? else {
        return Ok(Some(("no_kb", "there is no knowledge base yet")));
    };
    let Some(info) = super::input::session_info(&conn, source, session_id)? else {
        return Ok(Some(("not_indexed", "this session is not in the index")));
    };
    let digested = notes::digested_seq(&conn, source, session_id)?.unwrap_or(0);
    let has_new = super::input::next_chunk(
        &conn,
        &info,
        digested,
        Utc::now(),
        None,
        super::input::CHUNK_CHARS,
    )?
    .is_some();
    Ok((!has_new).then_some((
        "up_to_date",
        "nothing new in this session since its last note",
    )))
}

/// The "stop" button. True when a running digest was asked to stop.
#[tauri::command]
pub async fn kb_digest_cancel() -> Res<bool> {
    blocking(|| {
        let data = DataPaths::new(KbConfig::load()?.data_dir);
        Ok(super::request_stop(&data))
    })
    .await
}

/// Where a CLI lives, for the settings page to remember. Asks the login
/// shell (like a terminal would), then the usual install places.
#[tauri::command]
pub async fn kb_digest_find_cli(engine: String) -> Res<Option<String>> {
    blocking(move || {
        let engine = Engine::parse(&engine).ok_or("unknown AI")?;
        let found =
            engine::find_via_login_shell(engine).or_else(|| engine::resolve_cli(engine, None).ok());
        Ok(found.map(|p| p.display().to_string()))
    })
    .await
}

/// The settings page's "test" button: one real, tiny call with the settings
/// as they are on screen (not necessarily saved yet). The path is resolved
/// the way a background run would have to: absolute, from the login shell.
#[tauri::command]
pub async fn kb_digest_test(draft: DigestConfig) -> Res<TestReport> {
    blocking(move || {
        let mut config = KbConfig::load()?;
        config.digest = draft;
        config.digest.enabled = true;
        if config.digest.cli_path.is_none() {
            let engine = Engine::parse(&config.digest.engine).ok_or("unknown AI")?;
            config.digest.cli_path = engine::find_via_login_shell(engine);
        }
        let env = DigestEnv::from_config(&config);
        Ok(status::test_call(&env))
    })
    .await
}

// ---- notes -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NotesQuery {
    /// Words to look for; empty = newest first.
    pub query: Option<String>,
    #[serde(flatten)]
    pub filter: NotesFilter,
}

#[tauri::command]
pub async fn kb_notes_list(query: NotesQuery) -> Res<NotesPage> {
    blocking(move || {
        let data = DataPaths::new(KbConfig::load()?.data_dir);
        let words = query.query.as_deref().map(str::trim).unwrap_or_default();
        if words.is_empty() {
            return search::list_notes(&data, &query.filter).map_err(|e| e.message().to_string());
        }
        let params = SearchParams {
            query: words.to_string(),
            source: query.filter.source.clone(),
            since: query.filter.since.clone(),
            ..SearchParams::default()
        };
        let notes = search::find_notes_in(&data, &params, query.filter.limit.unwrap_or(50))
            .map_err(|e| e.message().to_string())?;
        Ok(NotesPage {
            total: notes.len(),
            notes,
        })
    })
    .await
}

/// One session's notes and how far it has been digested.
#[derive(Debug, Serialize)]
pub struct SessionNotes {
    pub notes: Vec<search::NoteHit>,
    /// `notes`, `nothing`, `partial`, or `None` when never digested.
    pub status: Option<String>,
    pub digested_at: Option<String>,
}

#[tauri::command]
pub async fn kb_notes_for_session(source: String, session_id: String) -> Res<SessionNotes> {
    blocking(move || {
        let data = DataPaths::new(KbConfig::load()?.data_dir);
        let page = search::list_notes(
            &data,
            &NotesFilter {
                source: Some(source.clone()),
                session_id: Some(session_id.clone()),
                limit: Some(200),
                ..NotesFilter::default()
            },
        )
        .map_err(|e| e.message().to_string())?;
        let file = notes::read(&data, &source, &session_id)?;
        Ok(SessionNotes {
            notes: page.notes,
            status: file.as_ref().map(|f| f.status.clone()),
            digested_at: file.map(|f| f.digested_at),
        })
    })
    .await
}
