//! Session files as stored: plain `.jsonl`, or a backup that `AgentKB`
//! compressed (`X.jsonl.packed.zst`, see `kb/shrink.rs`).
//!
//! A compressed backup is decompressed into memory once and the bytes are
//! kept for the next read of the same file (pagination and scrolling read it
//! again and again). Only the most recent one is kept.
//!
//! The backup may change between the moment a path was handed out and the
//! moment it is read: compressed (`X.jsonl` becomes `X.jsonl.packed.zst`) or
//! unpacked for new messages (the other way). A path that is gone is
//! therefore tried once more under the other name.

use std::io::{BufRead, BufReader, Read};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use memmap2::Mmap;
use once_cell::sync::Lazy;

use crate::kb::shrink::PACKED_SUFFIX;

/// Whether `path` is a backup `AgentKB` compressed (`*.jsonl.packed.zst`).
#[allow(clippy::case_sensitive_file_extension_comparisons)] // exact names the agents write
pub fn is_packed(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(PACKED_SUFFIX))
        .is_some_and(|plain| plain.ends_with(".jsonl"))
}

/// The same session under its other storage name.
pub fn twin_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if let Some(plain) = name.strip_suffix(PACKED_SUFFIX) {
        return Some(path.with_file_name(plain));
    }
    std::path::Path::new(name)
        .extension()
        .is_some_and(|e| e == "jsonl")
        .then(|| path.with_file_name(format!("{name}{PACKED_SUFFIX}")))
}

/// `path` when it exists, else its twin when that exists, else `path`.
pub fn existing(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    twin_path(path)
        .filter(|twin| twin.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

pub enum SessionBytes {
    Mapped(Mmap),
    Owned(Arc<Vec<u8>>),
}

impl Deref for SessionBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Mapped(map) => map,
            Self::Owned(bytes) => bytes,
        }
    }
}

type Key = (PathBuf, u64, Option<SystemTime>);

type Cached = Option<(Key, Arc<Vec<u8>>)>;

static LAST_UNPACKED: Lazy<Mutex<Cached>> = Lazy::new(|| Mutex::new(None));

/// The whole content of a session file: a memory map for plain text, the
/// decompressed bytes for a compressed backup.
#[allow(unsafe_code)] // Required for mmap performance optimization
pub fn session_bytes(path: &Path) -> Result<SessionBytes, String> {
    let path = existing(path);
    let file =
        std::fs::File::open(&path).map_err(|e| format!("Failed to open session file: {e}"))?;
    if !is_packed(&path) {
        // SAFETY: We're only reading the file, and the file handle is kept open
        // for the duration of the mmap's lifetime. Session files are append-only.
        let map = unsafe { Mmap::map(&file) }
            .map_err(|e| format!("Failed to memory-map session file: {e}"))?;
        return Ok(SessionBytes::Mapped(map));
    }
    let meta = file
        .metadata()
        .map_err(|e| format!("Failed to read session file: {e}"))?;
    let key: Key = (path.clone(), meta.len(), meta.modified().ok());
    let mut last = LAST_UNPACKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((cached, bytes)) = last.as_ref() {
        if *cached == key {
            return Ok(SessionBytes::Owned(Arc::clone(bytes)));
        }
    }
    *last = None;
    let bytes = Arc::new(
        zstd::decode_all(BufReader::new(file))
            .map_err(|e| format!("Failed to decompress session file: {e}"))?,
    );
    *last = Some((key, Arc::clone(&bytes)));
    Ok(SessionBytes::Owned(bytes))
}

/// A line reader over the plain text of a session file.
pub fn open_lines(path: &Path) -> std::io::Result<Box<dyn BufRead>> {
    let path = existing(path);
    let file = std::fs::File::open(&path)?;
    if is_packed(&path) {
        Ok(Box::new(BufReader::new(zstd::stream::Decoder::new(file)?)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

/// The plain text of a session file, read whole.
pub fn read_all(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    open_lines(path)?.read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compressed_backup_reads_as_its_plain_text_under_either_name() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("s.jsonl");
        let packed = dir.path().join("s.jsonl.packed.zst");
        let text = b"{\"a\":1}\n{\"b\":2}\n";
        std::fs::write(&packed, zstd::encode_all(&text[..], 9).unwrap()).unwrap();
        assert!(is_packed(&packed) && !is_packed(&plain));
        assert_eq!(&*session_bytes(&packed).unwrap(), text);
        assert_eq!(
            &*session_bytes(&plain).unwrap(),
            text,
            "the old name still opens"
        );
        assert_eq!(read_all(&plain).unwrap(), text);

        std::fs::remove_file(&packed).unwrap();
        std::fs::write(&plain, text).unwrap();
        assert_eq!(&*session_bytes(&packed).unwrap(), text, "and the other way");
        assert!(session_bytes(&dir.path().join("t.jsonl")).is_err());
    }

    #[test]
    fn twin_names() {
        let p = Path::new("/a/s.jsonl");
        assert_eq!(twin_path(p).unwrap(), Path::new("/a/s.jsonl.packed.zst"));
        assert_eq!(twin_path(&twin_path(p).unwrap()).unwrap(), p);
        assert_eq!(twin_path(Path::new("/a/s.json")), None);
        assert!(!is_packed(Path::new("/a/s.txt.packed.zst")));
    }
}
