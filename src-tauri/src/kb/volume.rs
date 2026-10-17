//! Telling *which disk* a path lives on.
//!
//! A backup folder like `/Volumes/Backup1/AgentKB-backup` must never be
//! trusted by name: with the disk unplugged, `/Volumes/Backup1` can still exist
//! as a plain folder on the system disk, and writing there would silently
//! fill the wrong drive. So the disk is identified by its volume UUID, taken
//! from the mount point of whatever filesystem actually holds the path.
//!
//! The OS queries sit behind [`SystemQuery`] so the decision logic is tested
//! with fakes; [`MacQuery`] shells out to `df` and `diskutil`.

use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::time::Duration;

use serde::Serialize;

use super::fsutil;

#[cfg(not(windows))]
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
const NETWORK_FS: [&str; 6] = ["smbfs", "nfs", "afpfs", "webdav", "cifs", "ftp"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VolumeInfo {
    pub uuid: String,
    pub mount_point: PathBuf,
    pub name: Option<String>,
    pub internal: bool,
    pub fs_type: String,
    /// False for read-only volumes such as mounted installer images.
    pub writable: bool,
}

pub trait SystemQuery {
    /// Mount point of the filesystem holding `path` (which must exist).
    fn mount_point(&self, path: &Path) -> Result<PathBuf, String>;
    /// Describe the volume mounted at exactly `mount`.
    fn info(&self, mount: &Path) -> Result<VolumeInfo, String>;
}

/// Why a location cannot be used as the second backup.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Unsupported {
    Internal { name: Option<String> },
    Network { fs_type: String },
    Cloud,
}

/// Outcome of looking at a location.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Probe {
    /// The location sits on an external volume.
    External { volume: VolumeInfo },
    /// It sits on something we refuse to use as a second copy.
    Unsupported { reason: Unsupported },
    /// Nothing identifiable is mounted there (or the query failed).
    Unavailable { reason: String },
}

fn is_cloud_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.contains("/Library/CloudStorage/")
        || text.contains("/Mobile Documents/")
        || text.contains("/iCloud Drive")
}

/// Look at `path` (or its nearest existing ancestor) without creating anything.
pub fn probe(path: &Path, query: &dyn SystemQuery) -> Probe {
    if is_cloud_path(path) {
        return Probe::Unsupported {
            reason: Unsupported::Cloud,
        };
    }
    let Some(existing) = fsutil::nearest_existing(path) else {
        return Probe::Unavailable {
            reason: format!("{} does not exist", path.display()),
        };
    };
    let mount = match query.mount_point(&existing) {
        Ok(mount) => mount,
        Err(reason) => return Probe::Unavailable { reason },
    };
    let volume = match query.info(&mount) {
        Ok(volume) => volume,
        Err(reason) => return Probe::Unavailable { reason },
    };
    if NETWORK_FS.contains(&volume.fs_type.to_ascii_lowercase().as_str()) {
        return Probe::Unsupported {
            reason: Unsupported::Network {
                fs_type: volume.fs_type,
            },
        };
    }
    if volume.internal {
        return Probe::Unsupported {
            reason: Unsupported::Internal { name: volume.name },
        };
    }
    Probe::External { volume }
}

/// Whether the disk recorded in the settings is the one behind `path` right now.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Presence {
    /// Same UUID: safe to write.
    Connected {
        volume: VolumeInfo,
    },
    /// The recorded disk is not mounted. The path may still exist as an
    /// ordinary folder on another disk; nothing may be written to it.
    NotConnected {
        reason: String,
    },
    Unsupported {
        reason: Unsupported,
    },
}

pub fn presence(path: &Path, expected_uuid: &str, query: &dyn SystemQuery) -> Presence {
    match probe(path, query) {
        Probe::External { volume } if volume.uuid.eq_ignore_ascii_case(expected_uuid) => {
            Presence::Connected { volume }
        }
        Probe::External { volume } => Presence::NotConnected {
            reason: format!(
                "{} is on a different disk ({}), not the backup disk",
                path.display(),
                volume.name.as_deref().unwrap_or(&volume.uuid)
            ),
        },
        // An internal disk answering for the path is exactly the "unplugged,
        // but the folder still exists" case.
        Probe::Unsupported {
            reason: Unsupported::Internal { .. },
        } => Presence::NotConnected {
            reason: format!(
                "{} is on the system disk; the backup disk is not connected",
                path.display()
            ),
        },
        Probe::Unsupported { reason } => Presence::Unsupported { reason },
        Probe::Unavailable { reason } => Presence::NotConnected { reason },
    }
}

/// True when `path` and `mount` are on the same device right now. Checked
/// before each batch so a disk pulled mid-copy cannot redirect later writes.
#[cfg(unix)]
pub fn same_device(path: &Path, mount: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(path), std::fs::metadata(mount)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(windows)]
pub fn same_device(path: &Path, mount: &Path) -> bool {
    super::platform::same_volume(path, mount)
}

#[cfg(not(any(unix, windows)))]
pub fn same_device(_path: &Path, _mount: &Path) -> bool {
    true
}

/// External volumes currently mounted under `/Volumes`.
pub fn list_external(query: &dyn SystemQuery) -> Vec<VolumeInfo> {
    #[cfg(windows)]
    {
        let _ = query;
        super::platform::list_external()
    }
    #[cfg(not(windows))]
    {
        let Ok(entries) = std::fs::read_dir("/Volumes") else {
            return Vec::new();
        };
        let mut out: Vec<VolumeInfo> = Vec::new();
        for entry in entries.flatten() {
            // `/Volumes/Macintosh HD` is a symlink to `/`.
            if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                continue;
            }
            let path = entry.path();
            if let Probe::External { volume } = probe(&path, query) {
                // Only volumes mounted exactly here, not a folder on another disk.
                if volume.mount_point == path
                    && volume.writable
                    && !out.iter().any(|v| v.uuid == volume.uuid)
                {
                    out.push(volume);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

// ---------------------------------------------------------------------------
// The real thing
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
pub struct MacQuery;

#[cfg(windows)]
pub type MacQuery = super::platform::WindowsQuery;

#[cfg(windows)]
#[allow(non_upper_case_globals)]
pub const MacQuery: super::platform::WindowsQuery = super::platform::WindowsQuery;

#[cfg(windows)]
impl SystemQuery for super::platform::WindowsQuery {
    fn mount_point(&self, path: &Path) -> Result<PathBuf, String> {
        super::platform::volume_info(path).map(|v| v.mount_point)
    }

    fn info(&self, mount: &Path) -> Result<VolumeInfo, String> {
        super::platform::volume_info(mount)
    }
}

#[cfg(not(windows))]
fn run(program: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > QUERY_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} did not answer within {QUERY_TIMEOUT:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Mount point from `df -P` output: everything after the `NN%` capacity column
/// (the mount point itself may contain spaces).
pub fn parse_df_mount_point(output: &str) -> Option<PathBuf> {
    let line = output.lines().nth(1)?;
    let mut offset = 0;
    for token in line.split_whitespace() {
        let start = offset + line[offset..].find(token)?;
        offset = start + token.len();
        if token.ends_with('%') && token[..token.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            let rest = line[offset..].trim();
            // Other `NN%` columns (inode usage) may precede the mount point.
            if rest.starts_with('/') {
                return Some(PathBuf::from(rest));
            }
        }
    }
    None
}

pub fn parse_diskutil_info(mount: &Path, plist_bytes: &[u8]) -> Result<VolumeInfo, String> {
    let value = plist::Value::from_reader(std::io::Cursor::new(plist_bytes))
        .map_err(|e| format!("cannot read diskutil output: {e}"))?;
    let dict = value
        .as_dictionary()
        .ok_or("diskutil output is not a dictionary")?;
    let text = |key: &str| {
        dict.get(key)
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
    };
    let uuid = text("VolumeUUID")
        .or_else(|| text("DiskUUID"))
        .ok_or_else(|| format!("{} has no volume UUID", mount.display()))?;
    Ok(VolumeInfo {
        uuid,
        mount_point: mount.to_path_buf(),
        name: text("VolumeName").filter(|n| !n.is_empty()),
        // Missing means we cannot prove it is external: treat as internal.
        internal: dict
            .get("Internal")
            .and_then(plist::Value::as_boolean)
            .unwrap_or(true),
        fs_type: text("FilesystemType").unwrap_or_default(),
        // Missing means unknown: do not hide a disk over a missing key.
        writable: dict
            .get("WritableVolume")
            .and_then(plist::Value::as_boolean)
            .unwrap_or(true),
    })
}

#[cfg(not(windows))]
impl SystemQuery for MacQuery {
    fn mount_point(&self, path: &Path) -> Result<PathBuf, String> {
        let out = run("df", &["-P", &path.to_string_lossy()])?;
        parse_df_mount_point(&String::from_utf8_lossy(&out))
            .ok_or_else(|| format!("cannot find the disk holding {}", path.display()))
    }

    fn info(&self, mount: &Path) -> Result<VolumeInfo, String> {
        // `diskutil info` only answers for a mount point or device, never a
        // path inside a volume.
        let out = run("diskutil", &["info", "-plist", &mount.to_string_lossy()])?;
        parse_diskutil_info(mount, &out)
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::BTreeMap;

    /// Maps path prefixes to volumes, longest prefix wins (like a real mount table).
    #[derive(Default)]
    pub struct FakeQuery {
        pub volumes: BTreeMap<PathBuf, VolumeInfo>,
    }

    pub fn volume(uuid: &str, mount: &str, internal: bool, fs: &str) -> VolumeInfo {
        VolumeInfo {
            uuid: uuid.into(),
            mount_point: PathBuf::from(mount),
            name: Some(mount.rsplit('/').next().unwrap_or("").to_string()),
            internal,
            fs_type: fs.into(),
            writable: true,
        }
    }

    impl FakeQuery {
        pub fn with(volumes: impl IntoIterator<Item = VolumeInfo>) -> Self {
            Self {
                volumes: volumes
                    .into_iter()
                    .map(|v| (v.mount_point.clone(), v))
                    .collect(),
            }
        }
    }

    impl SystemQuery for FakeQuery {
        fn mount_point(&self, path: &Path) -> Result<PathBuf, String> {
            self.volumes
                .keys()
                .filter(|m| path.starts_with(m))
                .max_by_key(|m| m.as_os_str().len())
                .cloned()
                .ok_or_else(|| "no mount".to_string())
        }
        fn info(&self, mount: &Path) -> Result<VolumeInfo, String> {
            self.volumes
                .get(mount)
                .cloned()
                .ok_or_else(|| "no such volume".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    fn world(disk_plugged_in: bool) -> FakeQuery {
        let mut volumes = vec![volume("SYS-UUID", "/", true, "apfs")];
        if disk_plugged_in {
            volumes.push(volume("DISK-UUID", "/Volumes/Backup1", false, "exfat"));
        }
        FakeQuery::with(volumes)
    }

    // `probe` looks at the real filesystem for existence, so use a temp dir
    // that stands in for "/Volumes/Backup1" by being the fake mount point.
    fn disk_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    #[test]
    fn same_uuid_is_connected_and_the_nearest_existing_ancestor_is_used() {
        let (_g, mount) = disk_dir();
        let q = FakeQuery::with([
            volume("SYS-UUID", "/", true, "apfs"),
            VolumeInfo {
                mount_point: mount.clone(),
                ..volume("DISK-UUID", "/x", false, "exfat")
            },
        ]);
        // The backup folder does not exist yet; its parent (the mount) does.
        let target = mount.join("AgentKB-backup");
        assert!(matches!(
            presence(&target, "disk-uuid", &q),
            Presence::Connected { .. }
        ));
        assert!(!target.exists(), "probing must not create anything");
    }

    #[test]
    fn an_unplugged_disk_whose_folder_still_exists_on_the_system_disk_is_not_connected() {
        // Path exists (a plain folder), but the filesystem answering for it is
        // the system disk.
        let (_g, folder) = disk_dir();
        let q = FakeQuery::with([VolumeInfo {
            mount_point: PathBuf::from("/"),
            ..volume("SYS-UUID", "/", true, "apfs")
        }]);
        let result = presence(&folder, "DISK-UUID", &q);
        assert!(
            matches!(result, Presence::NotConnected { .. }),
            "{result:?}"
        );
    }

    #[test]
    fn a_different_external_disk_with_the_same_folder_is_not_the_backup_disk() {
        let (_g, mount) = disk_dir();
        let q = FakeQuery::with([VolumeInfo {
            mount_point: mount.clone(),
            ..volume("OTHER-UUID", "/x", false, "exfat")
        }]);
        assert!(matches!(
            presence(&mount, "DISK-UUID", &q),
            Presence::NotConnected { .. }
        ));
    }

    #[test]
    fn internal_network_and_cloud_locations_are_refused_when_choosing() {
        let (_g, dir) = disk_dir();
        let internal = FakeQuery::with([VolumeInfo {
            mount_point: dir.clone(),
            ..volume("U", "/x", true, "apfs")
        }]);
        assert!(matches!(
            probe(&dir, &internal),
            Probe::Unsupported {
                reason: Unsupported::Internal { .. }
            }
        ));
        let network = FakeQuery::with([VolumeInfo {
            mount_point: dir.clone(),
            ..volume("U", "/x", false, "smbfs")
        }]);
        assert!(matches!(
            probe(&dir, &network),
            Probe::Unsupported {
                reason: Unsupported::Network { .. }
            }
        ));
        assert!(matches!(
            probe(
                Path::new("/Users/x/Library/CloudStorage/GoogleDrive/a"),
                &network
            ),
            Probe::Unsupported {
                reason: Unsupported::Cloud
            }
        ));
    }

    #[test]
    fn a_path_that_does_not_exist_anywhere_is_unavailable() {
        let q = world(false);
        assert!(matches!(
            probe(Path::new("/definitely/not/here/at/all"), &q),
            Probe::Unavailable { .. } | Probe::Unsupported { .. }
        ));
    }

    #[test]
    fn df_output_parsing_handles_spaces_in_the_mount_point() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                   /dev/disk9s1 20480 1000 19480 5% /Volumes/My Backup Disk\n";
        assert_eq!(
            parse_df_mount_point(out),
            Some(PathBuf::from("/Volumes/My Backup Disk"))
        );
        let simple =
            "Filesystem 512-blocks Used Available Capacity iused ifree %iused Mounted on\n\
                      /dev/disk3s5 100 1 2 3% 1 2 3% /System/Volumes/Data\n";
        assert_eq!(
            parse_df_mount_point(simple),
            Some(PathBuf::from("/System/Volumes/Data"))
        );
        assert_eq!(parse_df_mount_point("garbage"), None);
    }

    #[test]
    fn diskutil_plist_is_parsed() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>VolumeUUID</key><string>D62F7387-D951-4AF4-9536-94103B9E94E9</string>
<key>VolumeName</key><string>KBProbe</string>
<key>Internal</key><false/>
<key>FilesystemType</key><string>apfs</string>
</dict></plist>"#;
        let info = parse_diskutil_info(Path::new("/Volumes/KBProbe"), xml).unwrap();
        assert_eq!(info.uuid, "D62F7387-D951-4AF4-9536-94103B9E94E9");
        assert!(!info.internal);
        assert_eq!(info.name.as_deref(), Some("KBProbe"));
        assert!(
            info.writable,
            "a missing WritableVolume key must not hide a disk"
        );
        assert!(parse_diskutil_info(Path::new("/x"), b"nope").is_err());
    }

    #[test]
    fn a_read_only_installer_image_is_not_writable() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>VolumeUUID</key><string>AAAA</string>
<key>VolumeName</key><string>AweSun Installer</string>
<key>Internal</key><false/>
<key>WritableVolume</key><false/>
<key>FilesystemType</key><string>hfs</string>
</dict></plist>"#;
        let info = parse_diskutil_info(Path::new("/Volumes/AweSun Installer"), xml).unwrap();
        assert!(!info.writable);
    }
}
