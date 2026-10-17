//! Small operating-system adapters used by the cross-platform KB code.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::*;

use std::process::Command;

/// Configure a child process so a GUI build never flashes a console window.
pub fn no_window(command: &mut Command) {
    #[cfg(windows)]
    windows::no_window(command);
}

/// Configure a long-lived child so it can outlive the short sync process.
pub fn detached(command: &mut Command) {
    #[cfg(windows)]
    windows::detached(command);
    #[cfg(not(windows))]
    let _ = command;
}
