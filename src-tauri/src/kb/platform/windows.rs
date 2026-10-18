//! Windows APIs kept in one file so the rest of `AgentKB` remains portable.
#![allow(unsafe_code)]

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::volume::VolumeInfo;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const STILL_ACTIVE: u32 = 259;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;

/// NSIS records the actual directory, including a user-selected drive/path.
pub fn installed_executable() -> Option<PathBuf> {
    use winreg::{
        enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WOW64_64KEY},
        RegKey,
    };
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall\AgentKB",
            KEY_READ | KEY_WOW64_64KEY,
        )
        .ok()?;
    let name: String = key.get_value("DisplayName").ok()?;
    let binary: String = key.get_value("MainBinaryName").ok()?;
    if name != "AgentKB" || binary != "AgentKB.exe" {
        return None;
    }
    let location: String = key.get_value("InstallLocation").ok()?;
    let dir = PathBuf::from(location.trim_matches('"'));
    dir.is_absolute().then(|| dir.join(binary))
}

#[link(name = "secur32")]
unsafe extern "system" {
    fn GetUserNameExW(format: u32, buffer: *mut u16, size: *mut u32) -> u8;
}

/// Windows console tools emit the OEM code page even when our own CLI is UTF-8.
pub fn console_text(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.into();
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    unsafe {
        let needed = MultiByteToWideChar(1, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
        if needed <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut output = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(1, 0, bytes.as_ptr(), len, output.as_mut_ptr(), needed);
        String::from_utf16_lossy(&output[..written.max(0) as usize])
    }
}

pub fn current_user() -> Result<String, String> {
    let mut buffer = vec![0u16; 1024];
    let mut size = buffer.len() as u32;
    if unsafe { GetUserNameExW(2, buffer.as_mut_ptr(), &mut size) } == 0 {
        return Err(format!(
            "cannot determine current user: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(String::from_utf16_lossy(&buffer[..size as usize]))
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        input: *const u8,
        input_len: i32,
        output: *mut u16,
        output_len: i32,
    ) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn GetExitCodeProcess(process: *mut c_void, code: *mut u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn SetPriorityClass(process: *mut c_void, class: u32) -> i32;
    fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
    fn GetVolumeInformationW(
        root: *const u16,
        name: *mut u16,
        name_len: u32,
        serial: *mut u32,
        max_component: *mut u32,
        flags: *mut u32,
        fs_name: *mut u16,
        fs_name_len: u32,
    ) -> i32;
    fn GetVolumeNameForVolumeMountPointW(
        mount: *const u16,
        volume: *mut u16,
        volume_len: u32,
    ) -> i32;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetLogicalDriveStringsW(len: u32, buffer: *mut u16) -> u32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn DeviceIoControl(
        handle: *mut c_void,
        code: u32,
        input: *const c_void,
        input_len: u32,
        output: *mut c_void,
        output_len: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

/// `StorageDeviceProperty` / `PropertyStandardQuery`. No write access is requested.
fn storage_bus(root: &Path) -> Option<u32> {
    let drive = root.to_str()?.trim_end_matches('\\');
    if drive.len() != 2 || !drive.ends_with(':') {
        return None;
    }
    let device = wide(&OsString::from(format!(r"\\.\{drive}")));
    unsafe {
        let handle = CreateFileW(
            device.as_ptr(),
            0,
            3,
            std::ptr::null(),
            3,
            0,
            std::ptr::null_mut(),
        );
        if handle as isize == -1 {
            return None;
        }
        let query = [0u32; 3];
        let mut descriptor = [0u8; 1024];
        let mut returned = 0;
        let ok = DeviceIoControl(
            handle,
            0x002D_1400,
            query.as_ptr().cast(),
            12,
            descriptor.as_mut_ptr().cast(),
            descriptor.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        ) != 0;
        let _ = CloseHandle(handle);
        // STORAGE_DEVICE_DESCRIPTOR.BusType follows the four string offsets.
        (ok && returned >= 32).then(|| u32::from_le_bytes(descriptor[28..32].try_into().unwrap()))
    }
}

fn is_external(root: &Path, system_drive: &str, drive_type: u32, bus: Option<u32>) -> bool {
    !root
        .to_string_lossy()
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(system_drive.trim_end_matches('\\'))
        && matches!(drive_type, 2 | 3)
        && matches!(bus, Some(4 | 7 | 12 | 13)) // 1394, USB, SD, MMC
}

fn external_drive(root: &Path, drive_type: u32) -> bool {
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    is_external(root, &system, drive_type, storage_bus(root))
}

#[repr(C)]
struct MemoryStatus {
    length: u32,
    memory_load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page: u64,
    avail_page: u64,
    total_virtual: u64,
    avail_virtual: u64,
    avail_extended: u64,
}

pub fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

pub fn detached(command: &mut Command) {
    no_window(command);
    // DETACHED_PROCESS keeps a scheduled GUI child independent of the console
    // (the process still inherits the redirected log handles).
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW | 0x0000_0008);
}

pub fn process_alive(pid: u32) -> bool {
    #[allow(unsafe_code)]
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        let _ = CloseHandle(handle);
        ok && code == STILL_ACTIVE
    }
}

pub fn available_memory() -> Option<u64> {
    #[allow(unsafe_code)]
    unsafe {
        let mut status = MemoryStatus {
            length: std::mem::size_of::<MemoryStatus>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page: 0,
            avail_page: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended: 0,
        };
        (GlobalMemoryStatusEx(&mut status) != 0).then_some(status.avail_phys)
    }
}

pub fn lower_priority(mode: &str) {
    let class = match mode {
        "none" => return,
        "background" => IDLE_PRIORITY_CLASS,
        _ => BELOW_NORMAL_PRIORITY_CLASS,
    };
    #[allow(unsafe_code)]
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), class);
    }
}

fn wide(text: &OsString) -> Vec<u16> {
    text.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn root_for(path: &Path) -> Option<PathBuf> {
    let text = path.to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\") {
        let mut parts = stripped.split('\\');
        let server = parts.next()?;
        let share = parts.next()?;
        return Some(PathBuf::from(format!(r"\\{server}\{share}\")));
    }
    let mut chars = text.chars();
    let drive = chars.next()?;
    (chars.next() == Some(':')).then(|| PathBuf::from(format!("{drive}:\\")))
}

fn volume_guid(root: &Path) -> Option<String> {
    let mount = wide(&root.as_os_str().to_os_string());
    let mut out = vec![0u16; 128];
    #[allow(unsafe_code)]
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), out.as_mut_ptr(), out.len() as u32) != 0
    };
    if !ok {
        return None;
    }
    let text =
        String::from_utf16_lossy(&out[..out.iter().position(|c| *c == 0).unwrap_or(out.len())]);
    Some(text.trim_matches('\\').to_string())
}

pub fn volume_info(mount: &Path) -> Result<VolumeInfo, String> {
    let root =
        root_for(mount).ok_or_else(|| format!("cannot determine drive for {}", mount.display()))?;
    let root_w = wide(&root.as_os_str().to_os_string());
    let mut name = vec![0u16; 256];
    let mut fs = vec![0u16; 256];
    let mut serial = 0;
    let mut max_component = 0;
    let mut flags = 0;
    #[allow(unsafe_code)]
    let ok = unsafe {
        GetVolumeInformationW(
            root_w.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            &mut serial,
            &mut max_component,
            &mut flags,
            fs.as_mut_ptr(),
            fs.len() as u32,
        ) != 0
    };
    if !ok {
        return Err(format!("cannot inspect {}", root.display()));
    }
    let text = |v: &[u16]| {
        String::from_utf16_lossy(&v[..v.iter().position(|c| *c == 0).unwrap_or(v.len())])
    };
    let drive_type = unsafe { GetDriveTypeW(root_w.as_ptr()) };
    Ok(VolumeInfo {
        uuid: volume_guid(&root).unwrap_or_else(|| format!("SERIAL-{serial:08X}")),
        internal: !external_drive(&root, drive_type),
        mount_point: root,
        name: Some(text(&name)).filter(|s| !s.is_empty()),
        fs_type: text(&fs),
        writable: flags & 0x0008_0000 == 0,
    })
}

pub fn list_external() -> Vec<VolumeInfo> {
    let mut buffer = vec![0u16; 512];
    #[allow(unsafe_code)]
    let len = unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) } as usize;
    let mut out = Vec::new();
    for part in buffer[..len.min(buffer.len())]
        .split(|c| *c == 0)
        .filter(|p| !p.is_empty())
    {
        let root = PathBuf::from(String::from_utf16_lossy(part));
        let root_w = wide(&root.as_os_str().to_os_string());
        #[allow(unsafe_code)]
        let kind = unsafe { GetDriveTypeW(root_w.as_ptr()) };
        if external_drive(&root, kind) {
            if let Ok(info) = volume_info(&root) {
                if info.writable {
                    out.push(info);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn same_volume(path: &Path, mount: &Path) -> bool {
    match (volume_info(path), volume_info(mount)) {
        (Ok(a), Ok(b)) => a.uuid == b.uuid,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_bus_includes_fixed_usb_disks_and_excludes_system_disk() {
        for bus in [4, 7, 12, 13] {
            for kind in [2, 3] {
                assert!(is_external(Path::new(r"E:\"), "C:", kind, Some(bus)));
                assert!(!is_external(Path::new(r"c:\"), "C:", kind, Some(bus)));
            }
        }
        for bus in [None, Some(0), Some(3), Some(11), Some(17)] {
            assert!(!is_external(Path::new(r"E:\"), "C:", 3, bus));
        }
        assert!(!is_external(Path::new(r"E:\"), "C:", 4, Some(7)));
    }

    #[test]
    fn unknown_volumes_are_not_assumed_to_be_the_same_disk() {
        assert!(!same_volume(
            Path::new("not-a-drive"),
            Path::new("also-unknown")
        ));
    }
}
