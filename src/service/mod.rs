//! `juicetop service install | uninstall | status`: runs the collector in the background with the
//! system's own service manager: systemd on Linux, launchd on macOS, Task Scheduler on Windows.
//! The macOS and Windows variants are untested.

use std::path::Path;

use anyhow::{Context, Result};

#[cfg(target_os = "macos")]
mod launchd;
#[cfg(windows)]
mod schtasks;
#[cfg(target_os = "linux")]
mod systemd;

#[cfg(target_os = "macos")]
pub use launchd::{install, status, uninstall};
#[cfg(windows)]
pub use schtasks::{install, status, uninstall};
#[cfg(target_os = "linux")]
pub use systemd::{install, status, uninstall};

/// What to run to set up the collector, shown when none is running.
#[cfg(unix)]
pub const INSTALL_HINT: &str = "sudo juicetop service install";
#[cfg(windows)]
pub const INSTALL_HINT: &str = "juicetop service install (as administrator)";

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn install(_user: bool) -> Result<()> {
    anyhow::bail!("no service manager support on this system; run `juicetop daemon` yourself")
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn uninstall(_user: bool, _purge: bool) -> Result<()> {
    install(false)
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn status(_user: bool) -> Result<()> {
    install(false)
}

/// Writes a file, creating its folder; `mode` applies on Unix.
#[allow(dead_code)]
fn write_file(path: &Path, content: &str, mode: u32) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

#[allow(dead_code)]
fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            println!("removed {}", path.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

#[allow(dead_code)]
fn remove_dir_if_exists(dir: Option<&Path>) -> Result<()> {
    let Some(dir) = dir else { return Ok(()) };
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {
            println!("removed {}", dir.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", dir.display())),
    }
}
