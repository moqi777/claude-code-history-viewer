//! Small filesystem helpers shared by the config, backup and migration code.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const COPY_BUF: usize = 1024 * 1024;

pub fn sync_file(file: &File) -> std::io::Result<()> {
    match file.sync_all() {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(()),
        Err(e) => Err(e),
    }
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(tmp)
}

/// Flush a directory entry change (rename/create) to disk where supported.
pub fn fsync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Temp file + fsync + rename in the same directory, so a reader sees either
/// the old or the new content, never a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = tmp_sibling(path);
    let write = || -> std::io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        sync_file(&file)
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", tmp.display()));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot move {} into place: {e}", tmp.display())
    })?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent);
    }
    Ok(())
}

/// Copy `src` to `dst` through a temp file, renaming at the end. Returns bytes
/// copied. Creates missing parent directories of `dst`.
pub fn copy_atomic(src: &Path, dst: &Path) -> Result<u64, String> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let tmp = tmp_sibling(dst);
    let copy = || -> std::io::Result<u64> {
        let mut input = File::open(src)?;
        let mut output = File::create(&tmp)?;
        let n = std::io::copy(&mut input, &mut output)?;
        sync_file(&output)?;
        Ok(n)
    };
    let copied = copy().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot copy {} to {}: {e}", src.display(), dst.display())
    })?;
    fs::rename(&tmp, dst).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot move {} into place: {e}", tmp.display())
    })?;
    Ok(copied)
}

/// Append `src[from..]` to the end of `dst` (which must already be exactly
/// `from` bytes long) and fsync. Returns bytes appended.
pub fn append_tail(src: &Path, dst: &Path, from: u64) -> Result<u64, String> {
    use std::io::{Seek, SeekFrom};
    let run = || -> std::io::Result<u64> {
        let mut input = File::open(src)?;
        input.seek(SeekFrom::Start(from))?;
        let mut output = fs::OpenOptions::new().write(true).open(dst)?;
        // Anything past the cursor is an orphan of an interrupted run.
        if output.metadata()?.len() != from {
            output.set_len(from)?;
        }
        output.seek(SeekFrom::Start(from))?;
        let n = std::io::copy(&mut input, &mut output)?;
        sync_file(&output)?;
        Ok(n)
    };
    run().map_err(|e| format!("cannot append {} to {}: {e}", src.display(), dst.display()))
}

/// Streaming SHA-256 of a whole file, hex-encoded.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        }))
}

/// Every regular file under `root`, as (path relative to root with `/`
/// separators, size). Symlinks are skipped. Missing root → empty.
pub fn walk_files(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let Ok(meta) = entry.metadata() else { continue };
                if let Ok(rel) = path.strip_prefix(root) {
                    let rel = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    out.push((rel, meta.len()));
                }
            }
        }
    }
    out.sort();
    out
}

pub fn dir_size(root: &Path) -> u64 {
    walk_files(root).iter().map(|(_, n)| n).sum()
}

/// Free bytes on the filesystem holding `path` (or its nearest existing ancestor).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut probe = Some(path);
    while let Some(p) = probe {
        if p.exists() {
            return fs4::available_space(p).ok();
        }
        probe = p.parent();
    }
    None
}

/// `path` itself, or its nearest ancestor that exists. Never creates anything.
pub fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut probe = Some(path);
    while let Some(p) = probe {
        if p.exists() {
            return Some(p.to_path_buf());
        }
        probe = p.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn append_tail_truncates_orphans_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::write(&src, b"abcdefgh").unwrap();
        fs::write(&dst, b"abcXX").unwrap();
        assert_eq!(append_tail(&src, &dst, 3).unwrap(), 5);
        assert_eq!(fs::read(&dst).unwrap(), b"abcdefgh");
    }

    #[test]
    fn walk_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("x/y")).unwrap();
        fs::write(dir.path().join("x/y/f"), b"hello").unwrap();
        fs::write(dir.path().join("g"), b"").unwrap();
        let files = walk_files(dir.path());
        assert_eq!(files, vec![("g".into(), 0), ("x/y/f".into(), 5)]);
        assert_eq!(
            sha256_file(&dir.path().join("x/y/f")).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(dir_size(dir.path()), 5);
    }
}
