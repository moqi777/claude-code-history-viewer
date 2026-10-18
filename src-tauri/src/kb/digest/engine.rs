//! Running the user's own `claude -p` / `codex exec` once, with everything
//! that could read files, run commands, reach the network or load the user's
//! setup switched off.
//!
//! The flags were measured on this machine (see `docs/agent-kb/P4-design.md`
//! §5 and `p4-evidence/`); the tests below pin them so a refactor cannot
//! quietly drop one. The session text goes in through stdin, never argv, and
//! stdin is always closed after writing: `codex exec` waits for EOF forever.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

pub const SYSTEM_PROMPT: &str = include_str!("prompt.txt");
const SCHEMA_CLAUDE: &str = include_str!("schema.json");
const SCHEMA_CODEX: &str = include_str!("schema-codex.json");

/// One call is given this long before its whole process group is killed.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(600);
/// Single-call spending guard. Only meaningful for pay-per-use API accounts;
/// on a subscription the per-round session cap is what limits usage.
const CLAUDE_MAX_BUDGET_USD: &str = "2";

const STDOUT_CAP: usize = 32 * 1024 * 1024;
const STDERR_CAP: usize = 64 * 1024;
const POLL: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Claude,
    Codex,
}

impl Engine {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// Why a call produced no usable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Usage limit reached: wait and retry later, without counting a failure.
    Quota(String),
    /// Not logged in / key rejected: same handling as quota.
    Auth(String),
    /// The CLI cannot be found or started.
    Config(String),
    /// Our own deadline expired.
    Timeout,
    /// The caller asked to stop.
    Cancelled,
    /// Anything else: counted against the session's three attempts.
    Other(String),
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Self::Quota(m) | Self::Auth(m) | Self::Config(m) | Self::Other(m) => m.clone(),
            Self::Timeout => "the AI did not answer in time".to_string(),
            Self::Cancelled => "cancelled".to_string(),
        }
    }

    /// Short machine name, shown in the UI and stored in `meta`.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Quota(_) => "quota",
            Self::Auth(_) => "auth",
            Self::Config(_) => "config",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Other(_) => "failed",
        }
    }

    /// Failures that say nothing about the session itself: retry later, and
    /// do not use up one of its attempts.
    pub const fn is_environmental(&self) -> bool {
        matches!(
            self,
            Self::Quota(_) | Self::Auth(_) | Self::Config(_) | Self::Cancelled
        )
    }
}

pub struct Call<'a> {
    pub engine: Engine,
    pub cli: &'a Path,
    /// Empty = the CLI's default model.
    pub model: &'a str,
    /// Codex only: reasoning effort, empty = its default.
    pub reasoning: &'a str,
    pub work_dir: &'a Path,
    pub system_prompt: &'a str,
    pub input: &'a str,
    pub timeout: Duration,
    pub cancel: &'a AtomicBool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// The answer's JSON text (`{"entries":[…]}`), not yet validated.
    pub answer: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The model that answered, when the CLI says so.
    pub model: Option<String>,
    pub duration_ms: u64,
    /// List-price cost of the call and the number of model turns it took, when
    /// the CLI reports them (Claude does). Logged, never used for decisions:
    /// on a subscription nothing is charged per call.
    pub cost_usd: Option<f64>,
    pub turns: Option<u64>,
}

// ---------------------------------------------------------------------------
// Command lines
// ---------------------------------------------------------------------------

pub fn claude_args(model: &str, system_prompt: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--no-session-persistence",
        "--safe-mode",
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        r#"{"mcpServers":{}}"#,
        "--disable-slash-commands",
        "--setting-sources",
        "",
        "--system-prompt",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(system_prompt.into());
    args.extend(
        [
            "--output-format",
            "json",
            "--json-schema",
            SCHEMA_CLAUDE.trim(),
            "--max-budget-usd",
            CLAUDE_MAX_BUDGET_USD,
        ]
        .map(OsString::from),
    );
    if !model.is_empty() {
        args.extend(["--model".into(), model.into()]);
    }
    args
}

pub fn codex_args(
    model: &str,
    reasoning: &str,
    work_dir: &Path,
    schema_file: &Path,
    out_file: &Path,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "exec",
        "--ephemeral",
        "--skip-git-repo-check",
        "--ignore-user-config",
        "--ignore-rules",
        "-s",
        "read-only",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    for feature in [
        "shell_tool",
        "unified_exec",
        "apps",
        "plugins",
        "browser_use",
        "computer_use",
        "multi_agent",
        "image_generation",
        "view_image",
        "skill_search",
        "hooks",
        "goals",
        "code_mode_host",
        "sleep_tool",
        "tool_suggest",
        "shell_snapshot",
    ] {
        args.extend(["--disable".into(), feature.into()]);
    }
    for setting in [
        r#"web_search="disabled""#,
        "mcp_servers={}",
        "skills.include_instructions=false",
    ] {
        args.extend(["-c".into(), setting.into()]);
    }
    if !reasoning.is_empty() {
        args.extend([
            "-c".into(),
            format!("model_reasoning_effort=\"{reasoning}\"").into(),
        ]);
    }
    args.extend([
        "--json".into(),
        "--output-schema".into(),
        schema_file.into(),
    ]);
    args.extend(["-o".into(), out_file.into()]);
    if !model.is_empty() {
        args.extend(["-m".into(), model.into()]);
    }
    args.extend(["-C".into(), work_dir.into(), "-".into()]);
    args
}

/// `<cli> --version`, first line: recorded in each note. Not an AI call.
pub fn cli_version(engine: Engine, cli: &Path) -> String {
    let Ok(mut command) = cli_command(engine, cli) else {
        return "unknown".into();
    };
    crate::kb::platform::no_window(&mut command);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|text| {
            text.split_whitespace()
                .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// npm's Windows shim goes through cmd.exe, which cannot carry our multiline
/// prompts safely. Use the package's declared bin directly, preserving argv.
#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn cli_command(engine: Engine, cli: &Path) -> Result<Command, Failure> {
    #[cfg(windows)]
    if cli
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"))
    {
        let failure = |detail: &str| {
            Failure::Config(format!(
            "cannot resolve npm launcher {}: {detail}; reinstall the CLI or select its executable in Settings",
            cli.display()
        ))
        };
        let prefix = cli.parent().ok_or_else(|| failure("missing parent"))?;
        let package = prefix.join("node_modules").join(match engine {
            Engine::Claude => "@anthropic-ai/claude-code",
            Engine::Codex => "@openai/codex",
        });
        let metadata: Value = std::fs::read(package.join("package.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| failure("package.json is missing or invalid"))?;
        let bin = metadata
            .get("bin")
            .and_then(|bin| {
                bin.as_str()
                    .or_else(|| bin.get(engine.as_str()).and_then(Value::as_str))
            })
            .ok_or_else(|| failure("package has no matching bin"))?;
        let relative = Path::new(bin);
        if relative.is_absolute()
            || relative.components().any(|c| {
                !matches!(
                    c,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
        {
            return Err(failure("bin must be inside the npm package"));
        }
        let entry = package.join(relative);
        if !entry.is_file() {
            return Err(failure("package bin does not exist"));
        }
        if entry
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            return Ok(Command::new(entry));
        }
        if !entry.extension().is_some_and(|ext| {
            ["js", "cjs", "mjs"]
                .iter()
                .any(|kind| ext.eq_ignore_ascii_case(kind))
        }) {
            return Err(failure("unsupported package bin type"));
        }
        let node = std::iter::once(prefix.join("node.exe"))
            .chain(std::env::var_os("PATH").into_iter().flat_map(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("node.exe"))
                    .collect::<Vec<_>>()
            }))
            .find(|path| path.is_file())
            .ok_or_else(|| failure("node.exe not found"))?;
        let mut command = Command::new(node);
        command.arg(entry);
        return Ok(command);
    }
    #[cfg(not(windows))]
    let _ = engine;
    Ok(Command::new(cli))
}

// ---------------------------------------------------------------------------
// Finding the CLI
// ---------------------------------------------------------------------------

/// The configured path when it exists, else the usual install places. A
/// background task has an empty `PATH`, so a bare name is not enough.
pub fn resolve_cli(engine: Engine, configured: Option<&Path>) -> Result<PathBuf, Failure> {
    if let Some(path) = configured {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(Failure::Config(format!(
                "{} not found at {}; open Settings and test again",
                engine.as_str(),
                path.display()
            )))
        };
    }
    candidate_paths(engine)
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| {
            Failure::Config(format!(
                "cannot find the {} command; open Settings and test again",
                engine.as_str()
            ))
        })
}

/// Ask the user's login shell where the command is, the way a terminal
/// would: the only way to see a `PATH` that a launchd job does not have. Used
/// by the settings page only (it runs the user's shell start-up files), with
/// a short deadline; the result is stored as an absolute path.
pub fn find_via_login_shell(engine: Engine) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        candidate_paths(engine).into_iter().find(|p| p.is_file())
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/zsh"));
        let mut child = Command::new(shell)
            .args(["-lic", &format!("command -v {}", engine.as_str())])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let reader = std::thread::spawn(move || read_capped(&mut out, 64 * 1024));
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        let text = String::from_utf8_lossy(&reader.join().ok()?).into_owned();
        text.lines()
            .map(str::trim)
            .rev()
            .find(|l| l.starts_with('/') && Path::new(l).is_file())
            .map(PathBuf::from)
    }
}

/// Places a CLI is usually installed, in order.
pub fn candidate_paths(engine: Engine) -> Vec<PathBuf> {
    let name = engine.as_str();
    let mut out = Vec::new();
    if let Some(home) = crate::utils::home_dir() {
        #[cfg(windows)]
        {
            let local = home.join(".local/bin");
            out.push(local.join(format!("{name}.exe")));
            out.push(local.join(format!("{name}.cmd")));
            if engine == Engine::Claude {
                out.push(home.join(".claude/local/claude.exe"));
                out.push(home.join(".claude/local/claude.cmd"));
            }
            let npm = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .map(|p| p.join("npm"));
            if let Some(npm) = npm {
                out.push(npm.join(format!("{name}.exe")));
                out.push(npm.join(format!("{name}.cmd")));
            }
        }
        #[cfg(not(windows))]
        {
            out.push(home.join(".local/bin").join(name));
            if engine == Engine::Claude {
                out.push(home.join(".claude/local").join(name));
            }
            out.push(home.join(".npm-global/bin").join(name));
        }
    }
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                out.push(dir.join(format!("{name}.exe")));
                out.push(dir.join(format!("{name}.cmd")));
            }
        }
        out
    }
    #[cfg(not(windows))]
    {
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            out.push(Path::new(dir).join(name));
        }
        if let Some(path) = std::env::var_os("PATH") {
            out.extend(std::env::split_paths(&path).map(|d| d.join(name)));
        }
        out
    }
}

#[cfg(all(test, windows))]
#[test]
#[serial_test::serial]
fn windows_candidates_never_include_extensionless_npm_shell_scripts() {
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("CCHV_TEST_HOME", root.path());
    for engine in [Engine::Claude, Engine::Codex] {
        let paths = candidate_paths(engine);
        assert_ne!(paths.len(), 0);
        assert!(paths
            .iter()
            .all(|p| matches!(p.extension().and_then(|s| s.to_str()), Some("exe" | "cmd"))));
        assert_eq!(paths[0].extension().unwrap(), "exe");
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

pub fn run(call: &Call<'_>) -> Result<Reply, Failure> {
    std::fs::create_dir_all(call.work_dir)
        .map_err(|e| Failure::Config(format!("cannot create {}: {e}", call.work_dir.display())))?;
    let started = Instant::now();
    let mut scratch = Scratch::default();
    let mut last_file: Option<PathBuf> = None;
    let (args, stdin_text) = match call.engine {
        Engine::Claude => (
            claude_args(call.model, call.system_prompt),
            call.input.to_string(),
        ),
        Engine::Codex => {
            let schema = scratch.file(call.work_dir, "schema", SCHEMA_CODEX)?;
            let out = scratch.reserve(call.work_dir, "last");
            let args = codex_args(call.model, call.reasoning, call.work_dir, &schema, &out);
            last_file = Some(out);
            (
                args,
                // Codex has no system-prompt option: the instructions lead the input.
                format!("{}\n\n{}", call.system_prompt, call.input),
            )
        }
    };
    let output = spawn_and_wait(call, &args, stdin_text)?;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut reply = match call.engine {
        Engine::Claude => parse_claude(&output)?,
        Engine::Codex => {
            let last = last_file.and_then(|p| std::fs::read_to_string(p).ok());
            parse_codex(&output, last.as_deref())?
        }
    };
    reply.duration_ms = duration_ms;
    Ok(reply)
}

/// Temp files in the working directory, removed on drop.
#[derive(Default)]
struct Scratch {
    paths: Vec<PathBuf>,
}

impl Scratch {
    fn reserve(&mut self, dir: &Path, stem: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = dir.join(format!(".{stem}-{}-{nanos}", std::process::id()));
        self.paths.push(path.clone());
        path
    }

    fn file(&mut self, dir: &Path, stem: &str, content: &str) -> Result<PathBuf, Failure> {
        let path = self.reserve(dir, stem);
        std::fs::write(&path, content)
            .map_err(|e| Failure::Config(format!("cannot write {}: {e}", path.display())))?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct Output {
    stdout: String,
    stderr: String,
    success: bool,
}

fn read_capped(mut source: impl Read, cap: usize) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    while let Ok(n) = source.read(&mut buf) {
        if n == 0 {
            break;
        }
        // Keep reading past the cap so the child never blocks on a full pipe.
        if data.len() < cap {
            data.extend_from_slice(&buf[..n.min(cap - data.len())]);
        }
    }
    data
}

#[allow(unsafe_code)] // `kill(2)` on our own child's process group
fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: plain syscall on the pid of our own child, which leads its
            // own process group (see `process_group(0)` below).
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_and_wait(
    call: &Call<'_>,
    args: &[OsString],
    stdin_text: String,
) -> Result<Output, Failure> {
    let mut command = cli_command(call.engine, call.cli)?;
    command
        .args(args)
        .current_dir(call.work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so a timeout or a cancel takes the CLI's own
    // helper processes down with it and nothing is left running.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    crate::kb::platform::no_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| Failure::Config(format!("cannot start {}: {e}", call.cli.display())))?;

    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(mut pipe) = stdin.take() {
            // A CLI that exits early closes the pipe; its exit status tells the story.
            let _ = pipe.write_all(stdin_text.as_bytes());
        }
        // Dropping the pipe is what sends EOF.
    });
    let stdout = child
        .stdout
        .take()
        .map(|s| std::thread::spawn(move || read_capped(s, STDOUT_CAP)));
    let stderr = child
        .stderr
        .take()
        .map(|s| std::thread::spawn(move || read_capped(s, STDERR_CAP)));

    let deadline = Instant::now() + call.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_group(&mut child);
                return Err(Failure::Other(format!(
                    "cannot wait for the AI process: {e}"
                )));
            }
        }
        if call.cancel.load(Ordering::SeqCst) {
            kill_group(&mut child);
            return Err(Failure::Cancelled);
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            return Err(Failure::Timeout);
        }
        std::thread::sleep(POLL);
    };
    let _ = writer.join();
    let text = |handle: Option<std::thread::JoinHandle<Vec<u8>>>| {
        handle
            .and_then(|h| h.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    Ok(Output {
        stdout: text(stdout),
        stderr: text(stderr),
        success: status.success(),
    })
}

// ---------------------------------------------------------------------------
// Reading the answers
// ---------------------------------------------------------------------------

static QUOTA: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)hit your .{0,40}limit|usage limit|session limit|rate[ _-]?limit|credit balance|quota|too many requests",
    )
    .expect("valid quota regex")
});
static AUTH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)not logged in|please (?:run )?/?log ?in|log in again|invalid api key|authentication|unauthori[sz]ed|forbidden|expired token|\blogin\b",
    )
    .expect("valid auth regex")
});

/// Sort an error report into [`Failure`]s. `status` is the HTTP status when
/// the CLI gives one.
pub fn classify(status: Option<u64>, text: &str) -> Failure {
    let shown = clip(text, 300);
    if status == Some(429) || QUOTA.is_match(text) {
        Failure::Quota(shown)
    } else if matches!(status, Some(401 | 403)) || AUTH.is_match(text) {
        Failure::Auth(shown)
    } else {
        Failure::Other(shown)
    }
}

fn clip(text: &str, max_chars: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(max_chars).collect()
}

fn token_count(usage: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .filter_map(|k| usage.get(*k).and_then(Value::as_u64))
        .sum()
}

fn parse_claude(output: &Output) -> Result<Reply, Failure> {
    let Ok(value) = serde_json::from_str::<Value>(output.stdout.trim()) else {
        let detail = if output.stderr.trim().is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        return Err(if output.success {
            Failure::Other("the answer is not JSON".to_string())
        } else {
            classify(None, detail)
        });
    };
    if value.get("is_error").and_then(Value::as_bool) == Some(true) || !output.success {
        let status = value.get("api_error_status").and_then(Value::as_u64);
        let text = value
            .get("result")
            .and_then(Value::as_str)
            .map_or_else(|| output.stderr.clone(), str::to_string);
        return Err(classify(status, &text));
    }
    let answer = match value.get("structured_output") {
        Some(structured) if !structured.is_null() => structured.to_string(),
        _ => value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };
    let usage = value.get("usage").cloned().unwrap_or(Value::Null);
    Ok(Reply {
        answer,
        input_tokens: token_count(
            &usage,
            &[
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
            ],
        ),
        output_tokens: token_count(&usage, &["output_tokens"]),
        model: value
            .get("modelUsage")
            .and_then(Value::as_object)
            .and_then(|m| m.keys().next().cloned()),
        duration_ms: 0,
        cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
        turns: value.get("num_turns").and_then(Value::as_u64),
    })
}

fn parse_codex(output: &Output, last_message: Option<&str>) -> Result<Reply, Failure> {
    let mut usage = Value::Null;
    let mut failure: Option<String> = None;
    let mut completed = false;
    let mut turn_failed = false;
    let mut last_agent_text: Option<String> = None;
    for line in output.stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("turn.completed") => {
                usage = event.get("usage").cloned().unwrap_or(Value::Null);
                completed = true;
            }
            Some("turn.failed") => {
                turn_failed = true;
                failure = Some(
                    event
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("turn failed")
                        .to_string(),
                );
            }
            Some("error") if failure.is_none() => {
                failure = event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Some("item.completed") => {
                if let Some(text) =
                    event
                        .pointer("/item/text")
                        .and_then(Value::as_str)
                        .filter(|_| {
                            event.pointer("/item/type").and_then(Value::as_str)
                                == Some("agent_message")
                        })
                {
                    last_agent_text = Some(text.to_string());
                }
            }
            _ => {}
        }
    }
    // `error` also carries transient reconnect notices. A completed turn with
    // a successful exit supersedes those notices, but never a terminal failure.
    if turn_failed || !completed || !output.success {
        if let Some(message) = failure {
            return Err(codex_failure(&message));
        }
    }
    if !output.success {
        let detail = if output.stderr.trim().is_empty() {
            "the AI process failed"
        } else {
            &output.stderr
        };
        return Err(codex_failure(detail));
    }
    let answer = last_message
        .map(str::to_string)
        .filter(|t| !t.trim().is_empty())
        .or(last_agent_text)
        .ok_or_else(|| Failure::Other("the AI returned no answer".to_string()))?;
    Ok(Reply {
        answer,
        input_tokens: token_count(&usage, &["input_tokens"]),
        output_tokens: token_count(&usage, &["output_tokens", "reasoning_output_tokens"]),
        model: None,
        duration_ms: 0,
        cost_usd: None,
        turns: None,
    })
}

fn codex_failure(message: &str) -> Failure {
    let lower = message.to_ascii_lowercase();
    if lower.contains("workspace routing discovery failed")
        || lower.contains("workspace routing discovery timed out")
    {
        Failure::Other(format!(
            "Codex 工作区路由连接失败或超时。请检查网络/代理，在 Codex 中确认当前工作区可用，再到每日整理设置点“测试”后重试。原始错误：{}",
            clip(message, 120)
        ))
    } else {
        classify(status_in(message), message)
    }
}

/// `"status": 429` inside an error message that is itself JSON text.
fn status_in(message: &str) -> Option<u64> {
    static STATUS: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#""status"\s*:\s*(\d{3})"#).expect("valid status regex"));
    STATUS.captures(message).and_then(|c| c[1].parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    #[serial_test::serial]
    fn npm_native_launcher_preserves_multiline_prompt_and_isolation_args() {
        use crate::kb::digest::testutil::FakeAi;
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("用户 with space & (npm)");
        let package = prefix.join("node_modules/@anthropic-ai/claude-code");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        let fake = FakeAi::new(root.path());
        std::fs::copy(&fake.script, package.join("bin/claude.exe")).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"bin":{"claude":"bin/claude.exe"}}"#,
        )
        .unwrap();
        let cli = prefix.join("claude.cmd");
        std::fs::write(&cli, "@echo off\r\nexit /b 99\r\n").unwrap();
        let args = claude_args("model & echo injected", SYSTEM_PROMPT);
        // Regression: Rust rejects multiline batch arguments before launching.
        assert_eq!(
            Command::new(&cli).args(&args).output().unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        let cancel = AtomicBool::new(false);
        let call = Call {
            engine: Engine::Claude,
            cli: &cli,
            model: "model & echo injected",
            reasoning: "",
            work_dir: root.path(),
            system_prompt: SYSTEM_PROMPT,
            input: "中文\n假会话",
            timeout: Duration::from_secs(15),
            cancel: &cancel,
        };
        assert!(run(&call).is_ok());
        let actual: Vec<String> =
            serde_json::from_slice(&std::fs::read(fake.dir.join("argv-1.json")).unwrap()).unwrap();
        assert_eq!(actual.iter().map(OsString::from).collect::<Vec<_>>(), args);
        assert_eq!(fake.input(1), call.input);
        assert_eq!(cli_version(Engine::Claude, &cli), "9.9.9");
    }

    #[cfg(windows)]
    #[test]
    fn npm_node_launchers_preserve_empty_json_unicode_and_shell_metacharacters() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("用户 with space & (npm)");
        for (engine, package_name) in [
            (Engine::Claude, "@anthropic-ai/claude-code"),
            (Engine::Codex, "@openai/codex"),
        ] {
            let package = prefix.join("node_modules").join(package_name);
            std::fs::create_dir_all(&package).unwrap();
            std::fs::write(
                package.join("package.json"),
                serde_json::json!({"bin":{engine.as_str():"cli.cjs"}}).to_string(),
            )
            .unwrap();
            std::fs::write(package.join("cli.cjs"), "let input=''; process.stdin.setEncoding('utf8'); process.stdin.on('data', c=>input+=c); process.stdin.on('end',()=>console.log(JSON.stringify({args:process.argv.slice(2),input}))); ").unwrap();
            let cli = prefix.join(format!("{}.cmd", engine.as_str()));
            std::fs::write(&cli, "@exit /b 99\r\n").unwrap();
            let cancel = AtomicBool::new(false);
            let call = Call {
                engine,
                cli: &cli,
                model: "",
                reasoning: "",
                work_dir: root.path(),
                system_prompt: "",
                input: "中文\r\n假会话",
                timeout: Duration::from_secs(15),
                cancel: &cancel,
            };
            let mut args = match engine {
                Engine::Claude => claude_args("", SYSTEM_PROMPT),
                Engine::Codex => codex_args(
                    "",
                    "",
                    root.path(),
                    &root.path().join("schema.json"),
                    &root.path().join("last.json"),
                ),
            };
            args.extend(
                [
                    "",
                    "中文\r\n第二行",
                    "\" & echo injected > marker & \"",
                    "%PATH% !VAR! ^ | < > ( )",
                    "C:\\with space\\",
                ]
                .map(OsString::from),
            );
            let output = spawn_and_wait(&call, &args, call.input.into()).unwrap();
            assert!(output.success, "{}", output.stderr);
            let value: Value = serde_json::from_str(&output.stdout).unwrap();
            assert_eq!(
                value["args"],
                serde_json::json!(args.iter().map(|a| a.to_str().unwrap()).collect::<Vec<_>>())
            );
            assert_eq!(value["input"], call.input);
            assert!(!root.path().join("marker").exists());
            std::fs::write(
                package.join("package.json"),
                r#"{"bin":"../../outside.js"}"#,
            )
            .unwrap();
            assert!(cli_command(engine, &cli).is_err());
        }
    }

    fn has_pair(args: &[OsString], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn claude_runs_with_every_isolation_flag() {
        let args = claude_args("", "PROMPT");
        assert_eq!(args[0], "-p");
        for flag in [
            "--no-session-persistence",
            "--safe-mode",
            "--strict-mcp-config",
            "--disable-slash-commands",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag}");
        }
        assert!(has_pair(&args, "--tools", ""), "built-in tools off");
        assert!(has_pair(&args, "--setting-sources", ""));
        assert!(has_pair(&args, "--mcp-config", r#"{"mcpServers":{}}"#));
        assert!(has_pair(&args, "--system-prompt", "PROMPT"));
        assert!(has_pair(&args, "--output-format", "json"));
        assert!(args.iter().any(|a| a == "--json-schema"));
        assert!(
            !args.iter().any(|a| a == "--model"),
            "empty model = CLI default"
        );
        assert!(has_pair(&claude_args("haiku", "P"), "--model", "haiku"));
    }

    #[test]
    fn codex_runs_with_every_isolation_flag_and_reads_stdin() {
        let args = codex_args(
            "",
            "",
            Path::new("/w"),
            Path::new("/w/s"),
            Path::new("/w/o"),
        );
        assert_eq!(args[0], "exec");
        for flag in [
            "--ephemeral",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ignore-rules",
            "--json",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag}");
        }
        assert!(has_pair(&args, "-s", "read-only"));
        for feature in ["shell_tool", "code_mode_host", "apps", "plugins", "hooks"] {
            assert!(has_pair(&args, "--disable", feature), "{feature}");
        }
        assert!(has_pair(&args, "-c", r#"web_search="disabled""#));
        assert!(has_pair(&args, "-c", "mcp_servers={}"));
        assert!(has_pair(&args, "-C", "/w"));
        assert_eq!(args.last().unwrap(), "-", "prompt comes from stdin");
        assert!(has_pair(
            &codex_args(
                "gpt-x",
                "",
                Path::new("/w"),
                Path::new("/s"),
                Path::new("/o")
            ),
            "-m",
            "gpt-x"
        ));
        let low = codex_args("", "low", Path::new("/w"), Path::new("/s"), Path::new("/o"));
        assert!(has_pair(&low, "-c", r#"model_reasoning_effort="low""#));
        assert!(
            !args
                .iter()
                .any(|a| a.to_string_lossy().contains("model_reasoning_effort")),
            "no effort is set unless asked for"
        );
    }

    #[test]
    fn quota_and_auth_errors_are_told_apart_from_ordinary_failures() {
        assert!(matches!(classify(Some(429), "anything"), Failure::Quota(_)));
        assert!(matches!(
            classify(
                None,
                "You've hit your session limit · resets 8:20pm (Asia/Shanghai)"
            ),
            Failure::Quota(_)
        ));
        assert!(matches!(
            classify(None, "You've hit your usage limit. Upgrade to Pro"),
            Failure::Quota(_)
        ));
        assert!(matches!(
            classify(Some(401), "Invalid API key · Please run /login"),
            Failure::Auth(_)
        ));
        assert!(matches!(classify(None, "Not logged in"), Failure::Auth(_)));
        assert!(matches!(
            classify(Some(404), "There's an issue with the selected model"),
            Failure::Other(_)
        ));
        assert!(Failure::Quota(String::new()).is_environmental());
        assert!(!Failure::Timeout.is_environmental());
        assert!(!Failure::Other(String::new()).is_environmental());
    }

    fn output(stdout: &str, success: bool) -> Output {
        Output {
            stdout: stdout.into(),
            stderr: String::new(),
            success,
        }
    }

    #[test]
    fn claude_json_gives_the_answer_tokens_and_model() {
        let out = output(
            r#"{"is_error":false,"structured_output":{"entries":[]},"usage":{"input_tokens":3,"cache_creation_input_tokens":4,"cache_read_input_tokens":5,"output_tokens":6},"modelUsage":{"claude-x":{}}}"#,
            true,
        );
        let reply = parse_claude(&out).unwrap();
        assert_eq!(reply.answer, r#"{"entries":[]}"#);
        assert_eq!((reply.input_tokens, reply.output_tokens), (12, 6));
        assert_eq!(reply.model.as_deref(), Some("claude-x"));
    }

    #[test]
    fn claude_error_json_is_classified() {
        let out = output(
            r#"{"is_error":true,"api_error_status":429,"result":"You've hit your session limit"}"#,
            false,
        );
        assert!(matches!(parse_claude(&out), Err(Failure::Quota(_))));
        let out = output(
            r#"{"is_error":true,"api_error_status":404,"result":"There's an issue with the selected model"}"#,
            false,
        );
        assert!(matches!(parse_claude(&out), Err(Failure::Other(_))));
        assert!(matches!(
            parse_claude(&output("not json", true)),
            Err(Failure::Other(_))
        ));
    }

    #[test]
    fn codex_reconnect_notices_do_not_discard_a_completed_answer() {
        let warning = r#"{"type":"error","message":"Reconnecting... 2/5 (workspace routing discovery timed out)"}"#;
        let completed =
            r#"{"type":"turn.completed","usage":{"input_tokens":123,"output_tokens":45}}"#;
        let stdout = format!("{warning}\n{completed}\n");
        let reply = parse_codex(&output(&stdout, true), Some(r#"{"entries":[]}"#)).unwrap();
        assert_eq!((reply.input_tokens, reply.output_tokens), (123, 45));
        for (text, success) in [
            (warning.to_string(), false),
            (stdout.clone(), false),
            (format!("{stdout}{{\"type\":\"turn.failed\",\"error\":{{\"message\":\"workspace routing discovery failed\"}}}}\n"), true),
        ] {
            let failure = parse_codex(&output(&text, success), Some(r#"{"entries":[]}"#)).unwrap_err();
            assert!(matches!(failure, Failure::Other(_)));
            assert!(failure.message().contains("网络/代理"));
            assert!(failure.message().contains("workspace routing discovery"));
        }
        let stderr = Output {
            stdout: String::new(),
            stderr: "workspace routing discovery failed".into(),
            success: false,
        };
        assert!(parse_codex(&stderr, None)
            .unwrap_err()
            .message()
            .contains("网络/代理"));
    }

    #[test]
    fn codex_events_give_tokens_and_failures() {
        let ok = output(
            "{\"type\":\"thread.started\"}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"reasoning_output_tokens\":3}}\n",
            true,
        );
        let reply = parse_codex(&ok, Some("{\"entries\":[]}")).unwrap();
        assert_eq!((reply.input_tokens, reply.output_tokens), (10, 5));

        let quota = output(
            "{\"type\":\"turn.failed\",\"error\":{\"message\":\"You've hit your usage limit.\"}}\n",
            false,
        );
        assert!(matches!(parse_codex(&quota, None), Err(Failure::Quota(_))));
        let bad_model = output(
            "{\"type\":\"error\",\"message\":\"{\\\"type\\\":\\\"error\\\",\\\"status\\\":400}\"}\n",
            false,
        );
        assert!(matches!(
            parse_codex(&bad_model, None),
            Err(Failure::Other(_))
        ));
    }

    #[test]
    fn a_missing_cli_is_a_configuration_problem() {
        let missing = Path::new("/definitely/not/here/claude");
        assert!(matches!(
            resolve_cli(Engine::Claude, Some(missing)),
            Err(Failure::Config(_))
        ));
    }
}
