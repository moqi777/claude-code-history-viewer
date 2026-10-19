//! Pieces shared by export and import: the error type (a stable `code` the
//! front end can translate, plus an English message), cancellation, and the
//! progress meter.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::progress::{Progress, ProgressSink};

/// A failure the user may have to act on. `code` is stable; the message is
/// for logs and the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferError {
    pub code: &'static str,
    pub message: String,
}

impl TransferError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The file is damaged, truncated or not an export at all.
    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new("corrupt", message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new("io", message)
    }

    pub fn cancelled() -> Self {
        Self::new("cancelled", "cancelled")
    }
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for TransferError {}

impl From<TransferError> for String {
    fn from(e: TransferError) -> Self {
        e.to_string()
    }
}

/// Internal helper errors (`String`) are I/O or database trouble.
impl From<String> for TransferError {
    fn from(message: String) -> Self {
        Self::io(message)
    }
}

pub type TResult<T> = Result<T, TransferError>;

#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> TResult<()> {
        if self.is_cancelled() {
            Err(TransferError::cancelled())
        } else {
            Ok(())
        }
    }
}

/// Counts files and bytes through one phase and reports at a sensible rate.
pub struct Meter<'a> {
    sink: &'a ProgressSink,
    op: &'static str,
    phase: &'static str,
    total_files: u64,
    total_bytes: u64,
    files: u64,
    bytes: u64,
    last_emit: u64,
}

const EMIT_EVERY_BYTES: u64 = 8 * 1024 * 1024;

impl<'a> Meter<'a> {
    pub fn new(sink: &'a ProgressSink, op: &'static str) -> Self {
        Self {
            sink,
            op,
            phase: "",
            total_files: 0,
            total_bytes: 0,
            files: 0,
            bytes: 0,
            last_emit: 0,
        }
    }

    pub fn phase(&mut self, phase: &'static str, total_files: u64, total_bytes: u64) {
        self.phase = phase;
        self.total_files = total_files;
        self.total_bytes = total_bytes;
        self.files = 0;
        self.bytes = 0;
        self.last_emit = 0;
        self.emit();
    }

    pub fn add_bytes(&mut self, n: u64) {
        self.bytes += n;
        if self.bytes - self.last_emit >= EMIT_EVERY_BYTES {
            self.emit();
        }
    }

    pub fn file_done(&mut self) {
        self.files += 1;
        if self.files == self.total_files {
            self.emit();
        }
    }

    pub fn emit(&mut self) {
        self.last_emit = self.bytes;
        self.sink.emit(&Progress {
            op: self.op.into(),
            phase: self.phase.into(),
            done: self.files,
            total: self.total_files,
            bytes_done: self.bytes,
            bytes_total: self.total_bytes,
        });
    }
}

/// Removes a directory tree when dropped, whatever way the work ends.
pub struct RemoveOnDrop(pub std::path::PathBuf);

impl RemoveOnDrop {
    /// Keep the directory: the work it guards has succeeded.
    pub fn keep(self) {
        std::mem::forget(self);
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The computer's name, for "came from another device (X)".
pub fn device_name() -> String {
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(name) = std::env::var(var) {
            if !name.trim().is_empty() {
                return name.trim().to_string();
            }
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown device".to_string())
}

/// Work folders of an earlier export/import that was killed, older than a
/// day, inside the data directory.
pub fn clean_stale_work_dirs(data_root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(data_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with(".export-")
            || name.starts_with(".import-")
            || name.starts_with(".restore-"))
        {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(24 * 3600));
        if old && entry.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A note file under `notes/.conflict-*` is a spare copy, not a note.
pub fn is_conflict_path(rel: &str) -> bool {
    rel.split('/').any(|part| part.starts_with(".conflict-"))
}

pub fn now_stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Lowercase hex of a digest.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_carry_a_code_and_cancel_is_shared() {
        let e = TransferError::corrupt("bad byte");
        assert_eq!(e.to_string(), "corrupt: bad byte");
        assert_eq!(String::from(e), "corrupt: bad byte");
        let c = Cancel::new();
        let d = c.clone();
        assert!(d.check().is_ok());
        c.cancel();
        assert_eq!(d.check().unwrap_err().code, "cancelled");
    }

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }
}
