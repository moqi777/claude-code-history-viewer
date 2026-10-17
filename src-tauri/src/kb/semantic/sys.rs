//! Operating system touches of the background builder: priority and memory.

/// Make this process polite. `AGENTKB_PRIORITY` (`none`, `nice`, `utility`, `background`)
/// overrides the default, which was chosen by measurement (design review, item 2):
/// see `docs/agent-kb/P6-report.md`. Failures are ignored: it only affects speed.
#[allow(unsafe_code)] // plain libc calls
pub fn lower_priority() {
    let mode = std::env::var("AGENTKB_PRIORITY").unwrap_or_else(|_| DEFAULT_PRIORITY.to_string());
    #[cfg(unix)]
    // SAFETY: plain libc calls with constant arguments.
    unsafe {
        match mode.as_str() {
            "none" => {}
            "background" => {
                libc::setpriority(libc::PRIO_PROCESS, 0, 19);
                #[cfg(target_os = "macos")]
                libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG);
            }
            "utility" => {
                libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                #[cfg(target_os = "macos")]
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
            }
            _ => {
                libc::setpriority(libc::PRIO_PROCESS, 0, 19);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
}

const DEFAULT_PRIORITY: &str = "nice";

/// Memory the system can give without swapping, in bytes: free, inactive,
/// speculative and purgeable pages on macOS (free pages alone are almost
/// always near zero there), `MemAvailable` on Linux. `None` when unknown.
pub fn available_memory() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/vm_stat")
            .output()
            .ok()?;
        return parse_vm_stat(&String::from_utf8_lossy(&out.stdout));
    }
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb: u64 = text
            .lines()
            .find(|l| l.starts_with("MemAvailable:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        return Some(kb * 1024);
    }
    #[allow(unreachable_code)]
    None
}

pub fn parse_vm_stat(text: &str) -> Option<u64> {
    let page: u64 = text
        .lines()
        .next()?
        .split("page size of")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |label: &str| -> u64 {
        text.lines()
            .find(|l| l.starts_with(label))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    let total = pages("Pages free")
        + pages("Pages inactive")
        + pages("Pages speculative")
        + pages("Pages purgeable");
    Some(total * page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_inactive_speculative_and_purgeable_pages() {
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
Pages free:                               10000.\n\
Pages active:                            500000.\n\
Pages inactive:                          200000.\n\
Pages speculative:                         5000.\n\
Pages purgeable:                          15000.\n";
        assert_eq!(
            parse_vm_stat(text),
            Some((10_000 + 200_000 + 5_000 + 15_000) * 16384)
        );
        assert_eq!(parse_vm_stat("garbage"), None);
    }
}
