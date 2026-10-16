//! What to do with each file of an import when this machine already holds
//! data. Planning only reads; applying never deletes or overwrites anything
//! the user already has: a file that differs is stored next to the local one.
//!
//! Content is compared up to the length the local database vouches for
//! (`mirror_offset`): bytes past it are leftovers of an interrupted run, not
//! data.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::db::{FileRow, ImportedFile};
use super::notes::NotesFile;
use super::paths::DataPaths;
use super::transfer::{hex, TResult, TransferError};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// The local copy is the start of the incoming one, but its source file
    /// is still on this machine, so its cursor must not move.
    LiveSource,
    /// Neither is the start of the other.
    Diverged,
    /// A file with no database row, or a compressed one, that differs.
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    /// Same, or the local one is longer: keep what is here.
    Skip,
    /// Not here yet.
    Add,
    /// The local one is the start of the incoming one and has no source
    /// here: append the rest.
    Extend { from: u64 },
    /// Keep the local file and store the incoming one beside it.
    KeepBoth { why: Why },
}

#[derive(Debug, Clone, Serialize)]
pub struct FilePlan {
    /// Path under `raw/`.
    pub rel: String,
    pub action: Action,
    pub len: u64,
    /// Where the incoming bytes end up (`rel`, or the renamed copy).
    pub target_rel: String,
    /// Index into the incoming rows, if the file has one.
    pub row: Option<usize>,
    /// Whether the incoming row is inserted.
    pub insert_row: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoteAction {
    Add,
    Skip,
    /// Both differ; the incoming one is newer. The local one is kept in
    /// `notes/.conflict-<id>/`.
    IncomingWins,
    /// Both differ; the local one is newer or equal. The incoming one goes
    /// to `notes/.conflict-<id>/`.
    LocalWins,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotePlan {
    /// Path under `notes/`.
    pub rel: String,
    pub action: NoteAction,
    pub entries_in: u64,
    pub entries_local: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Summary {
    /// Sessions (database rows) the import adds.
    pub sessions_added: u64,
    pub files_added: u64,
    pub identical: u64,
    pub extended: u64,
    /// Local copy is the start but its source is still here (rule F).
    pub kept_both_live: u64,
    /// Neither is the start of the other (rule G).
    pub kept_both_diverged: u64,
    /// Files without a database row (or compressed ones) that differ.
    pub kept_both_other: u64,
    /// Incoming rows left out because their source path is already used.
    pub row_collisions: u64,
    pub notes_added: u64,
    pub notes_identical: u64,
    pub notes_conflicts: u64,
    pub expected_sessions: u64,
    pub expected_notes: u64,
    pub expected_raw_files: u64,
    pub expected_note_files: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub files: Vec<FilePlan>,
    pub notes: Vec<NotePlan>,
    pub summary: Summary,
}

/// What the local side looks like before the import.
#[derive(Debug, Clone, Default)]
pub struct LocalCounts {
    pub sessions: u64,
    pub notes: u64,
    pub raw_files: u64,
    pub note_files: u64,
}

fn io_err(what: &str, e: impl std::fmt::Display) -> TransferError {
    TransferError::io(format!("{what}: {e}"))
}

fn same_prefix(a: &Path, b: &Path, len: u64) -> TResult<bool> {
    let mut fa = File::open(a).map_err(|e| io_err(&format!("cannot open {}", a.display()), e))?;
    let mut fb = File::open(b).map_err(|e| io_err(&format!("cannot open {}", b.display()), e))?;
    let (mut ba, mut bb) = (vec![0u8; 256 * 1024], vec![0u8; 256 * 1024]);
    let mut left = len;
    while left > 0 {
        let want = usize::try_from(left.min(ba.len() as u64)).unwrap_or(ba.len());
        fa.read_exact(&mut ba[..want])
            .map_err(|e| io_err(&format!("cannot read {}", a.display()), e))?;
        fb.read_exact(&mut bb[..want])
            .map_err(|e| io_err(&format!("cannot read {}", b.display()), e))?;
        if ba[..want] != bb[..want] {
            return Ok(false);
        }
        left -= want as u64;
    }
    Ok(true)
}

/// A name for the incoming copy that cannot collide with a session file:
/// `dir/stem.imported-<tag>.jsonl`.
pub fn imported_name(path: &str, tag: &str) -> String {
    let (dir, file) = match path.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, path),
    };
    let ext_len = if file.ends_with(".jsonl.zst") {
        ".jsonl.zst".len()
    } else {
        file.rfind('.').map_or(0, |i| file.len() - i)
    };
    let (stem, ext) = file.split_at(file.len() - ext_len);
    let name = format!("{stem}.imported-{tag}{ext}");
    dir.map_or(name.clone(), |d| format!("{d}/{name}"))
}

/// Eight hex characters derived from the export id.
pub fn tag_of(export_id: &str) -> String {
    hex(&Sha256::digest(export_id.as_bytes()))[..8].to_string()
}

fn classify(
    local: &Path,
    row: Option<&FileRow>,
    incoming: &Path,
    incoming_len: u64,
    rel: &str,
) -> TResult<Action> {
    if !local.is_file() {
        return Ok(Action::Add);
    }
    let size = std::fs::metadata(local)
        .map_err(|e| io_err(&format!("cannot read {}", local.display()), e))?
        .len();
    let compressed = std::path::Path::new(rel)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zst"))
        || row.is_some_and(|r| r.is_compressed);
    if compressed {
        let same = size == incoming_len && same_prefix(local, incoming, size)?;
        return Ok(if same {
            Action::Skip
        } else {
            Action::KeepBoth { why: Why::Other }
        });
    }
    // What the local database vouches for; the rest is an orphan tail.
    let effective = match row {
        Some(r) => size.min(u64::try_from(r.mirror_offset).unwrap_or(0)),
        None => size,
    };
    let common = effective.min(incoming_len);
    if !same_prefix(local, incoming, common)? {
        return Ok(Action::KeepBoth {
            why: if row.is_some() {
                Why::Diverged
            } else {
                Why::Other
            },
        });
    }
    if effective >= incoming_len {
        return Ok(Action::Skip);
    }
    // The local copy is the start of the incoming one.
    match row {
        Some(r) => {
            let source_here =
                r.origin.is_none() && !r.deleted && Path::new(&r.source_path).exists();
            if source_here {
                Ok(Action::KeepBoth {
                    why: Why::LiveSource,
                })
            } else {
                Ok(Action::Extend { from: effective })
            }
        }
        None => Ok(Action::KeepBoth { why: Why::Other }),
    }
}

fn note_entries(bytes: &[u8]) -> Option<(u64, String)> {
    let file: NotesFile = serde_json::from_slice(bytes).ok()?;
    Some((file.entries.len() as u64, file.digested_at))
}

/// Decide every file and note. `incoming` rows have their paths converted
/// already. Reads the staged files and the local ones; changes nothing.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    data: &DataPaths,
    staged: &Path,
    staged_files: &[(String, u64)],
    incoming: &[ImportedFile],
    local_rows: &[FileRow],
    local: &LocalCounts,
    export_id: &str,
) -> TResult<Plan> {
    let tag = tag_of(export_id);
    let local_by_rel: HashMap<&str, &FileRow> = local_rows
        .iter()
        .map(|r| (r.mirror_rel.as_str(), r))
        .collect();
    let mut used_paths: HashSet<String> =
        local_rows.iter().map(|r| r.source_path.clone()).collect();
    let incoming_by_rel: HashMap<&str, usize> = incoming
        .iter()
        .enumerate()
        .map(|(i, r)| (r.mirror_rel.as_str(), i))
        .collect();

    let mut out = Plan::default();
    let mut raw_added = 0u64;
    for (path, len) in staged_files {
        let Some(rel) = path.strip_prefix("raw/") else {
            continue;
        };
        let row_index = incoming_by_rel.get(rel).copied();
        let local_row = local_by_rel.get(rel).copied();
        let action = classify(
            &data.raw().join(rel),
            local_row,
            &staged.join(path),
            *len,
            rel,
        )?;
        let mut target_rel = rel.to_string();
        let mut insert_row = false;
        match action {
            Action::Skip => {
                out.summary.identical += 1;
                // A file left by an interrupted import has no row yet.
                insert_row = local_row.is_none() && row_index.is_some();
            }
            Action::Add => {
                out.summary.files_added += 1;
                raw_added += 1;
                insert_row = local_row.is_none() && row_index.is_some();
            }
            Action::Extend { .. } => out.summary.extended += 1,
            Action::KeepBoth { why } => {
                out.summary.files_added += 1;
                raw_added += 1;
                match why {
                    Why::LiveSource => out.summary.kept_both_live += 1,
                    Why::Diverged => out.summary.kept_both_diverged += 1,
                    Why::Other => out.summary.kept_both_other += 1,
                }
                target_rel = imported_name(rel, &tag);
                insert_row = row_index.is_some();
            }
        }
        if insert_row {
            if let Some(i) = row_index {
                let wanted = if target_rel == rel {
                    incoming[i].source_path.clone()
                } else {
                    imported_name(&incoming[i].source_path, &tag)
                };
                if used_paths.insert(wanted) {
                    out.summary.sessions_added += 1;
                } else {
                    out.summary.row_collisions += 1;
                    insert_row = false;
                }
            }
        }
        out.files.push(FilePlan {
            rel: rel.to_string(),
            action,
            len: *len,
            target_rel,
            row: row_index,
            insert_row,
        });
    }

    // Notes.
    let mut notes_delta: i64 = 0;
    let mut note_files_added = 0u64;
    for (path, _) in staged_files {
        let Some(rel) = path.strip_prefix("notes/") else {
            continue;
        };
        let incoming_bytes = std::fs::read(staged.join(path))
            .map_err(|e| io_err(&format!("cannot read {path}"), e))?;
        let (entries_in, at_in) = note_entries(&incoming_bytes).unwrap_or((0, String::new()));
        let local_path = data.notes().join(rel);
        let (action, entries_local) = match std::fs::read(&local_path) {
            Err(_) => {
                if !super::transfer::is_conflict_path(rel) {
                    note_files_added += 1;
                }
                notes_delta += i64::try_from(entries_in).unwrap_or(0);
                out.summary.notes_added += 1;
                (NoteAction::Add, 0)
            }
            Ok(bytes) if bytes == incoming_bytes => {
                out.summary.notes_identical += 1;
                (NoteAction::Skip, entries_in)
            }
            Ok(bytes) => {
                let (entries_local, at_local) = note_entries(&bytes).unwrap_or((0, String::new()));
                out.summary.notes_conflicts += 1;
                if at_in > at_local {
                    notes_delta += i64::try_from(entries_in).unwrap_or(0)
                        - i64::try_from(entries_local).unwrap_or(0);
                    (NoteAction::IncomingWins, entries_local)
                } else {
                    (NoteAction::LocalWins, entries_local)
                }
            }
        };
        out.notes.push(NotePlan {
            rel: rel.to_string(),
            action,
            entries_in,
            entries_local,
        });
    }

    out.summary.expected_sessions = local.sessions + out.summary.sessions_added;
    out.summary.expected_notes =
        u64::try_from(i64::try_from(local.notes).unwrap_or(0) + notes_delta).unwrap_or(0);
    out.summary.expected_raw_files = local.raw_files + raw_added;
    out.summary.expected_note_files = local.note_files + note_files_added;
    Ok(out)
}

/// Do the file work: copy, append, store beside. Each step is atomic and
/// repeatable, and none removes or rewrites a byte the user already had.
pub fn apply_files(
    plan: &Plan,
    data: &DataPaths,
    staged: &Path,
    cancel: &super::transfer::Cancel,
    on_file: &mut dyn FnMut(u64),
) -> TResult<()> {
    for item in &plan.files {
        cancel.check()?;
        let from = staged.join("raw").join(&item.rel);
        match item.action {
            Action::Skip => {}
            Action::Add => {
                super::fsutil::copy_atomic(&from, &data.raw().join(&item.rel))?;
            }
            Action::KeepBoth { .. } => {
                super::fsutil::copy_atomic(&from, &data.raw().join(&item.target_rel))?;
            }
            Action::Extend { from: at } => {
                // Cuts any orphan tail at `at` first, then appends.
                super::fsutil::append_tail(&from, &data.raw().join(&item.rel), at)?;
            }
        }
        on_file(item.len);
    }
    Ok(())
}

/// Write the notes. A conflicting pair keeps both: the loser is copied to
/// `notes/.conflict-<tag>/<path>` before anything is replaced.
pub fn apply_notes(plan: &Plan, data: &DataPaths, staged: &Path, export_id: &str) -> TResult<()> {
    let conflicts = data
        .notes()
        .join(format!(".conflict-{}", tag_of(export_id)));
    for note in &plan.notes {
        let incoming = staged.join("notes").join(&note.rel);
        let local = data.notes().join(&note.rel);
        match note.action {
            NoteAction::Skip => {}
            NoteAction::Add => {
                super::fsutil::copy_atomic(&incoming, &local)?;
            }
            NoteAction::LocalWins => {
                super::fsutil::copy_atomic(&incoming, &conflicts.join(&note.rel))?;
            }
            NoteAction::IncomingWins => {
                super::fsutil::copy_atomic(&local, &conflicts.join(&note.rel))?;
                super::fsutil::copy_atomic(&incoming, &local)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stored_copy_gets_a_name_that_is_not_a_session_file() {
        assert_eq!(
            imported_name("claude/projects/p/s.jsonl", "ab12cd34"),
            "claude/projects/p/s.imported-ab12cd34.jsonl"
        );
        assert_eq!(
            imported_name("codex/sessions/2026/x.jsonl.zst", "ab12cd34"),
            "codex/sessions/2026/x.imported-ab12cd34.jsonl.zst"
        );
        assert_eq!(imported_name("noext", "t"), "noext.imported-t");
        assert_eq!(tag_of("x").len(), 8);
        assert_eq!(tag_of("x"), tag_of("x"));
        assert_ne!(tag_of("x"), tag_of("y"));
    }

    fn files(
        dir: &Path,
        local: &[u8],
        incoming: &[u8],
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let a = dir.join("local");
        let b = dir.join("incoming");
        std::fs::write(&a, local).unwrap();
        std::fs::write(&b, incoming).unwrap();
        (a, b)
    }

    fn row(offset: i64, origin: Option<&str>, deleted: bool, source: &str) -> FileRow {
        FileRow {
            id: 1,
            source: super::super::Source::Claude,
            source_path: source.into(),
            mirror_rel: "claude/x.jsonl".into(),
            src_size: 0,
            src_mtime_ns: 0,
            head_hash: vec![],
            head_len: 0,
            mirror_offset: offset,
            is_compressed: false,
            deleted,
            origin: origin.map(str::to_string),
        }
    }

    #[test]
    fn every_case_of_one_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let gone = "/nonexistent/source.jsonl";
        let here = dir.path().join("source.jsonl");
        std::fs::write(&here, b"x").unwrap();
        let here = here.to_str().unwrap();
        let cls = |l: &[u8], i: &[u8], r: Option<&FileRow>| {
            let (a, b) = files(dir.path(), l, i);
            classify(&a, r, &b, i.len() as u64, "claude/x.jsonl").unwrap()
        };
        // only the incoming one has it
        assert_eq!(
            classify(
                &dir.path().join("missing"),
                None,
                &dir.path().join("missing"),
                1,
                "a"
            )
            .unwrap(),
            Action::Add
        );
        // same
        assert_eq!(
            cls(b"abc\n", b"abc\n", Some(&row(4, None, false, gone))),
            Action::Skip
        );
        // incoming is the start of the local one
        assert_eq!(
            cls(b"abc\ndef\n", b"abc\n", Some(&row(8, None, false, gone))),
            Action::Skip
        );
        // local is the start, its source is gone
        assert_eq!(
            cls(b"abc\n", b"abc\ndef\n", Some(&row(4, None, true, gone))),
            Action::Extend { from: 4 }
        );
        assert_eq!(
            cls(
                b"abc\n",
                b"abc\ndef\n",
                Some(&row(4, Some("A"), false, gone))
            ),
            Action::Extend { from: 4 }
        );
        // local is the start but its source is still here
        assert_eq!(
            cls(b"abc\n", b"abc\ndef\n", Some(&row(4, None, false, here))),
            Action::KeepBoth {
                why: Why::LiveSource
            }
        );
        // neither is the start of the other
        assert_eq!(
            cls(b"abc\nXX\n", b"abc\nYY\n", Some(&row(7, None, false, gone))),
            Action::KeepBoth { why: Why::Diverged }
        );
        // an orphan file that differs
        assert_eq!(
            cls(b"abc\n", b"abc\ndef\n", None),
            Action::KeepBoth { why: Why::Other }
        );
        // bytes past the cursor are an orphan tail, not data: the local file
        // counts as "abc\n" and the full incoming one extends it
        assert_eq!(
            cls(b"abc\nJUNK", b"abc\ndef\n", Some(&row(4, None, true, gone))),
            Action::Extend { from: 4 }
        );
    }

    #[test]
    fn compressed_files_are_only_ever_identical_or_stored_beside() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = files(dir.path(), b"zz", b"zz");
        assert_eq!(
            classify(&a, None, &b, 2, "codex/x.jsonl.zst").unwrap(),
            Action::Skip
        );
        let (a, b) = files(dir.path(), b"zz", b"zzz");
        assert_eq!(
            classify(&a, None, &b, 3, "codex/x.jsonl.zst").unwrap(),
            Action::KeepBoth { why: Why::Other }
        );
    }
}
