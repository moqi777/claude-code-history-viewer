//! Byte-exact, append-only mirroring of one source file.
//!
//! Crash safety rests on ordering: bytes are appended to the mirror and
//! fsynced first, and only then does the caller advance the database cursor.
//! If a run dies in between, the mirror is longer than the cursor; the next
//! run truncates it back to the cursor before appending, so nothing is ever
//! duplicated. Old mirror content is never deleted: a rewritten source moves
//! the previous mirror aside as `<name>.prev-<timestamp>` (not `.jsonl`, so
//! nothing mistakes it for a session).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::db::{Cursor, FileRow};
use super::index::for_each_line_chunk;

/// How much of the file start is fingerprinted to notice a rewrite.
pub const HEAD_BYTES: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Size and mtime match the cursor; the file is not even opened.
    Unchanged,
    New,
    /// Source grew in place; continue from this offset.
    Append {
        from: u64,
    },
    /// Source was replaced; the old mirror is kept as `.prev-*`.
    Rewrite,
}

pub struct Observed {
    pub size: u64,
    pub mtime_ns: i64,
    pub compressed: bool,
}

/// Cheap checks first (stat), file content only when something changed.
pub fn decide(
    src: &Path,
    mirror: &Path,
    prior: Option<&FileRow>,
    seen: &Observed,
) -> Result<Decision, String> {
    let Some(row) = prior else {
        return Ok(Decision::New);
    };
    let unchanged =
        u64::try_from(row.src_size).ok() == Some(seen.size) && row.src_mtime_ns == seen.mtime_ns;
    if unchanged {
        return Ok(Decision::Unchanged);
    }
    if seen.compressed {
        return Ok(Decision::Rewrite);
    }
    let prior_size = u64::try_from(row.src_size).unwrap_or(0);
    let head_len = u64::try_from(row.head_len).unwrap_or(0);
    if seen.size < prior_size || seen.size < head_len {
        return Ok(Decision::Rewrite);
    }
    let (hash, _) =
        hash_head(src, head_len).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    if hash != row.head_hash {
        return Ok(Decision::Rewrite);
    }
    let from = u64::try_from(row.mirror_offset).unwrap_or(0);
    // A mirror shorter than its cursor was damaged outside our control.
    // Starting over is the only way to get a consistent copy again.
    let mirror_len = fs::metadata(mirror).map_or(0, |m| m.len());
    if mirror_len < from {
        return Ok(Decision::Rewrite);
    }
    Ok(Decision::Append { from })
}

/// SHA-256 of the first `min(max, file length)` bytes, and how many bytes that was.
pub fn hash_head(path: &Path, max: u64) -> std::io::Result<(Vec<u8>, u64)> {
    let file = File::open(path)?;
    let mut buf = Vec::new();
    file.take(max).read_to_end(&mut buf)?;
    Ok((Sha256::digest(&buf).to_vec(), buf.len() as u64))
}

#[derive(Debug)]
pub struct Mirrored {
    pub cursor: Cursor,
    pub copied_bytes: u64,
    pub longest_line: u64,
    /// The previous mirror was moved aside (rewrite, or unrelated leftovers).
    pub rotated_previous: bool,
}

/// Perform `decision` (never called for `Unchanged`), passing every newly
/// mirrored block of whole lines to `feed`. `feed` sees the complete content
/// for `New`/`Rewrite` and only the new part for `Append`.
pub fn execute(
    src: &Path,
    mirror: &Path,
    decision: Decision,
    seen: &Observed,
    feed: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<Mirrored, String> {
    let io = |what: &str, e: std::io::Error| format!("{what} {}: {e}", src.display());
    let mut rotated = false;
    let from = match decision {
        Decision::Unchanged => return Err("execute called for an unchanged file".into()),
        Decision::Append { from } => {
            // Crash recovery: anything past the cursor is an orphan of a run
            // that died before committing.
            let mirror_file = OpenOptions::new()
                .write(true)
                .open(mirror)
                .map_err(|e| io("cannot open mirror for", e))?;
            if mirror_file
                .metadata()
                .map_err(|e| io("stat mirror for", e))?
                .len()
                > from
            {
                mirror_file
                    .set_len(from)
                    .map_err(|e| io("cannot truncate mirror for", e))?;
            }
            from
        }
        Decision::New => {
            rotated = prepare_fresh_mirror(src, mirror, false)
                .map_err(|e| io("cannot prepare mirror for", e))?;
            0
        }
        Decision::Rewrite => {
            rotated = prepare_fresh_mirror(src, mirror, true)
                .map_err(|e| io("cannot prepare mirror for", e))?;
            0
        }
    };

    let (copied, longest, offset) = if seen.compressed {
        copy_compressed(src, mirror, feed)?
    } else {
        append_plain(src, mirror, from, feed)?
    };

    let (head_hash, head_len) =
        hash_head(mirror, HEAD_BYTES).map_err(|e| io("cannot hash mirror of", e))?;
    Ok(Mirrored {
        cursor: Cursor {
            src_size: i64::try_from(seen.size).unwrap_or(i64::MAX),
            src_mtime_ns: seen.mtime_ns,
            head_hash,
            head_len: i64::try_from(head_len).unwrap_or(0),
            mirror_offset: i64::try_from(offset).unwrap_or(i64::MAX),
        },
        copied_bytes: copied,
        longest_line: longest,
        rotated_previous: rotated,
    })
}

/// Make `mirror` a non-existent path ready for a from-scratch copy.
/// Returns whether an existing mirror was moved aside.
///
/// A leftover mirror with no cursor (a first copy that crashed) is only
/// discarded when it is a byte prefix of the source, i.e. provably
/// reproducible; anything else is preserved as `.prev-*`.
fn prepare_fresh_mirror(src: &Path, mirror: &Path, force_rotate: bool) -> std::io::Result<bool> {
    if let Some(parent) = mirror.parent() {
        fs::create_dir_all(parent)?;
    }
    if !mirror.exists() {
        return Ok(false);
    }
    if !force_rotate && is_prefix_of(mirror, src)? {
        fs::remove_file(mirror)?;
        return Ok(false);
    }
    rotate_aside(mirror)?;
    Ok(true)
}

pub fn rotate_aside(mirror: &Path) -> std::io::Result<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = mirror
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut candidate = mirror.with_file_name(format!("{name}.prev-{stamp}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = mirror.with_file_name(format!("{name}.prev-{stamp}-{n}"));
        n += 1;
    }
    fs::rename(mirror, &candidate)?;
    Ok(candidate)
}

fn is_prefix_of(candidate: &Path, whole: &Path) -> std::io::Result<bool> {
    let (mut a, mut b) = (File::open(candidate)?, File::open(whole)?);
    if a.metadata()?.len() > b.metadata()?.len() {
        return Ok(false);
    }
    let (mut buf_a, mut buf_b) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = a.read(&mut buf_a)?;
        if n == 0 {
            return Ok(true);
        }
        b.read_exact(&mut buf_b[..n])?;
        if buf_a[..n] != buf_b[..n] {
            return Ok(false);
        }
    }
}

/// Whether everything `old` holds is the beginning of what `new` holds, with
/// each side read as plain text (`zstd` unpacked when `compressed`). Codex
/// compresses finished sessions in place; this is how "the same file, stored
/// differently" is told from "a different file with a similar name".
///
/// `old_limit` caps how many bytes of a plain `old` count (a mirror may hold
/// orphan bytes past its cursor).
pub fn content_continues(
    old: (&Path, bool, Option<u64>),
    new: (&Path, bool),
) -> Result<bool, String> {
    let open = |path: &Path, compressed: bool| -> Result<Box<dyn Read>, String> {
        let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        if compressed {
            let reader = zstd::stream::Decoder::new(file)
                .map_err(|e| format!("cannot decompress {}: {e}", path.display()))?;
            Ok(Box::new(reader))
        } else {
            Ok(Box::new(file))
        }
    };
    let (old_path, old_compressed, old_limit) = old;
    let mut a: Box<dyn Read> = open(old_path, old_compressed)?;
    if let (false, Some(limit)) = (old_compressed, old_limit) {
        a = Box::new(a.take(limit));
    }
    let mut b = open(new.0, new.1)?;
    let (mut buf_a, mut buf_b) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    let mut seen_any = false;
    loop {
        let n = read_full(&mut a, &mut buf_a)?;
        if n == 0 {
            // An empty `old` proves nothing about `new`.
            return Ok(seen_any);
        }
        seen_any = true;
        let m = read_full(&mut b, &mut buf_b[..n])?;
        if m < n || buf_a[..n] != buf_b[..n] {
            return Ok(false);
        }
    }
}

/// `read` until `buf` is full or the stream ends.
fn read_full(reader: &mut dyn Read, buf: &mut [u8]) -> Result<usize, String> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    Ok(filled)
}

fn sync_parent_dir(path: &Path) {
    if let Some(dir) = path.parent() {
        if let Ok(handle) = File::open(dir) {
            let _ = handle.sync_all();
        }
    }
}

/// Returns (bytes copied, longest line, new offset).
fn append_plain(
    src: &Path,
    mirror: &Path,
    from: u64,
    feed: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<(u64, u64, u64), String> {
    let mut source = File::open(src).map_err(|e| format!("cannot open {}: {e}", src.display()))?;
    source
        .seek(SeekFrom::Start(from))
        .map_err(|e| format!("cannot seek {}: {e}", src.display()))?;
    if let Some(parent) = mirror.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let existed = mirror.exists();
    let mut out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(mirror)
        .map_err(|e| format!("cannot open mirror {}: {e}", mirror.display()))?;

    let tail = for_each_line_chunk(source, |chunk| {
        out.write_all(chunk)
            .map_err(|e| format!("cannot write mirror {}: {e}", mirror.display()))?;
        feed(chunk)
    })?;

    out.sync_all()
        .map_err(|e| format!("cannot fsync mirror {}: {e}", mirror.display()))?;
    if !existed {
        sync_parent_dir(mirror);
    }
    Ok((tail.consumed, tail.longest_line, from + tail.consumed))
}

/// Compressed rollouts cannot be appended to: copy the whole file atomically
/// (temp + rename) and index its decompressed lines.
fn copy_compressed(
    src: &Path,
    mirror: &Path,
    feed: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<(u64, u64, u64), String> {
    let mut tmp_name = mirror
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = mirror.with_file_name(tmp_name);
    let copied = fs::copy(src, &tmp).map_err(|e| format!("cannot copy {}: {e}", src.display()))?;
    OpenOptions::new()
        .write(true)
        .open(&tmp)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("cannot fsync {}: {e}", tmp.display()))?;
    fs::rename(&tmp, mirror)
        .map_err(|e| format!("cannot move {} into place: {e}", tmp.display()))?;
    sync_parent_dir(mirror);

    let reader = File::open(mirror)
        .and_then(zstd::stream::Decoder::new)
        .map_err(|e| format!("cannot decompress {}: {e}", mirror.display()))?;
    let tail = for_each_line_chunk(reader, |chunk| feed(chunk))?;
    if !tail.tail.is_empty() {
        feed(&tail.tail)?;
    }
    Ok((copied, tail.longest_line, copied))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(path: &Path) -> Observed {
        let (size, mtime_ns) = super::super::discover::mtime_ns(path).unwrap();
        Observed {
            size,
            mtime_ns,
            compressed: false,
        }
    }

    fn row_from(m: &Mirrored, seen: &Observed) -> FileRow {
        FileRow {
            id: 1,
            source: super::super::Source::Claude,
            source_path: String::new(),
            mirror_rel: String::new(),
            src_size: i64::try_from(seen.size).unwrap(),
            src_mtime_ns: seen.mtime_ns,
            head_hash: m.cursor.head_hash.clone(),
            head_len: m.cursor.head_len,
            mirror_offset: m.cursor.mirror_offset,
            is_compressed: seen.compressed,
            deleted: false,
            origin: None,
        }
    }

    fn run(
        src: &Path,
        mirror: &Path,
        prior: Option<&FileRow>,
    ) -> (Decision, Option<Mirrored>, Vec<u8>) {
        let seen = observed(src);
        let decision = decide(src, mirror, prior, &seen).unwrap();
        let mut fed = Vec::new();
        if decision == Decision::Unchanged {
            return (decision, None, fed);
        }
        let m = execute(src, mirror, decision, &seen, &mut |c| {
            fed.extend_from_slice(c);
            Ok(())
        })
        .unwrap();
        (decision, Some(m), fed)
    }

    fn append(path: &Path, text: &str) {
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
        // Make the mtime visibly different even on coarse filesystems.
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .ok();
    }

    #[test]
    fn new_then_unchanged_then_append_keeps_the_mirror_identical() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mirror) = (dir.path().join("s.jsonl"), dir.path().join("raw/m.jsonl"));
        fs::write(&src, "{\"a\":1}\n{\"a\":2}\n").unwrap();

        let (d, m, fed) = run(&src, &mirror, None);
        assert_eq!(d, Decision::New);
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());
        assert_eq!(fed, fs::read(&src).unwrap());
        let m = m.unwrap();
        assert_eq!(m.copied_bytes, 16);
        let row = row_from(&m, &observed(&src));

        let (d, m2, _) = run(&src, &mirror, Some(&row));
        assert_eq!(d, Decision::Unchanged);
        assert!(m2.is_none());

        append(&src, "{\"a\":3}\n");
        let (d, m3, fed) = run(&src, &mirror, Some(&row));
        assert_eq!(d, Decision::Append { from: 16 });
        assert_eq!(fed, b"{\"a\":3}\n");
        assert_eq!(m3.unwrap().copied_bytes, 8);
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());
    }

    #[test]
    fn a_half_written_line_waits_for_the_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mirror) = (dir.path().join("s.jsonl"), dir.path().join("m.jsonl"));
        fs::write(&src, "{\"a\":1}\n{\"a\"").unwrap();
        let (_, m, fed) = run(&src, &mirror, None);
        let m = m.unwrap();
        assert_eq!(fed, b"{\"a\":1}\n");
        assert_eq!(fs::read(&mirror).unwrap(), b"{\"a\":1}\n");
        assert_eq!(m.cursor.mirror_offset, 8);
        let row = row_from(&m, &observed(&src));

        append(&src, ":2}\n");
        let (d, m2, fed) = run(&src, &mirror, Some(&row));
        assert_eq!(d, Decision::Append { from: 8 });
        assert_eq!(fed, b"{\"a\":2}\n");
        assert_eq!(m2.unwrap().cursor.mirror_offset, 16);
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());
    }

    #[test]
    fn growing_past_the_fingerprint_size_is_an_append_not_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mirror) = (dir.path().join("s.jsonl"), dir.path().join("m.jsonl"));
        fs::write(&src, "{\"a\":1}\n").unwrap();
        let (_, m, _) = run(&src, &mirror, None);
        let row = row_from(&m.unwrap(), &observed(&src));
        assert!(row.head_len < i64::try_from(HEAD_BYTES).unwrap());

        append(&src, &format!("{{\"pad\":\"{}\"}}\n", "x".repeat(6000)));
        let (d, _, _) = run(&src, &mirror, Some(&row));
        assert_eq!(d, Decision::Append { from: 8 });
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());
    }

    #[test]
    fn rewritten_source_rotates_the_old_mirror_aside() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mirror) = (dir.path().join("s.jsonl"), dir.path().join("m.jsonl"));
        fs::write(&src, "{\"v\":\"original\"}\n").unwrap();
        let (_, m, _) = run(&src, &mirror, None);
        let row = row_from(&m.unwrap(), &observed(&src));

        fs::write(&src, "{\"v\":\"CHANGED!\"}\n{\"more\":1}\n").unwrap();
        let (d, m, fed) = run(&src, &mirror, Some(&row));
        assert_eq!(d, Decision::Rewrite);
        assert!(m.unwrap().rotated_previous);
        assert_eq!(fed, fs::read(&src).unwrap());
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());

        let prev: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".prev-"))
            .collect();
        assert_eq!(prev.len(), 1);
        assert!(!std::path::Path::new(&prev[0])
            .extension()
            .is_some_and(|e| e == "jsonl"));
        assert_eq!(
            fs::read(dir.path().join(&prev[0])).unwrap(),
            b"{\"v\":\"original\"}\n"
        );
    }

    #[test]
    fn shrunk_source_is_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mirror) = (dir.path().join("s.jsonl"), dir.path().join("m.jsonl"));
        fs::write(&src, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        let (_, m, _) = run(&src, &mirror, None);
        let row = row_from(&m.unwrap(), &observed(&src));
        fs::write(&src, "{\"a\":1}\n").unwrap();
        let (d, _, _) = run(&src, &mirror, Some(&row));
        assert_eq!(d, Decision::Rewrite);
    }

    #[test]
    fn crash_between_mirror_append_and_cursor_update_does_not_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mirror) = (dir.path().join("s.jsonl"), dir.path().join("m.jsonl"));
        fs::write(&src, "{\"a\":1}\n").unwrap();
        let (_, m, _) = run(&src, &mirror, None);
        let row = row_from(&m.unwrap(), &observed(&src));

        // Simulate: a later run appended to the mirror and died before the
        // cursor moved (mirror is longer than the cursor says).
        append(&src, "{\"a\":2}\n");
        let seen = observed(&src);
        let decision = decide(&src, &mirror, Some(&row), &seen).unwrap();
        execute(&src, &mirror, decision, &seen, &mut |_| Ok(())).unwrap();
        assert_eq!(
            fs::metadata(&mirror).unwrap().len(),
            16,
            "appended but cursor not committed"
        );

        // The next run still holds the OLD cursor: it must truncate, then append once.
        let (d, m, fed) = run(&src, &mirror, Some(&row));
        assert_eq!(d, Decision::Append { from: 8 });
        assert_eq!(fed, b"{\"a\":2}\n");
        assert_eq!(m.unwrap().cursor.mirror_offset, 16);
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());
    }

    #[test]
    fn mirror_shorter_than_its_cursor_forces_a_fresh_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mirror) = (dir.path().join("s.jsonl"), dir.path().join("m.jsonl"));
        fs::write(&src, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        let (_, m, _) = run(&src, &mirror, None);
        let row = row_from(&m.unwrap(), &observed(&src));
        fs::write(&mirror, "{\"a\":1}\n").unwrap();
        append(&src, "{\"a\":3}\n");
        let (d, _, _) = run(&src, &mirror, Some(&row));
        assert_eq!(d, Decision::Rewrite);
    }

    #[test]
    fn leftover_mirror_without_cursor_is_kept_unless_provably_reproducible() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s.jsonl");
        fs::write(&src, "{\"a\":1}\n{\"a\":2}\n").unwrap();

        // A prefix of the source: safe to redo in place, no .prev left.
        let mirror = dir.path().join("m1.jsonl");
        fs::write(&mirror, "{\"a\":1}\n").unwrap();
        let (_, m, _) = run(&src, &mirror, None);
        assert!(!m.unwrap().rotated_previous);
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());

        // Not a prefix: preserved.
        let mirror = dir.path().join("m2.jsonl");
        fs::write(&mirror, "something else entirely\n").unwrap();
        let (_, m, _) = run(&src, &mirror, None);
        assert!(m.unwrap().rotated_previous);
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());
    }

    #[test]
    fn compressed_files_are_copied_whole_and_indexed_decompressed() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mirror) = (
            dir.path().join("r.jsonl.zst"),
            dir.path().join("raw/r.jsonl.zst"),
        );
        let plain = b"{\"a\":1}\n{\"a\":2}";
        fs::write(&src, zstd::encode_all(&plain[..], 3).unwrap()).unwrap();
        let mut seen = observed(&src);
        seen.compressed = true;
        let decision = decide(&src, &mirror, None, &seen).unwrap();
        let mut fed = Vec::new();
        let m = execute(&src, &mirror, decision, &seen, &mut |c| {
            fed.extend_from_slice(c);
            Ok(())
        })
        .unwrap();
        assert_eq!(fed, plain, "the unterminated last line is still indexed");
        assert_eq!(fs::read(&mirror).unwrap(), fs::read(&src).unwrap());
        assert_eq!(m.copied_bytes, seen.size);

        let row = row_from(&m, &seen);
        let again = decide(&src, &mirror, Some(&row), &seen).unwrap();
        assert_eq!(again, Decision::Unchanged);
    }
}
