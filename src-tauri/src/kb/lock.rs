//! Cross-process "only one sync at a time" lock.
//!
//! An OS file lock (`flock` / `LockFileEx`) is released by the kernel when the
//! holder dies, so a crash can never leave a stale lock behind. The pid and
//! start time written into the file are diagnostics only and never consulted
//! to decide who owns the lock.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};

const RETRY_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct SyncLock {
    file: File,
    info_path: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockInfo {
    pub pid: u32,
    pub started_at: String,
}

impl SyncLock {
    /// Try to take the lock, retrying every 100 ms for up to `wait`.
    /// `Ok(None)` means somebody else holds it.
    pub fn acquire(path: &Path, wait: Duration) -> Result<Option<Self>, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0x0000_0001 | 0x0000_0002 | 0x0000_0004);
        }
        let file = options
            .open(path)
            .map_err(|e| format!("cannot open lock file {}: {e}", path.display()))?;

        let deadline = Instant::now() + wait;
        loop {
            // Fully qualified: newer std has an inherent `File::try_lock` with a
            // different error type, and the MSRV predates it.
            match FileExt::try_lock(&file) {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(TryLockError::Error(e)) => {
                    return Err(format!("cannot lock {}: {e}", path.display()))
                }
            }
        }

        let info_path = path
            .file_name()
            .map(|name| path.with_file_name(format!(".{}.info", name.to_string_lossy())))
            .unwrap_or_else(|| path.with_extension("info"));
        let mut lock = Self { file, info_path };
        lock.write_info();
        Ok(Some(lock))
    }

    fn write_info(&mut self) {
        let info = LockInfo {
            pid: std::process::id(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Ok(text) = serde_json::to_string(&info) {
            // Diagnostics only: ignore failures.
            let _ = self.file.set_len(0);
            let _ = self.file.seek(SeekFrom::Start(0));
            let _ = self.file.write_all(text.as_bytes());
            let _ = self.file.flush();
            // Windows byte-range locks can prevent a second handle from
            // opening the lock file. Keep the diagnostic in a sidecar so
            // status reads remain possible while the lock is held.
            let _ = std::fs::write(&self.info_path, text);
        }
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        // The note is diagnostics for whoever looks while the lock is held;
        // an empty file says "nobody".
        let _ = self.file.set_len(0);
        let _ = std::fs::remove_file(&self.info_path);
        let _ = FileExt::unlock(&self.file);
    }
}

/// Whether the process named in the lock file is alive. Unlike taking the
/// lock to see whether it is free, asking never makes a run that starts at
/// the same moment find the lock busy. A holder that crashed leaves its pid
/// behind; a dead pid is "not held".
pub fn held_by_live_process(path: &Path) -> Option<LockInfo> {
    let info = read_info(path)?;
    process_alive(info.pid).then_some(info)
}

#[cfg(unix)]
#[allow(unsafe_code)] // `kill(pid, 0)` only checks that the process exists
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.split_whitespace()
            .any(|field| field == pid.to_string())
    })
}

#[cfg(not(any(unix, windows)))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// Who wrote the lock file last (may be stale; never use it for ownership).
pub fn read_info(path: &Path) -> Option<LockInfo> {
    let mut text = String::new();
    let sidecar = path
        .file_name()
        .map(|name| path.with_file_name(format!(".{}.info", name.to_string_lossy())))
        .unwrap_or_else(|| path.with_extension("info"));
    if let Ok(value) = std::fs::read_to_string(&sidecar) {
        if let Ok(info) = serde_json::from_str(&value) {
            return Some(info);
        }
    }
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001 | 0x0000_0002 | 0x0000_0004)
            .open(path)
            .ok()?
    };
    #[cfg(not(windows))]
    let file = File::open(path).ok()?;
    file.take(64 * 1024).read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_is_blocked_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kb.lock");

        let first = SyncLock::acquire(&path, Duration::ZERO).unwrap();
        assert!(first.is_some());
        assert!(SyncLock::acquire(&path, Duration::ZERO).unwrap().is_none());

        drop(first);
        assert!(SyncLock::acquire(&path, Duration::ZERO).unwrap().is_some());
    }

    #[test]
    fn waiting_acquire_succeeds_once_the_holder_releases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kb.lock");
        let holder = SyncLock::acquire(&path, Duration::ZERO).unwrap().unwrap();

        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(holder);
        });
        let started = Instant::now();
        let got = SyncLock::acquire(&path, Duration::from_secs(3)).unwrap();
        releaser.join().unwrap();
        assert!(got.is_some());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn lock_file_records_the_holder_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kb.lock");
        let _held = SyncLock::acquire(&path, Duration::ZERO).unwrap().unwrap();
        assert_eq!(read_info(&path).unwrap().pid, std::process::id());
    }

    #[test]
    fn dropping_the_lock_clears_the_note_and_a_dead_pid_is_not_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("digest.lock");
        let held = SyncLock::acquire(&path, Duration::ZERO).unwrap().unwrap();
        assert_eq!(held_by_live_process(&path).unwrap().pid, std::process::id());
        drop(held);
        assert!(held_by_live_process(&path).is_none());
        std::fs::write(
            &path,
            r#"{"pid":4194000,"started_at":"2020-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(held_by_live_process(&path).is_none(), "nobody has that pid");
    }

    #[test]
    fn stale_info_in_an_unlocked_file_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kb.lock");
        std::fs::write(
            &path,
            r#"{"pid":999999,"started_at":"2020-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(SyncLock::acquire(&path, Duration::ZERO).unwrap().is_some());
    }
}
