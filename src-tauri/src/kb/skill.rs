//! Installing the `agentkb` skill into Claude Code and Codex.
//!
//! This is the one place the knowledge base writes below `~/.claude` and
//! `~/.codex`, and only inside `skills/agentkb/`, only when the user clicks
//! install (or update), and only a single `SKILL.md`. Uninstalling removes
//! that file and the then-empty folder, nothing else.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use super::config::SourceRoots;
use super::fsutil;
use super::schedule::{classify_exe, ExeStability};
use super::AGENTKB_VERSION;

pub const SKILL_DIR_NAME: &str = "agentkb";
const SKILL_FILE: &str = "SKILL.md";
const MARKER_PREFIX: &str = "<!-- agentkb-skill v1";
/// Template revision; bump when the text changes so installed copies show as stale.
pub const TEMPLATE_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Claude,
    Codex,
}

impl Target {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// `<root>/skills/agentkb` for a tool's config root.
pub fn skill_dir(root: &Path) -> PathBuf {
    root.join("skills").join(SKILL_DIR_NAME)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    NotInstalled,
    Current,
    /// Ours, but written by another app version or pointing at another path.
    Stale,
    /// A folder called `agentkb` that this app did not write.
    Foreign,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillStatus {
    pub target: Target,
    /// Whether the tool's config folder exists at all.
    pub tool_found: bool,
    pub dir: String,
    pub state: State,
    pub installed_version: Option<String>,
    pub installed_exe: Option<String>,
    pub installed_exe_exists: Option<bool>,
    /// Why it is stale, in a word: `version`, `path`, `missing_exe`.
    pub stale_reason: Option<&'static str>,
}

/// Skill text for `target`, pointing at `exe`.
pub fn render(target: Target, exe: &str) -> String {
    let allowed = match target {
        // Pre-approves exactly the two read-only commands; a pipe, `cd` or
        // anything else falls outside the prefix and asks again.
        Target::Claude => {
            format!("allowed-tools: Bash({exe} --kb-search:*), Bash({exe} --kb-show:*)\n")
        }
        Target::Codex => String::new(),
    };
    format!(
        "---\n\
name: agentkb\n\
description: Search the user's past Claude Code and Codex conversations (backed up by AgentKB) to recall how something was solved or decided before. Use when the user says \"之前 / 上次 / 以前 / 那个问题是怎么解决的 / 我们是不是碰到过 / 有没有做过\", or asks to find an earlier session or get its resume command.\n\
{allowed}---\n\
{MARKER_PREFIX} · app {AGENTKB_VERSION} · template {TEMPLATE_VERSION} · exe={exe} -->\n\
\n\
# 查历史会话（AgentKB）\n\
\n\
1. 从用户的话里提 2–4 个具体关键词（报错类名、组件名、中文名词），**不要传整句话**。\n\
2. 搜索：`{exe} --kb-search \"<关键词>\"`\n\
   可选：`--source claude|codex`、`--project <名>`、`--since 30d`、`--limit 5`。\n\
   结果默认是精简的：每个会话一条，它的子会话（sub-agent）命中已并进去，只给 `subagent_hits` 计数；标题最多 60 字。\n\
3. 先看结果里的 `notes`（AgentKB 每天用 AI 整理的笔记：问题、原因、解决办法、结论，带出处）。笔记已经能回答时直接用它回答；再看每个会话的 `title`、`last_activity_at`、`hits[].snippet`。没找到或不相关：换关键词（同义词、中英文各一次）再搜，**一共最多 3 次**，仍没有就如实告诉用户没找到。\n\
4. 对最相关的 1–2 个会话，读上下文再回答：\n\
   `{exe} --kb-show <source>:<session_id> --around <hits[].seq>`\n\
   若该条 hit 带 `subagent_session_id`，上面的 `<session_id>` 要换成它（命中在子会话里）。\n\
   笔记的出处用 `notes[].show`（`--around` 指向笔记依据的第一条消息）。笔记是 AI 写的摘要，可能漏掉或理解错；要据此改代码、跑命令之前，先用 `show` 核对原文。\n\
5. 回答：先给结论，再列出处——会话标题、日期、项目，以及结果里的 `resume.command`。`resume.available` 为 false 时说明原会话已删除，只能用 `--kb-show` 看备份内容。\n\
   引用笔记时要说明“这是 AgentKB 整理的笔记”以及笔记日期（`digested_at`）。\n\
\n\
注意：\n\
- 需要完整字段或把子会话分开列出时才加 `--verbose` / `--include-subagents`。\n\
- 只想要原始消息命中时加 `--no-notes`；想多看几条笔记加 `--notes 5`。\n\
- `has_note` 为 true 而 `hits` 为空，表示这个会话是靠笔记找到的（笔记里有你要的内容，原消息里没有这些词）：看 `notes` 里属于它的那条，要核对原文就用那条笔记的 `show`。\n\
- 结果带 `match` 时：“both”是关键词和语义都命中，最可信；“keyword”是词对上了；“semantic”只表示意思相近，原文不一定有这些词，`hits` 里给的是意思最近的一段，先用 `--kb-show` 读原文确认再引用。`semantic.state` 不是“used”时本次只用了关键词（`semantic.note` 写着原因），照常使用结果即可。\n\
- 调用命令时不要加管道、`cd` 或其他命令拼接（会让预授权的前缀匹配失效，又弹确认）。\n\
- 不要自己 grep 或读取 `~/.claude`、`~/.codex`、`~/AgentKB`，只用上面两个命令。\n\
- 结果是过去的记录，可能已过时（看 `data_as_of` 和会话日期），据此动手前先到代码里核实。\n\
- 搜索本身产生的内容不会被再次索引，不用担心循环。\n\
- 命令不存在时（App 被移动）：`mdfind \"kMDItemCFBundleIdentifier == 'io.github.moqi777.agentkb'\"` 找到 AgentKB.app，把上面路径换成它里面的 `Contents/MacOS/AgentKB`。\n"
    )
}

struct Parsed {
    version: Option<String>,
    template: Option<u32>,
    exe: Option<String>,
}

fn parse_marker(text: &str) -> Option<Parsed> {
    let line = text.lines().find(|l| l.starts_with(MARKER_PREFIX))?;
    let field = |key: &str| {
        line.split(" · ")
            .find_map(|part| part.trim().strip_prefix(key))
            .map(|v| v.trim().trim_end_matches("-->").trim().to_string())
    };
    Some(Parsed {
        version: field("app "),
        template: field("template ").and_then(|v| v.parse().ok()),
        exe: field("exe="),
    })
}

/// How the folder for `target` under `root` looks right now. `current_exe` is
/// the program an up-to-date copy would point at.
pub fn status(target: Target, root: Option<&Path>, current_exe: &Path) -> SkillStatus {
    let Some(root) = root else {
        return SkillStatus {
            target,
            tool_found: false,
            dir: String::new(),
            state: State::NotInstalled,
            installed_version: None,
            installed_exe: None,
            installed_exe_exists: None,
            stale_reason: None,
        };
    };
    let dir = skill_dir(root);
    let mut out = SkillStatus {
        target,
        tool_found: root.is_dir(),
        dir: dir.display().to_string(),
        state: State::NotInstalled,
        installed_version: None,
        installed_exe: None,
        installed_exe_exists: None,
        stale_reason: None,
    };
    let file = dir.join(SKILL_FILE);
    let Ok(text) = std::fs::read_to_string(&file) else {
        if dir.exists() {
            // A folder that is not ours, or a stray file where we expected one.
            out.state = State::Foreign;
        }
        return out;
    };
    let Some(parsed) = parse_marker(&text) else {
        out.state = State::Foreign;
        return out;
    };
    out.installed_version.clone_from(&parsed.version);
    out.installed_exe.clone_from(&parsed.exe);
    let exe_exists = parsed.exe.as_deref().map(|e| Path::new(e).is_file());
    out.installed_exe_exists = exe_exists;
    out.state = State::Current;
    // The text only depends on the template revision and the program path, so a
    // new app version alone does not make the copy stale.
    if parsed.template != Some(TEMPLATE_VERSION) {
        out.state = State::Stale;
        out.stale_reason = Some("version");
    } else if exe_exists == Some(false) {
        out.state = State::Stale;
        out.stale_reason = Some("missing_exe");
    } else if parsed.exe.as_deref().map(Path::new) != Some(current_exe)
        && classify_exe(current_exe) == ExeStability::Stable
    {
        out.state = State::Stale;
        out.stale_reason = Some("path");
    }
    out
}

fn ensure_regular_dir(dir: &Path) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "{} is a symbolic link; not touching it",
                dir.display()
            ));
        }
        if !meta.is_dir() {
            return Err(format!("{} exists and is not a folder", dir.display()));
        }
    }
    Ok(())
}

/// Write (or update) the skill. Only ever the one file inside `skills/agentkb/`.
pub fn install(target: Target, root: &Path, exe: &Path) -> Result<SkillStatus, String> {
    match classify_exe(exe) {
        ExeStability::Stable => {}
        _ => {
            return Err(format!(
                "AgentKB is running from {} — a temporary or unpackaged location. \
                 Move the app to /Applications and open it from there before installing the skill.",
                exe.display()
            ))
        }
    }
    let dir = skill_dir(root);
    ensure_regular_dir(&dir)?;
    let file = dir.join(SKILL_FILE);
    if let Ok(existing) = std::fs::read_to_string(&file) {
        if parse_marker(&existing).is_none() {
            return Err(format!(
                "{} already holds a skill that AgentKB did not write; it was left untouched",
                file.display()
            ));
        }
    } else if dir.exists() && std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
        return Err(format!(
            "{} already exists and is not an AgentKB skill folder; it was left untouched",
            dir.display()
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    fsutil::write_atomic(&file, render(target, &exe.to_string_lossy()).as_bytes())?;
    Ok(status(target, Some(root), exe))
}

/// Remove the skill, but only a copy this app wrote.
pub fn uninstall(target: Target, root: &Path, current_exe: &Path) -> Result<SkillStatus, String> {
    let dir = skill_dir(root);
    ensure_regular_dir(&dir)?;
    let file = dir.join(SKILL_FILE);
    match std::fs::read_to_string(&file) {
        Ok(text) if parse_marker(&text).is_some() => {
            std::fs::remove_file(&file)
                .map_err(|e| format!("cannot remove {}: {e}", file.display()))?;
            // Only if nothing else is in there.
            let _ = std::fs::remove_dir(&dir);
        }
        Ok(_) => {
            return Err(format!(
                "{} was not written by AgentKB; it was left untouched",
                file.display()
            ))
        }
        Err(_) => {}
    }
    Ok(status(target, Some(root), current_exe))
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillTest {
    pub ok: bool,
    pub query: Option<String>,
    pub matched_sessions: Option<u64>,
    pub millis: u64,
    pub message: Option<String>,
}

/// Run `exe --kb-search <word>` for real and report what came back.
pub fn test(exe: &Path, word: &str) -> SkillTest {
    let started = Instant::now();
    let output = std::process::Command::new(exe)
        .args(["--kb-search", word, "--limit", "3"])
        .stdin(std::process::Stdio::null())
        .output();
    let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let fail = |message: String| SkillTest {
        ok: false,
        query: Some(word.to_string()),
        matched_sessions: None,
        millis,
        message: Some(message),
    };
    match output {
        Err(e) => fail(format!("cannot run {}: {e}", exe.display())),
        Ok(out) => {
            let json: Option<serde_json::Value> = serde_json::from_slice(&out.stdout).ok();
            match json {
                Some(v) if v.get("error").is_none() => SkillTest {
                    ok: true,
                    query: Some(word.to_string()),
                    matched_sessions: v
                        .get("matched_sessions")
                        .and_then(serde_json::Value::as_u64),
                    millis,
                    message: None,
                },
                Some(v) => fail(
                    v.get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("search failed")
                        .to_string(),
                ),
                None => fail(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            }
        }
    }
}

/// The skill folders for both tools, resolved from the configured source
/// roots (an install target must exist even if collection is switched off).
pub fn roots_for(config: &super::config::KbConfig) -> SourceRoots {
    let mut forced = config.clone();
    forced.sources.claude.enabled = true;
    forced.sources.codex.enabled = true;
    SourceRoots::resolve(&forced)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/Applications/AgentKB.app/Contents/MacOS/AgentKB";

    fn snapshot(root: &Path) -> Vec<(String, u64)> {
        let mut files = fsutil::walk_files(root);
        files.sort();
        files
    }

    #[test]
    fn the_two_templates_differ_only_by_the_permission_line() {
        let claude = render(Target::Claude, EXE);
        let codex = render(Target::Codex, EXE);
        assert!(claude.contains(&format!(
            "allowed-tools: Bash({EXE} --kb-search:*), Bash({EXE} --kb-show:*)"
        )));
        assert!(!codex.contains("allowed-tools"));
        assert_eq!(
            claude.replace(
                &format!("allowed-tools: Bash({EXE} --kb-search:*), Bash({EXE} --kb-show:*)\n"),
                ""
            ),
            codex
        );
        for text in [&claude, &codex] {
            assert!(text.starts_with("---\nname: agentkb\ndescription: "));
            assert!(text.contains("不要加管道"));
            assert!(text.contains("mdfind"));
            assert!(text.contains(&format!("exe={EXE}")));
        }
    }

    #[test]
    fn install_status_update_and_uninstall_touch_only_the_skill_folder() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".claude");
        std::fs::create_dir_all(root.join("projects/p")).unwrap();
        std::fs::write(root.join("projects/p/s.jsonl"), b"{}\n").unwrap();
        std::fs::write(root.join("settings.json"), b"{}").unwrap();
        let before = snapshot(home.path());
        let exe = Path::new(EXE);

        assert_eq!(
            status(Target::Claude, Some(&root), exe).state,
            State::NotInstalled
        );
        let installed = install(Target::Claude, &root, exe).unwrap();
        assert_eq!(installed.state, State::Current);
        let after = snapshot(home.path());
        let added: Vec<_> = after.iter().filter(|f| !before.contains(f)).collect();
        assert_eq!(added.len(), 1, "{added:?}");
        assert_eq!(added[0].0, ".claude/skills/agentkb/SKILL.md");
        assert!(
            before.iter().all(|f| after.contains(f)),
            "nothing else changed"
        );

        // Installing again is idempotent.
        assert_eq!(
            install(Target::Claude, &root, exe).unwrap().state,
            State::Current
        );

        let removed = uninstall(Target::Claude, &root, exe).unwrap();
        assert_eq!(removed.state, State::NotInstalled);
        // `skills/` itself was created by the install, and stays.
        let leftovers: Vec<_> = snapshot(home.path())
            .into_iter()
            .filter(|f| !before.contains(f))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert!(!skill_dir(&root).exists());
    }

    #[test]
    fn a_stale_copy_is_noticed_and_updating_fixes_it() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".codex");
        let old_exe = Path::new("/Applications/Old.app/Contents/MacOS/AgentKB");
        install(Target::Codex, &root, old_exe).unwrap();
        // The old path does not exist on this machine.
        let s = status(Target::Codex, Some(&root), Path::new(EXE));
        assert_eq!(s.state, State::Stale);
        assert_eq!(s.stale_reason, Some("missing_exe"));
        assert_eq!(s.installed_exe.as_deref(), old_exe.to_str());

        // A copy written by another app version is still current: only the
        // template revision and the program path matter.
        install(Target::Codex, &root, Path::new(EXE)).unwrap();
        let file = skill_dir(&root).join(SKILL_FILE);
        let text = std::fs::read_to_string(&file)
            .unwrap()
            .replace(&format!("app {AGENTKB_VERSION}"), "app 0.0.1");
        std::fs::write(&file, text).unwrap();
        let s = status(Target::Codex, Some(&root), Path::new(EXE));
        assert_eq!((s.state, s.stale_reason), (State::Current, None));

        // A copy from an older template is stale even if the path is fine.
        let text = std::fs::read_to_string(&file)
            .unwrap()
            .replace(&format!("template {TEMPLATE_VERSION}"), "template 0");
        std::fs::write(&file, text).unwrap();
        let s = status(Target::Codex, Some(&root), Path::new(EXE));
        assert_eq!((s.state, s.stale_reason), (State::Stale, Some("version")));

        install(Target::Codex, &root, Path::new(EXE)).unwrap();
        assert_eq!(
            status(Target::Codex, Some(&root), Path::new(EXE)).state,
            State::Current
        );
    }

    #[test]
    fn a_skill_that_is_not_ours_is_never_overwritten_or_removed() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".claude");
        let dir = skill_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SKILL_FILE), "---\nname: agentkb\n---\nmine\n").unwrap();
        let exe = Path::new(EXE);
        assert_eq!(
            status(Target::Claude, Some(&root), exe).state,
            State::Foreign
        );
        assert!(install(Target::Claude, &root, exe).is_err());
        assert!(uninstall(Target::Claude, &root, exe).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join(SKILL_FILE)).unwrap(),
            "---\nname: agentkb\n---\nmine\n"
        );
        // A folder with other content and no SKILL.md is not ours either.
        std::fs::remove_file(dir.join(SKILL_FILE)).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        assert!(install(Target::Claude, &root, exe).is_err());
        assert!(dir.join("notes.txt").exists());
    }

    #[test]
    fn uninstall_leaves_extra_files_in_the_folder_alone() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".claude");
        install(Target::Claude, &root, Path::new(EXE)).unwrap();
        let extra = skill_dir(&root).join("keep.txt");
        std::fs::write(&extra, "keep").unwrap();
        uninstall(Target::Claude, &root, Path::new(EXE)).unwrap();
        assert!(extra.exists(), "only SKILL.md is ours to remove");
        assert!(!skill_dir(&root).join(SKILL_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_skill_folder_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".claude");
        let elsewhere = home.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, skill_dir(&root)).unwrap();
        assert!(install(Target::Claude, &root, Path::new(EXE)).is_err());
        assert!(uninstall(Target::Claude, &root, Path::new(EXE)).is_err());
        assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
    }

    #[test]
    fn a_temporary_location_cannot_be_written_into_a_skill() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".claude");
        let err = install(
            Target::Claude,
            &root,
            Path::new("/Users/me/dev/target/debug/claude-code-history-viewer"),
        )
        .unwrap_err();
        assert!(err.contains("/Applications"));
        assert!(!root.exists());
    }

    #[test]
    fn missing_tool_folder_is_reported() {
        let s = status(Target::Codex, None, Path::new(EXE));
        assert!(!s.tool_found && s.state == State::NotInstalled);
        let home = tempfile::tempdir().unwrap();
        let s = status(
            Target::Codex,
            Some(&home.path().join(".codex")),
            Path::new(EXE),
        );
        assert!(!s.tool_found);
    }
}
