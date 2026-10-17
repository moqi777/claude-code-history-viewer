//! Layout of the KB data directory and path-safety helpers.

use std::path::{Component, Path, PathBuf};

pub fn is_absolute(path: &Path) -> bool {
    path.is_absolute()
}

/// `~/AgentKB/` (or wherever the config points). Pure path arithmetic.
#[derive(Debug, Clone)]
pub struct DataPaths {
    root: PathBuf,
}

impl DataPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn raw(&self) -> PathBuf {
        self.root.join("raw")
    }
    pub fn raw_codex_sessions(&self) -> PathBuf {
        self.raw().join("codex").join("sessions")
    }
    pub fn raw_codex_archived(&self) -> PathBuf {
        self.raw().join("codex").join("archived_sessions")
    }
    pub fn db(&self) -> PathBuf {
        self.root.join("kb.db")
    }
    pub fn lock(&self) -> PathBuf {
        self.root.join("kb.lock")
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    /// AI-written notes: like `raw/`, a source of truth (they cannot be
    /// regenerated for free); `kb.db` only indexes them.
    pub fn notes(&self) -> PathBuf {
        self.root.join("notes")
    }
    /// One file per session: `notes/<source>/<session_id>.json`. The id comes
    /// from file contents, so it is checked before it names a path.
    pub fn notes_file(&self, source: &str, session_id: &str) -> Result<PathBuf, String> {
        if !matches!(source, "claude" | "codex") || !is_safe_id(session_id) {
            return Err(format!("unsafe note path {source}/{session_id}"));
        }
        Ok(self.notes().join(source).join(format!("{session_id}.json")))
    }
    pub fn digest_lock(&self) -> PathBuf {
        self.root.join("digest.lock")
    }
    /// The AI's working directory. Always excluded from collection.
    pub fn digest_work(&self) -> PathBuf {
        self.root.join("digest-work")
    }

    /// Absolute mirror path for a `mirror_rel` stored in the database.
    pub fn mirror_path(&self, mirror_rel: &str) -> Result<PathBuf, String> {
        safe_join(&self.raw(), mirror_rel)
    }
}

/// Session ids as the CLIs write them: letters, digits, `-` and `_` only.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Join `rel` (always `/`-separated) onto `base`, refusing anything that could
/// leave `base`: absolute paths, `..`, drive prefixes and empty components.
pub fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.is_empty() {
        return Err("empty relative path".to_string());
    }
    let mut out = base.to_path_buf();
    for part in rel.split('/') {
        let mut components = Path::new(part).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(name)), None) if !part.contains('\\') => {
                out.push(name);
                if out.exists() && is_link_or_junction(&out) {
                    return Err(format!("path component is a symlink or junction: {rel}"));
                }
            }
            _ => return Err(format!("unsafe relative path: {rel}")),
        }
    }
    Ok(out)
}

/// Windows junctions are reparse points but are not always reported through
/// `FileType::is_symlink`. Treat every directory/file reparse link like a
/// symlink so a path cannot escape the trusted root.
pub fn is_link_or_junction(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let kind = meta.file_type();
    if kind.is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        kind.is_symlink_dir() || kind.is_symlink_file()
    }
    #[cfg(not(windows))]
    false
}

/// The two mirror roots Codex sessions are served from, canonicalised, for the
/// Codex provider's session-path allow list. Missing directories are skipped;
/// any config problem yields an empty list rather than an error, because the
/// caller is a path validator that must keep working without the KB.
pub fn codex_mirror_roots_canonical() -> Vec<PathBuf> {
    // A test that never sandboxed HOME must not trip `home_dir()`'s guard.
    #[cfg(test)]
    if std::env::var_os("CCHV_TEST_HOME").is_none() {
        return Vec::new();
    }
    let Ok(config) = crate::kb::config::KbConfig::load() else {
        return Vec::new();
    };
    let paths = DataPaths::new(config.data_dir);
    [paths.raw_codex_sessions(), paths.raw_codex_archived()]
        .into_iter()
        .filter_map(|dir| dir.canonicalize().ok())
        .collect()
}

/// Whether `path` lies inside the `AgentKB` mirror (`<data dir>/raw`). The viewer's
/// destructive session actions (delete, rename) refuse such paths: the mirror
/// is the backup, and a rename would also write to the user's Codex state.
pub fn is_inside_mirror(path: &Path) -> bool {
    #[cfg(test)]
    if std::env::var_os("CCHV_TEST_HOME").is_none() {
        return false;
    }
    let Ok(config) = crate::kb::config::KbConfig::load() else {
        return false;
    };
    let raw = DataPaths::new(config.data_dir).raw();
    canonical_even_if_missing(path).starts_with(canonical_even_if_missing(&raw))
}

/// Canonicalise the longest existing prefix and re-append the rest, so a path
/// that does not exist (yet) still compares correctly across symlinks.
fn canonical_even_if_missing(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut probe = path;
    loop {
        if let Ok(real) = probe.canonicalize() {
            return tail.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (probe.parent(), probe.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                probe = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_accepts_nested_relative_paths() {
        let joined = safe_join(Path::new("/raw"), "claude/projects/p/a.jsonl").unwrap();
        assert_eq!(joined, Path::new("/raw/claude/projects/p/a.jsonl"));
    }

    #[test]
    fn safe_join_rejects_escapes() {
        for bad in ["", "../x", "a/../b", "/abs", "a//b", "a\\b", "./a", "a/."] {
            assert!(safe_join(Path::new("/raw"), bad).is_err(), "{bad}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn safe_join_rejects_junction_components() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let target = tmp.path().join("target");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let junction = base.join("linked");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .unwrap();
        if !status.success() {
            println!("skipping safe_join junction test: mklink /J unavailable");
            return;
        }
        assert!(safe_join(&base, "linked/file.jsonl").is_err());
    }

    #[test]
    fn note_paths_refuse_ids_that_could_escape() {
        let data = DataPaths::new("/d");
        assert_eq!(
            data.notes_file("claude", "8e6f1373-1e1d").unwrap(),
            Path::new("/d/notes/claude/8e6f1373-1e1d.json")
        );
        for bad in ["", "../x", "a/b", "a.b", "a b", "a\\b"] {
            assert!(data.notes_file("claude", bad).is_err(), "{bad}");
        }
        assert!(data.notes_file("gemini", "abc").is_err());
    }

    const ID: &str = "019e7371-a6b9-7a50-9825-b7acd6255914";

    /// Sandboxed HOME whose KB config points at `<home>/AgentKB`, with Codex
    /// itself not installed (no `~/.codex`).
    fn kb_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("CCHV_TEST_HOME", home.path());
        std::env::remove_var("CODEX_HOME");
        let config = crate::kb::config::config_path().unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let data = home.path().join("AgentKB");
        std::fs::write(config, serde_json::json!({ "data_dir": data }).to_string()).unwrap();
        home
    }

    fn write_rollout(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("rollout-2026-05-29T11-14-48-{ID}.jsonl"));
        let meta = format!(
            r#"{{"timestamp":"2026-05-29T11:15:04Z","type":"session_meta","payload":{{"id":"{ID}","cwd":"/x"}}}}"#
        );
        let msg = r#"{"timestamp":"2026-05-29T11:15:06Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#;
        std::fs::write(&path, format!("{meta}\n{msg}\n")).unwrap();
        path
    }

    #[test]
    fn mirrored_codex_sessions_open_from_both_mirror_roots() {
        let home = kb_home();
        let data = DataPaths::new(home.path().join("AgentKB"));
        let live = write_rollout(&data.raw_codex_sessions().join("2026/05/29"));
        let archived = write_rollout(&data.raw_codex_archived());

        for path in [live, archived] {
            let messages = crate::providers::codex::load_messages(path.to_str().unwrap()).unwrap();
            assert_ne!(messages.len(), 0, "{}", path.display());
        }
    }

    #[test]
    fn codex_session_paths_outside_the_mirror_and_codex_roots_are_still_rejected() {
        let home = kb_home();
        let data = DataPaths::new(home.path().join("AgentKB"));
        write_rollout(&data.raw_codex_sessions());
        let outside = write_rollout(&home.path().join("elsewhere"));
        let error = crate::providers::codex::load_messages(outside.to_str().unwrap()).unwrap_err();
        assert!(
            error.contains("outside Codex session directories"),
            "{error}"
        );
    }

    #[test]
    fn mirror_roots_that_do_not_exist_yet_are_skipped() {
        let _home = kb_home();
        assert_eq!(codex_mirror_roots_canonical().len(), 0);
    }

    #[test]
    fn mirror_paths_are_recognised_even_before_the_file_exists() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("CCHV_TEST_HOME", home.path());
        let raw = home.path().join("AgentKB/raw/claude/projects/p");
        std::fs::create_dir_all(&raw).unwrap();
        assert!(is_inside_mirror(&raw.join("a.jsonl")));
        assert!(!is_inside_mirror(
            &home.path().join(".claude/projects/p/a.jsonl")
        ));
        assert!(!is_inside_mirror(
            &home.path().join("AgentKB/raw-other/a.jsonl")
        ));
    }
}
