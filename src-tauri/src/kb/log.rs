//! Plain-file sync log: `<data>/logs/sync-YYYY-MM-DD.log`, kept for 14 days.
//! Headless runs have no `log` backend, so the KB writes its own file.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const RETENTION_DAYS: u64 = 14;
const FILE_PREFIX: &str = "sync-";
const FILE_SUFFIX: &str = ".log";

pub struct SyncLog {
    path: PathBuf,
}

impl SyncLog {
    pub fn open(logs_dir: &Path) -> Self {
        let _ = fs::create_dir_all(logs_dir);
        let name = format!(
            "{FILE_PREFIX}{}{FILE_SUFFIX}",
            chrono::Local::now().format("%Y-%m-%d")
        );
        Self {
            path: logs_dir.join(name),
        }
    }

    pub fn line(&self, message: &str) {
        let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z");
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "[{stamp}] {message}");
        }
    }
}

/// Delete `sync-*.log` files older than the retention window. Returns how many
/// were removed.
pub fn prune(logs_dir: &Path) -> usize {
    prune_older_than(logs_dir, Duration::from_secs(RETENTION_DAYS * 86_400))
}

fn prune_older_than(logs_dir: &Path, max_age: Duration) -> usize {
    let Ok(entries) = fs::read_dir(logs_dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX)) {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok());
        if age.is_some_and(|age| age > max_age) && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_lines_and_prunes_only_old_sync_logs() {
        let dir = tempfile::tempdir().unwrap();
        let log = SyncLog::open(dir.path());
        log.line("hello");
        log.line("again");
        let text = fs::read_to_string(&log.path).unwrap();
        assert_eq!(text.lines().count(), 2);

        // Nothing is older than 14 days yet.
        assert_eq!(prune(dir.path()), 0);
        // With a zero retention window the sync log goes, unrelated files stay.
        fs::write(dir.path().join("launchd.out.log"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(prune_older_than(dir.path(), Duration::from_millis(1)), 1);
        assert!(dir.path().join("launchd.out.log").exists());
    }
}
