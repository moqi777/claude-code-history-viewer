//! Finding the session files to collect.
//!
//! Scope: Claude `projects/<proj>/<id>.jsonl` and
//! `projects/<proj>/<id>/subagents/*.jsonl` (everything else under `projects/`,
//! e.g. `memory/`, is not a session); Codex `rollout-*.jsonl[.zst]` under
//! `sessions/` and `archived_sessions/`. Symlinks are never followed.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use walkdir::WalkDir;

use super::config::SourceRoots;
use super::Source;
use crate::providers::codex as codex_provider;

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub source: Source,
    pub path: PathBuf,
    /// `path` as stored in the database.
    pub path_str: String,
    /// Mirror location relative to `raw/`, always `/`-separated.
    pub mirror_rel: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub is_compressed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanError {
    pub path: String,
    pub stage: &'static str,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Discovery {
    pub files: Vec<SourceFile>,
    /// Directories that were listed completely. Only files below one of these
    /// may be declared deleted: an unreadable or unmounted directory must not
    /// turn its whole history into "source deleted".
    pub complete_roots: Vec<PathBuf>,
    pub errors: Vec<ScanError>,
}

pub fn discover(roots: &SourceRoots, exclude_prefixes: &[String]) -> Discovery {
    let mut out = Discovery::default();
    if let Some(claude) = &roots.claude {
        scan_claude(claude, exclude_prefixes, &mut out);
    }
    if let Some(codex) = &roots.codex {
        scan_codex_dir(codex, "sessions", exclude_prefixes, &mut out);
        scan_codex_dir(codex, "archived_sessions", exclude_prefixes, &mut out);
    }
    out
}

pub fn is_excluded(path: &Path, exclude_prefixes: &[String]) -> bool {
    let text = path.to_string_lossy();
    exclude_prefixes
        .iter()
        .filter(|p| !p.is_empty())
        .any(|prefix| path.starts_with(Path::new(prefix)) || text.starts_with(prefix.as_str()))
}

fn mtime_ns_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

pub fn mtime_ns(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), mtime_ns_of(&meta)))
}

/// `Some(true)` when `dir` was listed; a missing directory under an existing
/// root counts as an (empty) complete listing.
fn start_walk(dir: &Path, root: &Path, out: &mut Discovery) -> bool {
    match std::fs::read_dir(dir) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && root.is_dir() => {
            out.complete_roots.push(dir.to_path_buf());
            false
        }
        Err(e) => {
            out.errors.push(ScanError {
                path: dir.display().to_string(),
                stage: "scan",
                message: e.to_string(),
            });
            false
        }
    }
}

fn scan_claude(root: &Path, exclude: &[String], out: &mut Discovery) {
    let projects = root.join("projects");
    if !start_walk(&projects, root, out) {
        return;
    }
    let mut complete = true;
    for entry in WalkDir::new(&projects).min_depth(1).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                complete = false;
                out.errors.push(ScanError {
                    path: e
                        .path()
                        .map_or_else(String::new, |p| p.display().to_string()),
                    stage: "scan",
                    message: e.to_string(),
                });
                continue;
            }
        };
        if super::paths::is_link_or_junction(entry.path())
            || !entry.file_type().is_file()
            || entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl")
        {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(&projects) else {
            continue;
        };
        let parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
        let is_session = matches!(parts.as_slice(), [_, _] | [_, _, "subagents", _]);
        if !is_session || is_excluded(entry.path(), exclude) {
            continue;
        }
        push_file(
            out,
            Source::Claude,
            entry.path(),
            &format!("claude/projects/{}", parts.join("/")),
            false,
        );
    }
    if complete {
        out.complete_roots.push(projects);
    }
}

fn scan_codex_dir(root: &Path, sub: &str, exclude: &[String], out: &mut Discovery) {
    let dir = root.join(sub);
    if !start_walk(&dir, root, out) {
        return;
    }
    let mut complete = true;
    for entry in WalkDir::new(&dir).min_depth(1).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                complete = false;
                out.errors.push(ScanError {
                    path: e
                        .path()
                        .map_or_else(String::new, |p| p.display().to_string()),
                    stage: "scan",
                    message: e.to_string(),
                });
                continue;
            }
        };
        if super::paths::is_link_or_junction(entry.path())
            || !entry.file_type().is_file()
            || !codex_provider::is_discoverable_rollout(entry.path())
            || is_excluded(entry.path(), exclude)
        {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(&dir) else {
            continue;
        };
        let rel: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
        let compressed = entry
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| Path::new(n).extension().is_some_and(|e| e == "zst"));
        push_file(
            out,
            Source::Codex,
            entry.path(),
            &format!("codex/{sub}/{}", rel.join("/")),
            compressed,
        );
    }
    if complete {
        out.complete_roots.push(dir);
    }
}

fn push_file(out: &mut Discovery, source: Source, path: &Path, mirror_rel: &str, compressed: bool) {
    match std::fs::metadata(path) {
        Ok(meta) => out.files.push(SourceFile {
            source,
            path: path.to_path_buf(),
            path_str: path.to_string_lossy().into_owned(),
            mirror_rel: mirror_rel.to_string(),
            size: meta.len(),
            mtime_ns: mtime_ns_of(&meta),
            is_compressed: compressed,
        }),
        Err(e) => out.errors.push(ScanError {
            path: path.display().to_string(),
            stage: "scan",
            message: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"{}\n").unwrap();
    }

    #[test]
    fn claude_sessions_and_subagents_are_found_and_other_files_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".claude");
        let p = root.join("projects/-Users-me-proj");
        touch(&p.join("aaa.jsonl"));
        touch(&p.join("aaa/subagents/agent-1.jsonl"));
        touch(&p.join("memory/notes.jsonl"));
        touch(&p.join("aaa/tool-results/x.jsonl"));
        touch(&p.join("sessions-index.json"));
        let found = discover(
            &SourceRoots {
                claude: Some(root),
                codex: None,
            },
            &[],
        );
        let mut rels: Vec<_> = found.files.iter().map(|f| f.mirror_rel.clone()).collect();
        rels.sort();
        assert_eq!(
            rels,
            vec![
                "claude/projects/-Users-me-proj/aaa.jsonl",
                "claude/projects/-Users-me-proj/aaa/subagents/agent-1.jsonl",
            ]
        );
        assert_eq!(found.complete_roots.len(), 1);
    }

    #[test]
    fn codex_sessions_archived_and_compressed_twins() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".codex");
        let id = "019e7371-a6b9-7a50-9825-b7acd6255914";
        touch(&root.join(format!(
            "sessions/2026/05/29/rollout-2026-05-29T11-14-48-{id}.jsonl"
        )));
        touch(&root.join(format!(
            "archived_sessions/rollout-2026-05-01T00-00-00-{id}.jsonl.zst"
        )));
        // zst whose plain twin exists is skipped (the plain file is current).
        touch(&root.join(format!(
            "sessions/2026/05/29/rollout-2026-05-29T11-14-48-{id}.jsonl.zst"
        )));
        touch(&root.join("sessions/2026/05/29/notes.txt"));
        let found = discover(
            &SourceRoots {
                claude: None,
                codex: Some(root),
            },
            &[],
        );
        let mut rels: Vec<_> = found
            .files
            .iter()
            .map(|f| (f.mirror_rel.clone(), f.is_compressed))
            .collect();
        rels.sort();
        assert_eq!(rels.len(), 2);
        assert!(rels[0].0.starts_with("codex/archived_sessions/") && rels[0].1);
        assert!(rels[1].0.starts_with("codex/sessions/2026/05/29/") && !rels[1].1);
    }

    #[test]
    fn exclude_prefix_and_symlinks_are_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".claude");
        let p = root.join("projects/keep");
        let skip = root.join("projects/skip");
        touch(&p.join("a.jsonl"));
        touch(&skip.join("b.jsonl"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(p.join("a.jsonl"), p.join("link.jsonl")).unwrap();
        let found = discover(
            &SourceRoots {
                claude: Some(root),
                codex: None,
            },
            &[skip.to_string_lossy().into_owned()],
        );
        let rels: Vec<_> = found.files.iter().map(|f| f.mirror_rel.as_str()).collect();
        assert_eq!(rels, vec!["claude/projects/keep/a.jsonl"]);
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_skipped_like_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".claude");
        let target = dir.path().join("outside");
        fs::create_dir_all(target.join("nested")).unwrap();
        touch(&target.join("nested/outside.jsonl"));
        let junction = root.join("projects/junction");
        fs::create_dir_all(junction.parent().unwrap()).unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .unwrap();
        if !status.success() {
            println!("skipping junction test: mklink /J unavailable");
            return;
        }
        let found = discover(
            &SourceRoots {
                claude: Some(root),
                codex: None,
            },
            &[],
        );
        assert!(
            found.files.is_empty(),
            "junction contents must not be collected"
        );
    }

    #[test]
    fn missing_projects_dir_under_existing_root_is_a_complete_empty_listing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".claude");
        fs::create_dir_all(&root).unwrap();
        let found = discover(
            &SourceRoots {
                claude: Some(root),
                codex: None,
            },
            &[],
        );
        assert!(found.files.is_empty());
        assert_eq!(found.complete_roots.len(), 1);
        assert!(found.errors.is_empty());
    }

    #[test]
    fn nonexistent_root_is_neither_complete_nor_an_error_for_deletion_purposes() {
        let dir = tempfile::tempdir().unwrap();
        let found = discover(
            &SourceRoots {
                claude: Some(dir.path().join("gone")),
                codex: None,
            },
            &[],
        );
        assert!(
            found.complete_roots.is_empty(),
            "must not declare anything deleted"
        );
        assert_eq!(found.errors.len(), 1);
    }
}
