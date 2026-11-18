//! Windows: the collector as a Task Scheduler task. Untested.
//!
//! The system task runs as SYSTEM from boot; the user task runs as you from logon. A task, not a
//! Windows service, because it needs no service wrapper in the binary. Task Scheduler ends the
//! collector without warning, so stopping it loses the current minute.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{remove_dir_if_exists, remove_if_exists};
use crate::live::{DataPaths, is_root};

const TASK: &str = "juicetop";
const USER_TASK: &str = "juicetop-user";
pub const TASK_XML: &str = include_str!("../../packaging/windows/juicetop-task.xml");
const ADMIN_HINT: &str = "open a terminal as administrator and run: juicetop service install";

fn system_bin() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Program Files"))
        .join("juicetop")
        .join("juicetop.exe")
}

fn current_user() -> Option<String> {
    let name = std::env::var("USERNAME").ok()?;
    Some(match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{name}"),
        _ => name,
    })
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The task definition: from boot as SYSTEM, or from logon as `user`.
pub fn task_xml(exe: &Path, user: Option<&str>) -> String {
    let (trigger, principal) = match user {
        None => (
            "<BootTrigger><Enabled>true</Enabled></BootTrigger>".to_string(),
            "<UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel>".to_string(),
        ),
        Some(u) => {
            let u = escape(u);
            (
                format!("<LogonTrigger><Enabled>true</Enabled><UserId>{u}</UserId></LogonTrigger>"),
                format!(
                    "<UserId>{u}</UserId><LogonType>InteractiveToken</LogonType>\
                     <RunLevel>LeastPrivilege</RunLevel>"
                ),
            )
        }
    };
    TASK_XML
        .replace("__TRIGGER__", &trigger)
        .replace("__PRINCIPAL__", &principal)
        .replace("__COMMAND__", &escape(&exe.display().to_string()))
}

fn schtasks(args: &[&str]) -> Result<()> {
    let status = Command::new("schtasks")
        .args(args)
        .status()
        .context("running schtasks")?;
    if !status.success() {
        bail!("schtasks {} failed", args.join(" "));
    }
    Ok(())
}

/// Registers a task from XML. schtasks reads the file reliably as UTF-16 with a byte order mark.
fn register(name: &str, xml: &str) -> Result<()> {
    let path = std::env::temp_dir().join(format!("{name}-task.xml"));
    let mut bytes = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    let path_s = path.display().to_string();
    let result = schtasks(&["/Create", "/TN", name, "/XML", &path_s, "/F"]);
    let _ = std::fs::remove_file(&path);
    result
}

pub fn install(user: bool) -> Result<()> {
    let exe = std::env::current_exe().context("finding the juicetop binary")?;
    if user {
        let who = current_user().context("cannot tell the current user (USERNAME is unset)")?;
        let _ = Command::new("schtasks")
            .args(["/End", "/TN", USER_TASK])
            .status();
        register(USER_TASK, &task_xml(&exe, Some(&who)))?;
        schtasks(&["/Run", "/TN", USER_TASK])?;
        println!("registered the task {USER_TASK}: the collector runs as {who} from logon");
        println!("data: {}", DataPaths::user().db.display());
    } else {
        if !is_root() {
            bail!("installing the system task needs administrator rights; {ADMIN_HINT}");
        }
        let bin = system_bin();
        let _ = Command::new("schtasks")
            .args(["/End", "/TN", TASK])
            .status();
        if exe != bin {
            if let Some(dir) = bin.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // A running binary cannot be overwritten on Windows, but it can be renamed.
            if bin.exists() {
                let old = bin.with_file_name("juicetop.old.exe");
                let _ = std::fs::remove_file(&old);
                let _ = std::fs::rename(&bin, &old);
            }
            std::fs::copy(&exe, &bin)
                .with_context(|| format!("copying {} to {}", exe.display(), bin.display()))?;
            println!("installed {}", bin.display());
        }
        register(TASK, &task_xml(&bin, None))?;
        schtasks(&["/Run", "/TN", TASK])?;
        println!("registered the task {TASK}: the collector runs as SYSTEM from boot");
        println!("data: {}", DataPaths::system().db.display());
    }
    println!("open the dashboard with: juicetop");
    Ok(())
}

pub fn uninstall(user: bool, purge: bool) -> Result<()> {
    let name = if user { USER_TASK } else { TASK };
    if !user && !is_root() {
        bail!("removing the system task needs administrator rights");
    }
    let _ = Command::new("schtasks")
        .args(["/End", "/TN", name])
        .status();
    let _ = schtasks(&["/Delete", "/TN", name, "/F"]);
    println!("removed the task {name}");
    if !user {
        let bin = system_bin();
        if std::env::current_exe().ok().as_deref() == Some(bin.as_path()) {
            println!(
                "delete {} yourself; it is the running program",
                bin.display()
            );
        } else {
            remove_if_exists(&bin)?;
            remove_dir_if_exists(bin.parent())?;
        }
    }
    if purge {
        let paths = if user {
            DataPaths::user()
        } else {
            DataPaths::system()
        };
        remove_dir_if_exists(paths.db.parent())?;
        remove_dir_if_exists(paths.live.parent())?;
    } else {
        println!("kept the recorded data; add --purge to delete it");
    }
    Ok(())
}

pub fn status(user: bool) -> Result<()> {
    let name = if user { USER_TASK } else { TASK };
    Command::new("schtasks")
        .args(["/Query", "/TN", name, "/V", "/FO", "LIST"])
        .status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_fills_every_placeholder() {
        let sys = task_xml(Path::new("C:\\Program Files\\juicetop\\juicetop.exe"), None);
        assert!(!sys.contains("__"), "{sys}");
        assert!(sys.contains("<BootTrigger>"));
        assert!(sys.contains("S-1-5-18"));
        let user = task_xml(Path::new("C:\\a&b\\juicetop.exe"), Some("PC\\sam"));
        assert!(user.contains("<LogonTrigger>"));
        assert!(user.contains("C:\\a&amp;b\\juicetop.exe"));
        assert!(user.contains("<UserId>PC\\sam</UserId>"));
    }
}
