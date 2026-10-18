//! The scheduled collection task: a launchd user agent that runs
//! `<this executable> --kb-sync` every N seconds and once at login, whether or
//! not the app window is open. Windows uses a per-user Task Scheduler task.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::config::KbConfig;

/// Deliberately independent of `tauri.conf.json`'s identifier: a user may have
/// the original app installed too, and sharing a Label would make the two
/// overwrite each other's task.
pub const LAUNCHD_LABEL: &str = "io.github.moqi777.agentkb.sync";

/// Set in the plist so the run can tell it was scheduled.
pub const TRIGGER_ENV: &str = "AGENTKB_TRIGGER";

/// What the OS service manager can do; faked in tests.
pub trait Launchctl {
    fn bootstrap(&self, domain: &str, plist: &Path) -> Result<(), String>;
    fn bootout(&self, target: &str) -> Result<(), String>;
    /// `Ok(None)` when the job is not loaded.
    fn print(&self, target: &str) -> Result<Option<String>, String>;
}

pub struct SystemLaunchctl;

impl SystemLaunchctl {
    fn run(args: &[&str]) -> Result<std::process::Output, String> {
        std::process::Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|e| format!("cannot run launchctl: {e}"))
    }
}

impl Launchctl for SystemLaunchctl {
    fn bootstrap(&self, domain: &str, plist: &Path) -> Result<(), String> {
        let out = Self::run(&["bootstrap", domain, &plist.to_string_lossy()])?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "launchctl bootstrap failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn bootout(&self, target: &str) -> Result<(), String> {
        let out = Self::run(&["bootout", target])?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn print(&self, target: &str) -> Result<Option<String>, String> {
        let out = Self::run(&["print", target])?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub struct PlistSpec<'a> {
    pub exe: &'a Path,
    pub interval_secs: u64,
    pub logs_dir: &'a Path,
    pub env: &'a [(String, String)],
}

pub fn plist_xml(spec: &PlistSpec<'_>) -> String {
    let s = |text: &str| format!("<string>{}</string>", xml_escape(text));
    let out_log = spec.logs_dir.join("launchd.out.log");
    let err_log = spec.logs_dir.join("launchd.err.log");
    let mut env_entries = String::new();
    for (k, v) in spec.env {
        env_entries.push_str(&format!("    <key>{}</key>\n    {}\n", xml_escape(k), s(v)));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
  <key>Label</key>\n  {label}\n\
  <key>ProgramArguments</key>\n  <array>\n    {exe}\n    {arg}\n  </array>\n\
  <key>StartInterval</key>\n  <integer>{interval}</integer>\n\
  <key>RunAtLoad</key>\n  <true/>\n\
  <key>StandardOutPath</key>\n  {out}\n\
  <key>StandardErrorPath</key>\n  {err}\n\
  <key>ProcessType</key>\n  <string>Background</string>\n\
  <key>EnvironmentVariables</key>\n  <dict>\n{env}  </dict>\n\
</dict>\n</plist>\n",
        label = s(LAUNCHD_LABEL),
        exe = s(&spec.exe.to_string_lossy()),
        arg = s("--kb-sync"),
        interval = spec.interval_secs,
        out = s(&out_log.to_string_lossy()),
        err = s(&err_log.to_string_lossy()),
        env = env_entries,
    )
}

pub struct Scheduler<'a> {
    pub launch_agents_dir: PathBuf,
    pub uid: String,
    pub exe: PathBuf,
    pub launchctl: &'a dyn Launchctl,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ScheduleStatus {
    pub supported: bool,
    pub label: String,
    pub plist_path: String,
    pub installed: bool,
    pub loaded: bool,
    pub interval_secs: Option<u64>,
    pub program: Option<String>,
    /// Executable named in the installed plist (what launchd will run), and
    /// whether it is still there. `false` means the task is dead (app moved
    /// or deleted).
    pub plist_program: Option<String>,
    pub program_exists: Option<bool>,
    /// launchd's own view: `running` / `not running` / `waiting`…
    pub state: Option<String>,
    pub last_exit_code: Option<i64>,
    pub runs: Option<i64>,
}

impl Scheduler<'_> {
    pub fn plist_path(&self) -> PathBuf {
        self.launch_agents_dir
            .join(format!("{LAUNCHD_LABEL}.plist"))
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self) -> String {
        format!("{}/{LAUNCHD_LABEL}", self.domain())
    }

    /// Idempotent: replaces any previous definition, so a changed interval
    /// takes effect.
    pub fn install(&self, config: &KbConfig) -> Result<ScheduleStatus, String> {
        let logs = super::paths::DataPaths::new(config.data_dir.clone()).logs();
        std::fs::create_dir_all(&logs)
            .map_err(|e| format!("cannot create {}: {e}", logs.display()))?;
        std::fs::create_dir_all(&self.launch_agents_dir)
            .map_err(|e| format!("cannot create {}: {e}", self.launch_agents_dir.display()))?;

        // launchd does not inherit the shell environment; carry over what
        // locates things: the home directory (so a run started with a different
        // HOME, such as a sandboxed check, keeps working on that same home) and
        // the two variables that move the source roots.
        let mut env = vec![(TRIGGER_ENV.to_string(), "schedule".to_string())];
        for key in ["HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
            if let Ok(value) = std::env::var(key) {
                env.push((key.to_string(), value));
            }
        }
        let xml = plist_xml(&PlistSpec {
            exe: &self.exe,
            interval_secs: config.interval_secs,
            logs_dir: &logs,
            env: &env,
        });

        let _ = self.launchctl.bootout(&self.target());
        write_atomic(&self.plist_path(), xml.as_bytes())?;
        self.launchctl
            .bootstrap(&self.domain(), &self.plist_path())?;
        self.status()
    }

    pub fn uninstall(&self) -> Result<ScheduleStatus, String> {
        let _ = self.launchctl.bootout(&self.target());
        match std::fs::remove_file(self.plist_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "cannot remove {}: {e}",
                    self.plist_path().display()
                ))
            }
        }
        self.status()
    }

    pub fn status(&self) -> Result<ScheduleStatus, String> {
        let plist = self.plist_path();
        let mut status = ScheduleStatus {
            supported: true,
            label: LAUNCHD_LABEL.to_string(),
            plist_path: plist.display().to_string(),
            installed: plist.is_file(),
            ..ScheduleStatus::default()
        };
        if let Some(installed) = read_installed(&plist) {
            status.program_exists = installed.program.as_deref().map(Path::exists);
            status.plist_program = installed.program.map(|p| p.display().to_string());
        }
        if let Some(text) = self.launchctl.print(&self.target())? {
            status.loaded = true;
            status.state = print_field(&text, "state").map(str::to_owned);
            status.last_exit_code =
                print_field(&text, "last exit code").and_then(|v| v.parse().ok());
            status.runs = print_field(&text, "runs").and_then(|v| v.parse().ok());
            status.program = print_field(&text, "program").map(str::to_owned);
            status.interval_secs = print_field(&text, "run interval")
                .and_then(|v| v.split_whitespace().next().and_then(|n| n.parse().ok()));
        }
        Ok(status)
    }
}

#[cfg(windows)]
fn windows_task_name() -> &'static str {
    LAUNCHD_LABEL
}

#[cfg(windows)]
fn run_schtasks(args: &[&str]) -> Result<std::process::Output, String> {
    let mut command = std::process::Command::new("schtasks.exe");
    command.args(args);
    super::platform::no_window(&mut command);
    command
        .output()
        .map_err(|e| format!("cannot run schtasks.exe: {e}"))
}

#[cfg(windows)]
fn windows_task_xml(exe: &Path, interval_secs: u64, user: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let work_dir = exe
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .display()
        .to_string();
    let exe = esc(&exe.display().to_string());
    let work = esc(&work_dir);
    let interval = interval_secs.clamp(60, 86_400);
    let user = esc(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><URI>\{}</URI></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger>
    <CalendarTrigger><StartBoundary>2000-01-01T00:00:00</StartBoundary><Enabled>true</Enabled>
      <ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>
      <Repetition><Interval>PT{}S</Interval><Duration>P1D</Duration><StopAtDurationEnd>false</StopAtDurationEnd></Repetition>
    </CalendarTrigger>
  </Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit></Settings>
  <Actions Context="Author"><Exec><Command>{}</Command><Arguments>--kb-sync --scheduled</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>
</Task>
"#,
        windows_task_name(),
        interval,
        exe,
        work
    )
}

#[cfg(windows)]
fn utf16_xml(xml: &str) -> Vec<u8> {
    std::iter::once(0xFEFFu16)
        .chain(xml.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
fn decode_task_xml(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[b'<', 0]) {
        let bytes = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
        if bytes.len() % 2 != 0 {
            return Err("invalid UTF-16 task XML".into());
        }
        String::from_utf16(
            &bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())
    } else {
        // schtasks /Query can emit the console code page even though the
        // document declares UTF-16. Decode the actual bytes, not that header.
        Ok(super::platform::console_text(bytes))
    }
}

#[cfg(windows)]
fn parse_task_xml(xml: &str) -> Result<InstalledPlist, String> {
    use quick_xml::{events::Event, Reader};
    let mut reader = Reader::from_str(xml);
    let mut installed = InstalledPlist {
        program: None,
        interval_secs: None,
    };
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) if e.local_name().as_ref() == b"Command" => {
                let text = reader.read_text(e.name()).map_err(|e| e.to_string())?;
                installed.program = Some(PathBuf::from(
                    quick_xml::escape::unescape(&text)
                        .map_err(|e| e.to_string())?
                        .as_ref(),
                ));
            }
            Event::Start(e) if e.local_name().as_ref() == b"Interval" => {
                let text = reader.read_text(e.name()).map_err(|e| e.to_string())?;
                installed.interval_secs = parse_interval(&text);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(installed)
}

#[cfg(windows)]
fn parse_interval(text: &str) -> Option<u64> {
    if let Some(days) = text.strip_prefix('P').and_then(|s| s.strip_suffix('D')) {
        return days
            .parse::<u64>()
            .ok()?
            .checked_mul(86_400)
            .filter(|n| *n > 0);
    }
    let mut total = 0u64;
    let mut digits = String::new();
    for c in text.strip_prefix("PT")?.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            let multiplier = match c {
                'H' => 3600,
                'M' => 60,
                'S' => 1,
                _ => return None,
            };
            total = total.checked_add(digits.parse::<u64>().ok()?.checked_mul(multiplier)?)?;
            digits.clear();
        }
    }
    (digits.is_empty() && total > 0).then_some(total)
}

#[cfg(windows)]
fn windows_status() -> Result<ScheduleStatus, String> {
    windows_status_named(windows_task_name())
}

#[cfg(windows)]
fn windows_status_named(name: &str) -> Result<ScheduleStatus, String> {
    let output = run_schtasks(&["/Query", "/TN", name, "/XML"])?;
    let installed = output.status.success();
    let definition = if installed {
        Some(parse_task_xml(&decode_task_xml(&output.stdout)?)?)
    } else {
        None
    };
    let program = definition.as_ref().and_then(|d| d.program.as_ref());
    Ok(ScheduleStatus {
        supported: true,
        label: name.into(),
        plist_path: name.into(),
        installed,
        loaded: installed,
        program: program.map(|p| p.display().to_string()),
        plist_program: program.map(|p| p.display().to_string()),
        program_exists: program.map(|p| p.is_file()),
        interval_secs: definition.and_then(|d| d.interval_secs),
        ..ScheduleStatus::default()
    })
}

#[cfg(windows)]
fn windows_install(config: &KbConfig) -> Result<ScheduleStatus, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate AgentKB.exe: {e}"))?;
    if classify_exe(&exe) != ExeStability::Stable {
        return Err(format!(
            "AgentKB must run from %LOCALAPPDATA%\\Programs\\AgentKB: {}",
            exe.display()
        ));
    }
    windows_register(
        windows_task_name(),
        &windows_task_xml(
            &exe,
            config.interval_secs,
            &super::platform::current_user()?,
        ),
    )?;
    windows_status()
}

#[cfg(windows)]
fn windows_register(name: &str, xml: &str) -> Result<(), String> {
    let temp = tempfile::Builder::new()
        .prefix("agentkb-task-")
        .suffix(".xml")
        .tempfile()
        .map_err(|e| format!("cannot create task definition: {e}"))?
        .into_temp_path();
    // schtasks opens its input exclusively: keep the cleanup guard, but close
    // NamedTempFile's handle before asking the scheduler to read it.
    std::fs::write(&temp, utf16_xml(xml))
        .map_err(|e| format!("cannot write task definition: {e}"))?;
    let output = run_schtasks(&[
        "/Create",
        "/TN",
        name,
        "/XML",
        &temp.to_string_lossy(),
        "/F",
    ])?;
    if !output.status.success() {
        return Err(super::platform::console_text(&output.stderr)
            .trim()
            .to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn windows_uninstall() -> Result<ScheduleStatus, String> {
    windows_uninstall_named(windows_task_name())
}

#[cfg(windows)]
fn windows_uninstall_named(name: &str) -> Result<ScheduleStatus, String> {
    let status = windows_status_named(name)?;
    if !status.installed {
        return Ok(status);
    }
    let output = run_schtasks(&["/Delete", "/TN", name, "/F"])?;
    if !output.status.success() {
        return Err(super::platform::console_text(&output.stderr)
            .trim()
            .to_string());
    }
    windows_status_named(name)
}

#[cfg(windows)]
fn windows_ensure(config: &KbConfig) -> Result<Repair, String> {
    if !config.schedule_enabled {
        return Ok(Repair::NotNeeded);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let stability = classify_exe(&exe);
    if stability != ExeStability::Stable {
        return Ok(Repair::Skipped { stability });
    }
    let status = windows_status()?;
    let installed = status.installed.then(|| InstalledPlist {
        program: status.program.as_ref().map(PathBuf::from),
        interval_secs: status.interval_secs,
    });
    let Some(reason) = definition_difference(installed.as_ref(), &exe, config.interval_secs) else {
        return Ok(Repair::NotNeeded);
    };
    windows_install(config)?;
    Ok(Repair::Repaired {
        from: status.program,
        to: exe.display().to_string(),
        reason: reason.into(),
    })
}

/// `\tkey = value` lines from `launchctl print`.
fn print_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once(" = ")?;
        (k == key).then_some(v.trim())
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    super::fsutil::write_atomic(path, bytes)
}

// ---------------------------------------------------------------------------
// Keeping the task pointed at the app that is actually installed
// ---------------------------------------------------------------------------

/// Whether a running executable is a place a background task may safely point at.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExeStability {
    Stable,
    /// `cargo run`, `tauri dev`, a bare binary: not an installed app.
    NotInApp,
    /// macOS runs an unsigned app opened from Downloads from a random,
    /// read-only copy; the path is gone after the next launch.
    Translocated,
    /// Inside a mounted disk image.
    DiskImage,
}

#[allow(clippy::needless_return)]
pub fn classify_exe(exe: &Path) -> ExeStability {
    #[cfg(windows)]
    {
        let Some(local) = std::env::var_os("LOCALAPPDATA") else {
            return ExeStability::NotInApp;
        };
        let expected = PathBuf::from(local)
            .join("Programs")
            .join("AgentKB")
            .join("AgentKB.exe");
        if same_file(exe, &expected) && exe.is_file() {
            ExeStability::Stable
        } else {
            ExeStability::NotInApp
        }
    }
    #[cfg(not(windows))]
    {
        let text = exe.to_string_lossy();
        if text.contains("/AppTranslocation/") {
            ExeStability::Translocated
        } else if text.starts_with("/Volumes/") {
            ExeStability::DiskImage
        } else if !text.contains(".app/Contents/MacOS/") {
            ExeStability::NotInApp
        } else {
            ExeStability::Stable
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlist {
    pub program: Option<PathBuf>,
    pub interval_secs: Option<u64>,
}

/// What the installed plist says, or `None` when it is missing or unreadable.
pub fn read_installed(plist_path: &Path) -> Option<InstalledPlist> {
    let value = plist::Value::from_file(plist_path).ok()?;
    let dict = value.as_dictionary()?;
    let program = dict
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .and_then(|args| args.first())
        .and_then(plist::Value::as_string)
        .map(PathBuf::from);
    let interval_secs = dict
        .get("StartInterval")
        .and_then(plist::Value::as_unsigned_integer);
    Some(InstalledPlist {
        program,
        interval_secs,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Repair {
    /// Already correct, or the user turned the schedule off.
    NotNeeded,
    /// We are not running from an installed app; leave the task alone.
    Skipped { stability: ExeStability },
    Repaired {
        from: Option<String>,
        to: String,
        reason: String,
    },
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Shared by launchd and Task Scheduler; absence, path and interval all matter.
fn definition_difference(
    installed: Option<&InstalledPlist>,
    exe: &Path,
    interval: u64,
) -> Option<&'static str> {
    match installed {
        None => Some("the task was not installed"),
        Some(i) if i.program.as_deref().map_or(true, |p| !same_file(p, exe)) => {
            Some("the app moved")
        }
        Some(i) if i.interval_secs != Some(interval) => {
            Some("the interval differs from the settings")
        }
        Some(_) => None,
    }
}

impl Scheduler<'_> {
    /// Reinstall the task if it no longer matches this app and the settings.
    /// Safe to call on every start: it only acts when something differs.
    pub fn ensure_current(&self, config: &KbConfig) -> Result<Repair, String> {
        if !config.schedule_enabled {
            return Ok(Repair::NotNeeded);
        }
        let stability = classify_exe(&self.exe);
        if stability != ExeStability::Stable {
            return Ok(Repair::Skipped { stability });
        }
        let installed = read_installed(&self.plist_path());
        let reason =
            match definition_difference(installed.as_ref(), &self.exe, config.interval_secs) {
                Some(reason) => Some(reason),
                None if self.launchctl.print(&self.target())?.is_none() => {
                    Some("the task was not loaded")
                }
                None => None,
            };
        let Some(reason) = reason else {
            return Ok(Repair::NotNeeded);
        };
        let from = installed
            .and_then(|i| i.program)
            .map(|p| p.display().to_string());
        self.install(config)?;
        Ok(Repair::Repaired {
            from,
            to: self.exe.display().to_string(),
            reason: reason.to_string(),
        })
    }
}

pub const META_REPAIRED_AT: &str = "schedule_repaired_at";
pub const META_REPAIRED_FROM: &str = "schedule_repaired_from";
pub const META_REPAIRED_TO: &str = "schedule_repaired_to";
pub const META_REPAIRED_REASON: &str = "schedule_repaired_reason";

/// App-start hook: repair the task for a configured install and leave a note
/// for the home page. Users who never finished setup (no `config.json`) get
/// nothing installed behind their back.
pub fn ensure_current_now() -> Result<Repair, String> {
    if !matches!(KbConfig::state()?, super::config::ConfigState::Valid) {
        return Ok(Repair::NotNeeded);
    }
    let config = KbConfig::load()?;
    #[cfg(windows)]
    let repair = windows_ensure(&config)?;
    #[cfg(not(windows))]
    let repair = with_system_scheduler(|s| s.ensure_current(&config), || Repair::NotNeeded)?;
    let data = super::paths::DataPaths::new(config.data_dir.clone());
    let log = super::log::SyncLog::open(&data.logs());
    match &repair {
        Repair::Repaired { from, to, reason } => {
            log.line(&format!(
                "schedule repaired: {} -> {to} ({reason})",
                from.as_deref().unwrap_or("(none)")
            ));
            let conn = super::db::open(&data.db())?;
            let now = chrono::Utc::now().to_rfc3339();
            super::db::set_meta(&conn, META_REPAIRED_AT, &now)?;
            super::db::set_meta(&conn, META_REPAIRED_FROM, from.as_deref().unwrap_or(""))?;
            super::db::set_meta(&conn, META_REPAIRED_TO, to)?;
            super::db::set_meta(&conn, META_REPAIRED_REASON, reason)?;
        }
        Repair::Skipped { stability } => {
            log.line(&format!(
                "schedule repair skipped: unstable executable path ({stability:?})"
            ));
        }
        Repair::NotNeeded => {}
    }
    Ok(repair)
}

#[cfg(target_os = "macos")]
pub fn system_scheduler() -> Result<(PathBuf, String, PathBuf), String> {
    let home = crate::utils::home_dir().ok_or("cannot determine the home directory")?;
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("cannot run id -u: {e}"))?;
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    if uid.is_empty() {
        return Err("cannot determine the user id".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    Ok((home.join("Library").join("LaunchAgents"), uid, exe))
}

/// Run `f` with the real macOS scheduler; elsewhere report "unsupported".
pub fn with_system_scheduler<T>(
    f: impl FnOnce(&Scheduler<'_>) -> Result<T, String>,
    unsupported: impl FnOnce() -> T,
) -> Result<T, String> {
    #[cfg(target_os = "macos")]
    {
        let _ = unsupported;
        let (dir, uid, exe) = system_scheduler()?;
        f(&Scheduler {
            launch_agents_dir: dir,
            uid,
            exe,
            launchctl: &SystemLaunchctl,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = f;
        Ok(unsupported())
    }
}

// The closures cannot be method paths: `Scheduler<'a>` carries a lifetime that
// the higher-ranked `FnOnce(&Scheduler<'_>)` bound does not let a path name.
#[allow(clippy::redundant_closure_for_method_calls)]
pub fn status_now() -> Result<ScheduleStatus, String> {
    #[cfg(windows)]
    return windows_status();
    #[cfg(not(windows))]
    with_system_scheduler(|s| s.status(), unsupported_status)
}

#[allow(clippy::redundant_closure_for_method_calls)]
pub fn uninstall_now() -> Result<ScheduleStatus, String> {
    #[cfg(windows)]
    return windows_uninstall();
    #[cfg(not(windows))]
    with_system_scheduler(|s| s.uninstall(), unsupported_status)
}

pub fn install_now(config: &KbConfig) -> Result<ScheduleStatus, String> {
    #[cfg(windows)]
    return windows_install(config);
    #[cfg(not(windows))]
    with_system_scheduler(|s| s.install(config), unsupported_status)
}

pub fn unsupported_status() -> ScheduleStatus {
    ScheduleStatus {
        supported: false,
        label: LAUNCHD_LABEL.to_string(),
        ..ScheduleStatus::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeLaunchctl {
        calls: RefCell<Vec<String>>,
        loaded: RefCell<bool>,
    }

    impl Launchctl for FakeLaunchctl {
        fn bootstrap(&self, domain: &str, plist: &Path) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("bootstrap {domain} {}", plist.display()));
            *self.loaded.borrow_mut() = true;
            Ok(())
        }
        fn bootout(&self, target: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("bootout {target}"));
            let was = std::mem::replace(&mut *self.loaded.borrow_mut(), false);
            if was {
                Ok(())
            } else {
                Err("not loaded".into())
            }
        }
        fn print(&self, _target: &str) -> Result<Option<String>, String> {
            Ok(self.loaded.borrow().then(|| {
                "io.github.moqi777.agentkb.sync = {\n\tstate = not running\n\truns = 3\n\tlast exit code = 0\n\trun interval = 600 seconds\n\tprogram = /x/app\n}".to_string()
            }))
        }
    }

    fn config(dir: &Path) -> KbConfig {
        // `KbConfig::default()` resolves the home directory; sandbox it.
        std::env::set_var("CCHV_TEST_HOME", dir);
        KbConfig {
            data_dir: dir.join("AgentKB"),
            interval_secs: 600,
            ..KbConfig::default()
        }
    }

    fn scheduler<'a>(dir: &Path, fake: &'a FakeLaunchctl) -> Scheduler<'a> {
        Scheduler {
            launch_agents_dir: dir.join("LaunchAgents"),
            uid: "501".into(),
            exe: PathBuf::from("/Apps/Agent & KB/app"),
            launchctl: fake,
        }
    }

    #[cfg(unix)]
    #[test]
    fn plist_has_label_args_interval_runatload_and_escapes_paths() {
        let xml = plist_xml(&PlistSpec {
            exe: Path::new("/Apps/Agent & KB/app"),
            interval_secs: 600,
            logs_dir: Path::new("/data/logs"),
            env: &[(TRIGGER_ENV.into(), "schedule".into())],
        });
        assert!(xml.contains("<string>io.github.moqi777.agentkb.sync</string>"));
        assert!(xml.contains("<string>/Apps/Agent &amp; KB/app</string>"));
        assert!(xml.contains("<string>--kb-sync</string>"));
        assert!(xml.contains("<key>StartInterval</key>\n  <integer>600</integer>"));
        assert!(xml.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(xml.contains("/data/logs/launchd.out.log"));
        assert!(xml.contains("<key>AGENTKB_TRIGGER</key>"));
    }

    #[test]
    fn install_is_idempotent_and_status_reports_launchd_fields() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeLaunchctl::default();
        let sched = scheduler(dir.path(), &fake);

        let first = sched.install(&config(dir.path())).unwrap();
        assert!(first.installed && first.loaded);
        assert_eq!(first.runs, Some(3));
        assert_eq!(first.last_exit_code, Some(0));
        assert_eq!(first.interval_secs, Some(600));
        assert_eq!(first.state.as_deref(), Some("not running"));
        assert!(sched
            .plist_path()
            .ends_with("io.github.moqi777.agentkb.sync.plist"));

        let again = sched.install(&config(dir.path())).unwrap();
        assert!(again.loaded);
        let calls = fake.calls.borrow().join("\n");
        assert_eq!(calls.matches("bootstrap").count(), 2);
        assert_eq!(
            calls
                .matches("bootout gui/501/io.github.moqi777.agentkb.sync")
                .count(),
            2
        );
    }

    #[test]
    fn uninstall_unloads_and_removes_the_plist_and_is_repeatable() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeLaunchctl::default();
        let sched = scheduler(dir.path(), &fake);
        sched.install(&config(dir.path())).unwrap();
        let gone = sched.uninstall().unwrap();
        assert!(!gone.installed && !gone.loaded);
        assert!(!sched.plist_path().exists());
        sched.uninstall().unwrap();
    }

    #[test]
    fn no_temp_file_is_left_behind_by_install() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeLaunchctl::default();
        let sched = scheduler(dir.path(), &fake);
        sched.install(&config(dir.path())).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("LaunchAgents"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    fn scheduler_at<'a>(dir: &Path, exe: &str, fake: &'a FakeLaunchctl) -> Scheduler<'a> {
        Scheduler {
            launch_agents_dir: dir.join("LaunchAgents"),
            uid: "501".into(),
            exe: PathBuf::from(exe),
            launchctl: fake,
        }
    }

    const APP_A: &str = "/Applications/AgentKB.app/Contents/MacOS/AgentKB";
    #[cfg(not(windows))]
    const APP_B: &str = "/Users/x/Apps/AgentKB.app/Contents/MacOS/AgentKB";

    #[cfg(not(windows))]
    #[test]
    fn exe_stability_classification() {
        assert_eq!(classify_exe(Path::new(APP_A)), ExeStability::Stable);
        assert_eq!(
            classify_exe(Path::new("/repo/src-tauri/target/debug/agentkb")),
            ExeStability::NotInApp
        );
        assert_eq!(
            classify_exe(Path::new(
                "/private/var/folders/x/AppTranslocation/ABC/d/AgentKB.app/Contents/MacOS/AgentKB"
            )),
            ExeStability::Translocated
        );
        assert_eq!(
            classify_exe(Path::new(
                "/Volumes/AgentKB/AgentKB.app/Contents/MacOS/AgentKB"
            )),
            ExeStability::DiskImage
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_moved_app_gets_the_task_repointed_and_the_old_path_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeLaunchctl::default();
        scheduler_at(dir.path(), APP_A, &fake)
            .install(&config(dir.path()))
            .unwrap();
        let installed =
            read_installed(&scheduler_at(dir.path(), APP_A, &fake).plist_path()).unwrap();
        assert_eq!(installed.program, Some(PathBuf::from(APP_A)));

        // Same app, nothing to do.
        let same = scheduler_at(dir.path(), APP_A, &fake);
        assert_eq!(
            same.ensure_current(&config(dir.path())).unwrap(),
            Repair::NotNeeded
        );

        // The app now runs from somewhere else.
        let moved = scheduler_at(dir.path(), APP_B, &fake);
        let repair = moved.ensure_current(&config(dir.path())).unwrap();
        assert!(
            matches!(&repair, Repair::Repaired { from: Some(f), to, .. }
            if f == APP_A && to == APP_B),
            "{repair:?}"
        );
        let now = read_installed(&moved.plist_path()).unwrap();
        assert_eq!(now.program, Some(PathBuf::from(APP_B)));
        assert_eq!(
            moved.ensure_current(&config(dir.path())).unwrap(),
            Repair::NotNeeded
        );
    }

    #[test]
    fn a_dev_build_never_hijacks_the_installed_task() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeLaunchctl::default();
        scheduler_at(dir.path(), APP_A, &fake)
            .install(&config(dir.path()))
            .unwrap();
        let dev = scheduler_at(dir.path(), "/repo/target/debug/agentkb", &fake);
        assert_eq!(
            dev.ensure_current(&config(dir.path())).unwrap(),
            Repair::Skipped {
                stability: ExeStability::NotInApp
            }
        );
        let still = read_installed(&dev.plist_path()).unwrap();
        assert_eq!(still.program, Some(PathBuf::from(APP_A)));
    }

    #[cfg(not(windows))]
    #[test]
    fn missing_plist_changed_interval_or_unloaded_task_are_repaired_and_disabled_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeLaunchctl::default();
        let sched = scheduler_at(dir.path(), APP_A, &fake);
        let mut cfg = config(dir.path());

        assert!(matches!(
            sched.ensure_current(&cfg).unwrap(),
            Repair::Repaired { from: None, .. }
        ));
        cfg.interval_secs = 300;
        assert!(matches!(
            sched.ensure_current(&cfg).unwrap(),
            Repair::Repaired { .. }
        ));
        assert_eq!(
            read_installed(&sched.plist_path()).unwrap().interval_secs,
            Some(300)
        );

        *fake.loaded.borrow_mut() = false; // launchd forgot the job
        assert!(matches!(
            sched.ensure_current(&cfg).unwrap(),
            Repair::Repaired { .. }
        ));

        cfg.schedule_enabled = false;
        cfg.interval_secs = 120;
        assert_eq!(sched.ensure_current(&cfg).unwrap(), Repair::NotNeeded);
    }

    #[test]
    fn status_flags_a_task_whose_program_no_longer_exists() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeLaunchctl::default();
        let sched = scheduler_at(
            dir.path(),
            "/gone/AgentKB.app/Contents/MacOS/AgentKB",
            &fake,
        );
        sched.install(&config(dir.path())).unwrap();
        let status = sched.status().unwrap();
        assert_eq!(status.program_exists, Some(false));
        assert_eq!(
            status.plist_program.as_deref(),
            Some("/gone/AgentKB.app/Contents/MacOS/AgentKB")
        );
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// Only launched by the temporary scheduled task in the explicit OS check.
    #[test]
    #[ignore = "helper for temporary_task_keeps_background_children_alive"]
    fn scheduled_child_probe() {
        let root = std::env::current_dir().unwrap();
        assert!(root.join("p7-probe-marker").is_file());
        let exe = root.join("worker.exe");
        let data = crate::kb::paths::DataPaths::new(root.clone());
        crate::kb::digest::spawn_background(&exe, &data, &["digest"], &[]).unwrap();
        crate::kb::semantic::build::spawn_background(&exe, &data, &["embed"]).unwrap();
        std::fs::write(root.join("parent.pid"), std::process::id().to_string()).unwrap();
    }

    #[test]
    #[ignore = "registers and deletes a temporary Windows task; run explicitly"]
    fn temporary_task_keeps_background_children_alive() {
        let root = tempfile::Builder::new()
            .prefix("agentkb-用户 with space-")
            .tempdir()
            .unwrap();
        std::fs::write(root.path().join("p7-probe-marker"), "test").unwrap();
        let source = root.path().join("worker.rs");
        std::fs::write(
            &source,
            r#"
fn main() {
    let root = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let kind = std::env::args().nth(1).unwrap();
    std::fs::write(root.join(format!("{kind}.started")), std::process::id().to_string()).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(8));
    std::fs::write(root.join(format!("{kind}.done")), "completed after parent exit").unwrap();
}
"#,
        )
        .unwrap();
        let compiled = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(root.path().join("worker.exe"))
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let name = format!("AgentKB-P7-probe-{}", uuid::Uuid::new_v4());
        struct TaskCleanup(String);
        impl Drop for TaskCleanup {
            fn drop(&mut self) {
                // Cleanup must still work if querying/parsing is what failed.
                let _ = run_schtasks(&["/Delete", "/TN", &self.0, "/F"]);
            }
        }
        let _cleanup = TaskCleanup(name.clone());
        println!("temporary task: {name}");
        assert!(!windows_uninstall_named(&name).unwrap().installed);
        let exe = std::env::current_exe().unwrap();
        let mut xml = windows_task_xml(&exe, 600, &crate::kb::platform::current_user().unwrap());
        xml = xml
            .replace(
                &format!("<URI>\\{LAUNCHD_LABEL}</URI>"),
                &format!("<URI>\\{name}</URI>"),
            )
            .replace(
                "--kb-sync --scheduled",
                "--exact kb::schedule::windows_tests::scheduled_child_probe --ignored --nocapture",
            )
            .replace(
                &format!(
                    "<WorkingDirectory>{}</WorkingDirectory>",
                    xml_escape(&exe.parent().unwrap().display().to_string())
                ),
                &format!(
                    "<WorkingDirectory>{}</WorkingDirectory>",
                    xml_escape(&root.path().display().to_string())
                ),
            );
        xml = xml.replace("<Enabled>true</Enabled>", "<Enabled>false</Enabled>");
        windows_register(&name, &xml).unwrap();
        let status = windows_status_named(&name).unwrap();
        assert_eq!(status.program.as_deref(), exe.to_str());
        assert_eq!(status.program_exists, Some(true));
        assert_eq!(status.interval_secs, Some(600));
        windows_register(&name, &xml.replace("PT600S", "PT300S")).unwrap();
        assert_eq!(
            windows_status_named(&name).unwrap().interval_secs,
            Some(300)
        );
        assert!(run_schtasks(&["/Run", "/TN", &name])
            .unwrap()
            .status
            .success());
        let started = std::time::Instant::now();
        while !root.path().join("parent.pid").is_file() && started.elapsed().as_secs() < 30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let pid: u32 = std::fs::read_to_string(root.path().join("parent.pid"))
            .unwrap()
            .parse()
            .unwrap();
        while crate::kb::platform::process_alive(pid) && started.elapsed().as_secs() < 30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(
            !crate::kb::platform::process_alive(pid),
            "scheduled parent must exit"
        );
        assert!(!root.path().join("digest.done").exists());
        assert!(!root.path().join("embed.done").exists());
        println!("task={name}; parent pid={pid} exited before either worker completed");
        for kind in ["digest", "embed"] {
            while !root.path().join(format!("{kind}.done")).exists()
                && started.elapsed().as_secs() < 40
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let result = std::fs::read_to_string(root.path().join(format!("{kind}.done"))).unwrap();
            println!("{kind}: {result}");
        }
        assert!(!windows_uninstall_named(&name).unwrap().installed);
        assert!(!windows_uninstall_named(&name).unwrap().installed);
        println!("task deleted; repeated uninstall returned installed=false");
    }

    #[test]
    #[serial_test::serial]
    fn installed_location_is_required_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("LOCALAPPDATA");
        std::env::set_var("LOCALAPPDATA", dir.path());
        let exe = dir.path().join("Programs/AgentKB/AgentKB.exe");
        assert_eq!(classify_exe(&exe), ExeStability::NotInApp);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "test").unwrap();
        assert_eq!(classify_exe(&exe), ExeStability::Stable);
        let dev = dir.path().join("AgentKB.exe");
        std::fs::write(&dev, "test").unwrap();
        assert_eq!(classify_exe(&dev), ExeStability::NotInApp);
        match previous {
            Some(v) => std::env::set_var("LOCALAPPDATA", v),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
    }

    #[test]
    fn xml_round_trip_preserves_unicode_paths_and_normalized_intervals() {
        let path = Path::new(r"C:\用户 & space\AgentKB.exe");
        let xml = windows_task_xml(path, 600, r"PC\用户");
        let bytes = utf16_xml(&xml);
        assert!(bytes.starts_with(&[0xFF, 0xFE]));
        let decoded = decode_task_xml(&bytes).unwrap();
        assert_eq!(decoded, xml);
        let installed = parse_task_xml(&decoded).unwrap();
        assert_eq!(installed.program.as_deref(), Some(path));
        assert_eq!(installed.interval_secs, Some(600));
        assert_eq!(parse_interval("PT10M"), Some(600));
        assert_eq!(parse_interval("PT1H2M3S"), Some(3723));
        assert_eq!(parse_interval("P1D"), Some(86_400));
        assert_eq!(parse_interval("PTbad"), None);
        assert!(xml.contains("encoding=\"UTF-16\""));
        assert_eq!(xml.matches("<UserId>PC\\用户</UserId>").count(), 2);
        assert!(xml.contains("<Duration>P1D</Duration>"));
    }

    #[test]
    fn task_repairs_missing_moved_or_changed_interval_definitions() {
        let path = Path::new(r"C:\用户 with space\AgentKB.exe");
        let mut installed = parse_task_xml(&windows_task_xml(path, 600, "test")).unwrap();
        assert_eq!(
            definition_difference(None, path, 600),
            Some("the task was not installed")
        );
        assert_eq!(definition_difference(Some(&installed), path, 600), None);
        assert_eq!(
            definition_difference(Some(&installed), path, 300),
            Some("the interval differs from the settings")
        );
        installed.program = Some(PathBuf::from(r"C:\old\AgentKB.exe"));
        assert_eq!(
            definition_difference(Some(&installed), path, 600),
            Some("the app moved")
        );
    }

    #[test]
    fn task_definition_uses_interactive_logon_and_start_when_available() {
        let xml = windows_task_xml(
            Path::new(r"C:\Users\Example User\AppData\Local\Programs\AgentKB\AgentKB.exe"),
            600,
            r"TEST\用户",
        );
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<StartWhenAvailable>true</StartWhenAvailable>"));
        assert!(xml.contains("--kb-sync --scheduled"));
        assert!(!xml.contains("<LogonType>S4U</LogonType>"));
    }
}
