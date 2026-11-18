//! macOS: the collector as a launchd daemon.
//!
//! macOS gives CPU and GPU energy only to root (through `powermetrics`), so there is no user
//! service: a per-user agent would have no sensor to read.

use std::path::Path;
use std::process::Command;

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
    let target = format!("system/{LABEL}");
    // Stop a running copy before its binary is replaced.
    let _ = Command::new("launchctl")
        .args(["bootout", &target])
        .status();
    if exe != Path::new(SYSTEM_BIN) {
        std::fs::create_dir_all("/usr/local/bin")?;
        let tmp = format!("{SYSTEM_BIN}.new");
        std::fs::copy(&exe, &tmp).with_context(|| format!("copying {} to {tmp}", exe.display()))?;
        std::fs::rename(&tmp, SYSTEM_BIN)?;
        println!("installed {SYSTEM_BIN}");
    }
    write_file(Path::new(PLIST_PATH), PLIST, 0o644)?;
    launchctl(&["bootstrap", "system", PLIST_PATH])?;
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
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("system/{LABEL}")])
        .status();
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
