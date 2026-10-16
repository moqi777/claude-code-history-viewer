//! The `.agentkb` container.
//!
//! ```text
//! 0     "AGENTKB\0" | format u16 | flags u16 | reserved u32      (16 bytes)
//! 16    one zstd frame (content checksum on) holding the entry stream
//! end   manifest JSON | sha256(manifest) | manifest_len u64 | "AGENTKB\0"
//! ```
//!
//! The entry stream is `kind u8 = 1 | path_len u16 | path | size u64 | bytes`
//! repeated, then `kind u8 = 0`. Paths are relative with `/` and checked on
//! the way out; there is no way to express a symlink, a permission or a path
//! outside the target. The manifest is at the end so a writer can hash while
//! it streams, and a reader finds it with one seek.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::transfer::{hex, Cancel, TResult, TransferError};

pub const MAGIC: &[u8; 8] = b"AGENTKB\0";
pub const FORMAT: u16 = 1;
pub const HEADER_LEN: u64 = 16;
const TRAILER_FIXED: u64 = 32 + 8 + 8;
const CHUNK: usize = 1024 * 1024;
const MAX_PATH_LEN: usize = 1024;
/// A manifest this big is not one of ours (600 files make ~100 KB).
const MAX_MANIFEST: u64 = 256 * 1024 * 1024;
pub const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SourceDevice {
    pub name: String,
    pub os: String,
    pub home: String,
    /// The exporting machine's data directory (its digest work folder shows
    /// up in the stored exclusion list).
    pub data_dir: String,
    pub claude_root: Option<String>,
    pub codex_root: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Counts {
    pub sessions: u64,
    pub notes: u64,
    pub sessions_with_notes: u64,
    pub raw_files: u64,
    pub note_files: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Totals {
    pub uncompressed_bytes: u64,
    pub payload_files: u64,
    /// Size of the compressed frame, checked against the file length.
    pub payload_bytes: u64,
    /// SHA-256 of the compressed frame: covers the few frame-header bytes that
    /// zstd's own checksum does not.
    pub payload_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Compression {
    pub codec: String,
    pub level: i32,
}

impl Default for Compression {
    fn default() -> Self {
        Self {
            codec: "zstd".into(),
            level: ZSTD_LEVEL,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Encryption {
    pub scheme: String,
}

impl Default for Encryption {
    fn default() -> Self {
        Self {
            scheme: "none".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SourcesEnabled {
    pub claude: bool,
    pub codex: bool,
}

/// The settings that mean the same on another machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PortableDigest {
    pub engine: String,
    pub model: String,
    pub time: String,
    pub max_sessions_per_round: usize,
    pub recent_days: u32,
    pub quiet_minutes: u32,
    pub min_messages: usize,
    pub max_chunks_per_session: usize,
    pub codex_reasoning: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PortableSettings {
    pub interval_secs: u64,
    pub sources_enabled: SourcesEnabled,
    pub exclude_prefixes: Vec<String>,
    pub exclude_cwd_prefixes: Vec<String>,
    pub digest: PortableDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Manifest {
    pub format: u16,
    pub min_reader_format: u16,
    pub kind: String,
    pub export_id: String,
    pub exported_at: String,
    pub app: AppInfo,
    pub schema_version: i64,
    pub source_device: SourceDevice,
    pub counts: Counts,
    pub totals: Totals,
    pub compression: Compression,
    pub encryption: Encryption,
    pub settings: PortableSettings,
    pub files: Vec<FileEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            format: FORMAT,
            min_reader_format: 1,
            kind: "agentkb-export".into(),
            export_id: String::new(),
            exported_at: String::new(),
            app: AppInfo::default(),
            schema_version: 0,
            source_device: SourceDevice::default(),
            counts: Counts::default(),
            totals: Totals::default(),
            compression: Compression::default(),
            encryption: Encryption::default(),
            settings: PortableSettings::default(),
            files: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Paths inside the container
// ---------------------------------------------------------------------------

/// What an entry path must look like. The same rules hold on every platform,
/// so a file written on a Mac can be read on Windows.
pub fn check_entry_path(path: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("unsafe path in the file ({why}): {path:?}"));
    if path.is_empty() || path.len() > MAX_PATH_LEN {
        return bad("length");
    }
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return bad("absolute or not portable");
    }
    let mut parts = path.split('/').peekable();
    let first = parts.peek().copied().unwrap_or("");
    if !(path == "kb.db" || first == "raw" || first == "notes") {
        return bad("not under raw/ or notes/");
    }
    if first != "kb.db" && path.split('/').count() < 2 {
        return bad("a folder name, not a file");
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return bad("empty or relative segment");
        }
        if part.ends_with(['.', ' ']) && part != "." {
            return bad("segment ends with a dot or space");
        }
        if part
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return bad("character not allowed on every platform");
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0');
        if reserved {
            return bad("reserved name");
        }
    }
    Ok(())
}

/// Every path valid, none repeated even when only the case differs.
pub fn check_manifest_paths(files: &[FileEntry]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for f in files {
        check_entry_path(&f.path)?;
        if !seen.insert(f.path.to_lowercase()) {
            return Err(format!("the file lists {:?} twice", f.path));
        }
        if f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("bad checksum for {:?}", f.path));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

struct Counter<W> {
    inner: W,
    written: u64,
    hasher: Sha256,
}

impl<W: Write> Write for Counter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub struct Writer {
    enc: zstd::stream::write::Encoder<'static, Counter<BufWriter<File>>>,
    entries: Vec<FileEntry>,
    uncompressed: u64,
}

fn io_err(what: &str, e: impl std::fmt::Display) -> TransferError {
    TransferError::io(format!("{what}: {e}"))
}

impl Writer {
    pub fn new(file: File) -> TResult<Self> {
        let mut out = BufWriter::with_capacity(CHUNK, file);
        let mut header = Vec::with_capacity(HEADER_LEN as usize);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&FORMAT.to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        out.write_all(&header)
            .map_err(|e| io_err("cannot write the export", e))?;
        let mut enc = zstd::stream::write::Encoder::new(
            Counter {
                inner: out,
                written: 0,
                hasher: Sha256::new(),
            },
            ZSTD_LEVEL,
        )
        .map_err(|e| io_err("cannot start compression", e))?;
        enc.include_checksum(true)
            .map_err(|e| io_err("cannot start compression", e))?;
        Ok(Self {
            enc,
            entries: Vec::new(),
            uncompressed: 0,
        })
    }

    /// Append one file of exactly `size` bytes read from `src`. A source that
    /// turns out shorter is an error: the entry would otherwise be a lie.
    pub fn add(
        &mut self,
        path: &str,
        size: u64,
        src: &mut dyn Read,
        cancel: &Cancel,
        on_bytes: &mut dyn FnMut(u64),
    ) -> TResult<()> {
        check_entry_path(path).map_err(TransferError::io)?;
        let mut head = Vec::with_capacity(path.len() + 11);
        head.push(1u8);
        head.extend_from_slice(
            &u16::try_from(path.len())
                .map_err(|_| TransferError::io("path too long"))?
                .to_le_bytes(),
        );
        head.extend_from_slice(path.as_bytes());
        head.extend_from_slice(&size.to_le_bytes());
        self.enc
            .write_all(&head)
            .map_err(|e| io_err("cannot write the export", e))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut left = size;
        while left > 0 {
            cancel.check()?;
            let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
            let n = src
                .read(&mut buf[..want])
                .map_err(|e| io_err(&format!("cannot read {path}"), e))?;
            if n == 0 {
                return Err(TransferError::io(format!(
                    "{path} became shorter while it was being exported"
                )));
            }
            hasher.update(&buf[..n]);
            self.enc
                .write_all(&buf[..n])
                .map_err(|e| io_err("cannot write the export", e))?;
            left -= n as u64;
            on_bytes(n as u64);
        }
        self.uncompressed += size;
        self.entries.push(FileEntry {
            path: path.to_string(),
            size,
            sha256: hex(&hasher.finalize()),
        });
        Ok(())
    }

    pub fn uncompressed_bytes(&self) -> u64 {
        self.uncompressed
    }

    /// Close the stream and write the trailer. Returns the finished file.
    pub fn finish(mut self, mut manifest: Manifest) -> TResult<File> {
        self.enc
            .write_all(&[0u8])
            .map_err(|e| io_err("cannot write the export", e))?;
        let mut counter = self
            .enc
            .finish()
            .map_err(|e| io_err("cannot finish compression", e))?;
        manifest.totals.payload_bytes = counter.written;
        manifest.totals.payload_sha256 = hex(&counter.hasher.clone().finalize());
        manifest.totals.payload_files = self.entries.len() as u64;
        manifest.totals.uncompressed_bytes = self.uncompressed;
        manifest.files = self.entries;
        let json =
            serde_json::to_vec(&manifest).map_err(|e| io_err("cannot write the manifest", e))?;
        let mut trailer = json.clone();
        trailer.extend_from_slice(&Sha256::digest(&json));
        trailer.extend_from_slice(&(json.len() as u64).to_le_bytes());
        trailer.extend_from_slice(MAGIC);
        counter
            .inner
            .write_all(&trailer)
            .map_err(|e| io_err("cannot write the export", e))?;
        let file = counter
            .inner
            .into_inner()
            .map_err(|e| io_err("cannot write the export", e.error()))?;
        file.sync_all()
            .map_err(|e| io_err("cannot write the export", e))?;
        Ok(file)
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Header and trailer checks only: cheap, and enough to say what the file is.
pub fn read_manifest(path: &Path) -> TResult<Manifest> {
    let mut file =
        File::open(path).map_err(|e| io_err(&format!("cannot open {}", path.display()), e))?;
    let len = file
        .metadata()
        .map_err(|e| io_err("cannot read the file", e))?
        .len();
    if len < HEADER_LEN + TRAILER_FIXED + 2 {
        return Err(TransferError::corrupt(
            "the file is too short to be an AgentKB export",
        ));
    }
    let mut header = [0u8; HEADER_LEN as usize];
    file.read_exact(&mut header)
        .map_err(|e| io_err("cannot read the file", e))?;
    if &header[..8] != MAGIC {
        return Err(TransferError::corrupt("this is not an AgentKB export"));
    }
    let format = u16::from_le_bytes([header[8], header[9]]);
    let flags = u16::from_le_bytes([header[10], header[11]]);
    let reserved = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    if format > FORMAT {
        return Err(TransferError::new(
            "too_new",
            format!("the file uses export format {format}; update AgentKB to read it"),
        ));
    }
    if format == 0 || flags != 0 || reserved != 0 {
        return Err(TransferError::corrupt("the file header is damaged"));
    }
    file.seek(SeekFrom::Start(len - TRAILER_FIXED))
        .map_err(|e| io_err("cannot read the file", e))?;
    let mut fixed = [0u8; TRAILER_FIXED as usize];
    file.read_exact(&mut fixed)
        .map_err(|e| io_err("cannot read the file", e))?;
    if &fixed[40..] != MAGIC {
        return Err(TransferError::corrupt(
            "the end of the file is missing: the export is incomplete or damaged",
        ));
    }
    let sha: [u8; 32] = fixed[..32].try_into().expect("32 bytes");
    let manifest_len = u64::from_le_bytes(fixed[32..40].try_into().expect("8 bytes"));
    if manifest_len == 0
        || manifest_len > MAX_MANIFEST
        || manifest_len > len - HEADER_LEN - TRAILER_FIXED
    {
        return Err(TransferError::corrupt(
            "the file's contents list is damaged",
        ));
    }
    file.seek(SeekFrom::Start(len - TRAILER_FIXED - manifest_len))
        .map_err(|e| io_err("cannot read the file", e))?;
    let mut json = vec![0u8; usize::try_from(manifest_len).unwrap_or(0)];
    file.read_exact(&mut json)
        .map_err(|e| io_err("cannot read the file", e))?;
    if Sha256::digest(&json).as_slice() != sha {
        return Err(TransferError::corrupt(
            "the file's contents list does not match its checksum",
        ));
    }
    let manifest: Manifest = serde_json::from_slice(&json)
        .map_err(|e| TransferError::corrupt(format!("the file's contents list is invalid: {e}")))?;
    if manifest.kind != "agentkb-export" {
        return Err(TransferError::corrupt("this is not an AgentKB export"));
    }
    if manifest.format != format {
        return Err(TransferError::corrupt("the file header does not match"));
    }
    if manifest.min_reader_format > FORMAT {
        return Err(TransferError::new(
            "too_new",
            "the file needs a newer AgentKB to be read; update the app",
        ));
    }
    if manifest.encryption.scheme != "none" {
        return Err(TransferError::new(
            "too_new",
            format!(
                "the file is encrypted ({}), which this AgentKB cannot read",
                manifest.encryption.scheme
            ),
        ));
    }
    if manifest.compression.codec != "zstd" {
        return Err(TransferError::new(
            "too_new",
            format!("unknown compression {}", manifest.compression.codec),
        ));
    }
    if manifest.totals.payload_bytes != len - HEADER_LEN - TRAILER_FIXED - manifest_len {
        return Err(TransferError::corrupt(
            "the file length does not match its contents list",
        ));
    }
    check_manifest_paths(&manifest.files).map_err(TransferError::corrupt)?;
    Ok(manifest)
}

struct HashingReader<R> {
    inner: R,
    hasher: std::rc::Rc<std::cell::RefCell<Sha256>>,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.borrow_mut().update(&buf[..n]);
        Ok(n)
    }
}

/// Unpack every entry under `dest`, which must not hold any of them yet.
/// Each file is checked against the manifest (path, order, size, SHA-256) as
/// it is written; the first mismatch aborts. Nothing outside `dest` is touched.
pub fn extract(
    path: &Path,
    manifest: &Manifest,
    dest: &Path,
    cancel: &Cancel,
    on_file: &mut dyn FnMut(&FileEntry, u64, bool),
) -> TResult<()> {
    let mut file =
        File::open(path).map_err(|e| io_err(&format!("cannot open {}", path.display()), e))?;
    file.seek(SeekFrom::Start(HEADER_LEN))
        .map_err(|e| io_err("cannot read the file", e))?;
    // The decoder may not read past the compressed frame into the trailer.
    let payload_hash = std::rc::Rc::new(std::cell::RefCell::new(Sha256::new()));
    let bounded = HashingReader {
        inner: file.take(manifest.totals.payload_bytes),
        hasher: payload_hash.clone(),
    };
    let mut decoder = zstd::stream::read::Decoder::new(bounded)
        .map_err(|e| TransferError::corrupt(format!("cannot decompress: {e}")))?
        .single_frame();
    let damaged = |what: &str, e: std::io::Error| {
        TransferError::corrupt(format!("the file is damaged ({what}): {e}"))
    };
    let mut buf = vec![0u8; CHUNK];
    for expected in &manifest.files {
        cancel.check()?;
        let mut kind = [0u8; 1];
        decoder
            .read_exact(&mut kind)
            .map_err(|e| damaged("entry", e))?;
        if kind[0] != 1 {
            return Err(TransferError::corrupt(
                "the file holds fewer or different entries than its contents list",
            ));
        }
        let mut len = [0u8; 2];
        decoder
            .read_exact(&mut len)
            .map_err(|e| damaged("entry", e))?;
        let plen = usize::from(u16::from_le_bytes(len));
        let mut name = vec![0u8; plen];
        decoder
            .read_exact(&mut name)
            .map_err(|e| damaged("entry", e))?;
        let mut size = [0u8; 8];
        decoder
            .read_exact(&mut size)
            .map_err(|e| damaged("entry", e))?;
        let size = u64::from_le_bytes(size);
        if name != expected.path.as_bytes() || size != expected.size {
            return Err(TransferError::corrupt(format!(
                "the entry for {} does not match the contents list",
                expected.path
            )));
        }
        let target = dest.join(&expected.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_err(&format!("cannot create {}", parent.display()), e))?;
        }
        let mut out = BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| io_err(&format!("cannot create {}", target.display()), e))?,
        );
        let mut hasher = Sha256::new();
        let mut left = size;
        while left > 0 {
            cancel.check()?;
            let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
            let n = decoder
                .read(&mut buf[..want])
                .map_err(|e| damaged(&expected.path, e))?;
            if n == 0 {
                return Err(TransferError::corrupt(format!(
                    "the file ends inside {}",
                    expected.path
                )));
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])
                .map_err(|e| io_err(&format!("cannot write {}", target.display()), e))?;
            left -= n as u64;
            on_file(expected, n as u64, left == 0);
        }
        if size == 0 {
            on_file(expected, 0, true);
        }
        let file = out
            .into_inner()
            .map_err(|e| io_err("cannot write", e.error()))?;
        file.sync_all()
            .map_err(|e| io_err(&format!("cannot write {}", target.display()), e))?;
        if hex(&hasher.finalize()) != expected.sha256 {
            return Err(TransferError::corrupt(format!(
                "the checksum of {} is wrong: the file is damaged",
                expected.path
            )));
        }
    }
    let mut end = [0u8; 1];
    decoder
        .read_exact(&mut end)
        .map_err(|e| damaged("end", e))?;
    if end[0] != 0 {
        return Err(TransferError::corrupt(
            "the file holds more entries than its contents list",
        ));
    }
    let mut extra = [0u8; 1];
    match decoder.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => {
            return Err(TransferError::corrupt(
                "unexpected data after the last entry",
            ))
        }
        Err(e) => return Err(damaged("end", e)),
    }
    drop(decoder);
    let actual = hex(&payload_hash.borrow().clone().finalize());
    if actual != manifest.totals.payload_sha256 {
        return Err(TransferError::corrupt(
            "the file's checksum is wrong: the file is damaged",
        ));
    }
    Ok(())
}

/// A reader over one file inside an open container is not needed; this reads
/// a plain file for `Writer::add` with an exact length.
pub fn open_prefix(path: &Path, len: u64) -> TResult<impl Read> {
    let file =
        File::open(path).map_err(|e| io_err(&format!("cannot open {}", path.display()), e))?;
    Ok(BufReader::with_capacity(CHUNK, file).take(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(dir: &Path) -> (std::path::PathBuf, Manifest) {
        let out = dir.join("x.agentkb");
        let file = File::create(&out).unwrap();
        let mut w = Writer::new(file).unwrap();
        let cancel = Cancel::new();
        let mut sink = |_n: u64| {};
        w.add("kb.db", 5, &mut &b"hello"[..], &cancel, &mut sink)
            .unwrap();
        w.add(
            "raw/claude/projects/p/s.jsonl",
            13,
            &mut &b"line1\nline2\n\n"[..],
            &cancel,
            &mut sink,
        )
        .unwrap();
        w.add("notes/claude/a.json", 0, &mut &b""[..], &cancel, &mut sink)
            .unwrap();
        let manifest = Manifest {
            export_id: "id1".into(),
            ..Manifest::default()
        };
        w.finish(manifest).unwrap();
        let m = read_manifest(&out).unwrap();
        (out, m)
    }

    #[test]
    fn a_file_round_trips_with_every_entry_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (path, m) = sample(dir.path());
        assert_eq!(m.files.len(), 3);
        assert_eq!(m.totals.uncompressed_bytes, 18);
        let dest = dir.path().join("out");
        let mut seen = 0;
        extract(&path, &m, &dest, &Cancel::new(), &mut |_, n, _| seen += n).unwrap();
        assert_eq!(seen, 18);
        assert_eq!(std::fs::read(dest.join("kb.db")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(dest.join("raw/claude/projects/p/s.jsonl")).unwrap(),
            b"line1\nline2\n\n"
        );
        assert!(dest.join("notes/claude/a.json").is_file());
    }

    #[test]
    fn changing_any_single_byte_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = sample(dir.path());
        let original = std::fs::read(&path).unwrap();
        for at in 0..original.len() {
            let mut bad = original.clone();
            bad[at] ^= 0x01;
            let broken = dir.path().join("bad.agentkb");
            std::fs::write(&broken, &bad).unwrap();
            let dest = dir.path().join(format!("o{at}"));
            let outcome = read_manifest(&broken)
                .and_then(|m| extract(&broken, &m, &dest, &Cancel::new(), &mut |_, _, _| {}));
            assert!(
                outcome.is_err(),
                "byte {at} changed but the file was accepted"
            );
        }
    }

    #[test]
    fn header_reserved_bytes_and_flags_must_be_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = sample(dir.path());
        for at in [10usize, 11, 12, 13, 14, 15] {
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[at] = 1;
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(read_manifest(&path).unwrap_err().code, "corrupt", "{at}");
        }
    }

    #[test]
    fn truncated_and_extended_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = sample(dir.path());
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(read_manifest(&path).is_err());
        std::fs::write(&path, &bytes[..40]).unwrap();
        assert!(read_manifest(&path).is_err());
        let mut longer = bytes;
        longer.push(0);
        std::fs::write(&path, &longer).unwrap();
        assert!(read_manifest(&path).is_err());
    }

    #[test]
    fn a_newer_format_is_told_apart_from_damage() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = sample(dir.path());
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[8] = 9;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read_manifest(&path).unwrap_err().code, "too_new");
    }

    #[test]
    fn entry_paths_cannot_escape_or_be_unportable() {
        for ok in [
            "kb.db",
            "raw/claude/projects/-Users-a-b/x.jsonl",
            "notes/codex/abc.json",
        ] {
            assert!(check_entry_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "raw/../../x",
            "raw//x",
            "raw/./x",
            "raw\\x",
            "C:/x",
            "other/x",
            "raw",
            "raw/CON",
            "raw/aux.txt",
            "raw/x ",
            "raw/x.",
            "raw/a|b",
            "raw/a\nb",
            "kb.db/x",
        ] {
            assert!(check_entry_path(bad).is_err(), "{bad:?}");
        }
        let dup = [
            FileEntry {
                path: "raw/A.jsonl".into(),
                size: 1,
                sha256: "0".repeat(64),
            },
            FileEntry {
                path: "raw/a.jsonl".into(),
                size: 1,
                sha256: "0".repeat(64),
            },
        ];
        assert!(check_manifest_paths(&dup).is_err());
    }

    #[test]
    fn a_shrinking_source_and_cancel_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let file = File::create(dir.path().join("y")).unwrap();
        let mut w = Writer::new(file).unwrap();
        let err = w
            .add("raw/a", 10, &mut &b"abc"[..], &Cancel::new(), &mut |_| {})
            .unwrap_err();
        assert!(err.message.contains("shorter"));
        let c = Cancel::new();
        c.cancel();
        assert_eq!(
            w.add("raw/b", 1, &mut &b"a"[..], &c, &mut |_| {})
                .unwrap_err()
                .code,
            "cancelled"
        );
    }
}
