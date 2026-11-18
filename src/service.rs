//! `juicetop service install | uninstall | status`: sets up the collector as a systemd service.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::live::{DataPaths, is_root};

pub const SYSTEM_UNIT: &str = include_str!("../packaging/systemd/juicetop.service");
pub const USER_UNIT: &str = include_str!("../packaging/systemd/juicetop-user.service");

const SYSTEM_BIN: &str = "/usr/local/bin/juicetop";
const SYSTEM_UNIT_PATH: &str = "/etc/systemd/system/juicetop.service";
const USER_UNIT_EXEC: &str = "%h/.cargo/bin/juicetop";
const SUDO_HINT: &str = "run it with sudo: sudo \"$(command -v juicetop)\" service install";

fn user_unit_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("no config directory")?
        .join("systemd/user/juicetop.service"))
}

fn systemctl(user: bool, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    let status = cmd
        .args(args)
        .status()
        .context("running systemctl (is this a systemd system?)")?;
    if !status.success() {
        bail!("systemctl {} failed", args.join(" "));
    }
    Ok(())
}

fn write_file(path: &Path, content: &str, mode: u32) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// The user unit with `ExecStart` pointing at this binary.
pub fn user_unit_for(exe: &Path) -> String {
    USER_UNIT.replace(USER_UNIT_EXEC, &exe.display().to_string())
}

pub fn install(user: bool) -> Result<()> {
    let exe = std::env::current_exe().context("finding the juicetop binary")?;
    if user {
        if is_root() {
            bail!("--user installs a service for your own account; run it without sudo");
        }
        let path = user_unit_path()?;
        write_file(&path, &user_unit_for(&exe), 0o644)?;
        systemctl(true, &["daemon-reload"])?;
        systemctl(true, &["enable", "juicetop"])?;
        systemctl(true, &["restart", "juicetop"])?;
        println!("installed {}", path.display());
        println!("the collector now runs as your user and starts with your session");
        println!("data: {}", DataPaths::user().db.display());
    } else {
        if !is_root() {
            bail!("installing the system service needs root; {SUDO_HINT}");
        }
        // The service is sandboxed away from /home, so the binary must live outside it.
        if exe != Path::new(SYSTEM_BIN) {
            let tmp = format!("{SYSTEM_BIN}.new");
            std::fs::copy(&exe, &tmp)
                .with_context(|| format!("copying {} to {tmp}", exe.display()))?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            std::fs::rename(&tmp, SYSTEM_BIN)?;
            println!("installed {SYSTEM_BIN}");
        }
        write_file(Path::new(SYSTEM_UNIT_PATH), SYSTEM_UNIT, 0o644)?;
        systemctl(false, &["daemon-reload"])?;
        systemctl(false, &["enable", "juicetop"])?;
        systemctl(false, &["restart", "juicetop"])?;
        println!("installed {SYSTEM_UNIT_PATH}");
        println!("the collector is running and starts at boot");
        println!("data: {}", DataPaths::system().db.display());
    }
    println!("open the dashboard with: juicetop");
    Ok(())
}

pub fn uninstall(user: bool, purge: bool) -> Result<()> {
    if user {
        let path = user_unit_path()?;
        let _ = systemctl(true, &["disable", "--now", "juicetop"]);
        remove_if_exists(&path)?;
        let _ = systemctl(true, &["daemon-reload"]);
        if purge {
            remove_dir_if_exists(DataPaths::user().db.parent())?;
        }
    } else {
        if !is_root() {
            bail!(
                "removing the system service needs root: sudo \"$(command -v juicetop)\" service uninstall"
            );
        }
        let _ = systemctl(false, &["disable", "--now", "juicetop"]);
        remove_if_exists(Path::new(SYSTEM_UNIT_PATH))?;
        let _ = systemctl(false, &["daemon-reload"]);
        remove_if_exists(Path::new(SYSTEM_BIN))?;
        if purge {
            remove_dir_if_exists(DataPaths::system().db.parent())?;
        }
    }
    if !purge {
        println!("kept the recorded data; add --purge to delete it");
    }
    Ok(())
}

pub fn status(user: bool) -> Result<()> {
    let mut cmd = Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.args(["status", "--no-pager", "juicetop"]).status()?;
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_are_embedded_and_user_unit_points_at_binary() {
        assert!(SYSTEM_UNIT.contains("ExecStart=/usr/local/bin/juicetop daemon"));
        let unit = user_unit_for(Path::new("/opt/bin/juicetop"));
        assert!(
            unit.contains("ExecStart=/opt/bin/juicetop daemon"),
            "{unit}"
        );
    }
}
