//! macOS: the collector as a launchd daemon.
//!
//! macOS gives CPU and GPU energy only to root (through `powermetrics`), so there is no user
//! service: a per-user agent would have no sensor to read.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::{remove_dir_if_exists, remove_if_exists, write_file};
use crate::live::{DataPaths, is_root};

pub const LABEL: &str = "io.github.cookedapps.juicetop";
pub const PLIST: &str = include_str!("../../packaging/launchd/io.github.cookedapps.juicetop.plist");

const SYSTEM_BIN: &str = "/usr/local/bin/juicetop";
const PLIST_PATH: &str = "/Library/LaunchDaemons/io.github.cookedapps.juicetop.plist";
const SUDO_HINT: &str = "run it with sudo: sudo \"$(command -v juicetop)\" service install";

fn launchctl(args: &[&str]) -> Result<()> {
    let status = Command::new("launchctl")
        .args(args)
        .status()
        .context("running launchctl")?;
    if !status.success() {
        bail!("launchctl {} failed", args.join(" "));
    }
    Ok(())
}

/// True while launchd still knows the job.
fn loaded() -> bool {
    Command::new("launchctl")
        .args(["print", &format!("system/{LABEL}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Stops and unloads the job. `bootout` returns before launchd has finished, and a `bootstrap`
/// in that window fails with "Input/output error", so this waits until the job is gone.
fn unload() {
    if !loaded() {
        return;
    }
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("system/{LABEL}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    for _ in 0..50 {
        if !loaded() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Loads the job, retrying while launchd is still busy with the previous one.
fn load() -> Result<()> {
    let mut last = Ok(());
    for _ in 0..5 {
        last = launchctl(&["bootstrap", "system", PLIST_PATH]);
        if last.is_ok() || loaded() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    last
}

pub fn install(user: bool) -> Result<()> {
    if user {
        bail!(
            "macOS reports CPU and GPU energy only to root, so a user service would measure \
             nothing; {SUDO_HINT}"
        );
    }
    if !is_root() {
        bail!("installing the system service needs root; {SUDO_HINT}");
    }
    let exe = std::env::current_exe().context("finding the juicetop binary")?;
    // Stop a running copy before its binary is replaced.
    unload();
    if exe != Path::new(SYSTEM_BIN) {
        std::fs::create_dir_all("/usr/local/bin")?;
        let tmp = format!("{SYSTEM_BIN}.new");
        std::fs::copy(&exe, &tmp).with_context(|| format!("copying {} to {tmp}", exe.display()))?;
        std::fs::rename(&tmp, SYSTEM_BIN)?;
        println!("installed {SYSTEM_BIN}");
    }
    write_file(Path::new(PLIST_PATH), PLIST, 0o644)?;
    load()?;
    println!("installed {PLIST_PATH}");
    println!("the collector is running and starts at boot (log: /var/log/juicetop.log)");
    println!("data: {}", DataPaths::system().db.display());
    println!("open the dashboard with: juicetop");
    Ok(())
}

pub fn uninstall(user: bool, purge: bool) -> Result<()> {
    if user {
        bail!("macOS has no user service; remove the system service with sudo");
    }
    if !is_root() {
        bail!(
            "removing the system service needs root: sudo \"$(command -v juicetop)\" service uninstall"
        );
    }
    unload();
    remove_if_exists(Path::new(PLIST_PATH))?;
    remove_if_exists(Path::new(SYSTEM_BIN))?;
    if purge {
        let paths = DataPaths::system();
        remove_dir_if_exists(paths.db.parent())?;
        remove_dir_if_exists(paths.live.parent())?;
    } else {
        println!("kept the recorded data; add --purge to delete it");
    }
    Ok(())
}

pub fn status(_user: bool) -> Result<()> {
    Command::new("launchctl")
        .args(["print", &format!("system/{LABEL}")])
        .status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_the_installed_binary() {
        assert!(PLIST.contains(LABEL));
        assert!(PLIST.contains(&format!("<string>{SYSTEM_BIN}</string>")));
    }
}
