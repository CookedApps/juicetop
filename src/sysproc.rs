//! Processes, CPU load and memory through `sysinfo`, for systems without `/proc`
//! (macOS and Windows).

use std::collections::HashMap;

use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
    UpdateKind,
};

use crate::procs::{self, IDLE};

/// One process as the platform reports it.
pub struct Proc {
    pub pid: u32,
    pub parent: Option<u32>,
    pub start: u64,
    /// CPU time used so far, in milliseconds.
    pub cpu_ms: u64,
    pub name: String,
    /// The command line; empty when the system hides it.
    pub args: Vec<String>,
}

/// Every running process with a readable name.
pub fn processes(sys: &mut System) -> Vec<Proc> {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_cmd(UpdateKind::OnlyIfNotSet),
    );
    sys.processes()
        .iter()
        .map(|(pid, p)| {
            let args: Vec<String> = p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            Proc {
                pid: pid.as_u32(),
                parent: p.parent().map(|x| x.as_u32()),
                start: p.start_time(),
                cpu_ms: p.accumulated_cpu_time(),
                name: display_name(&p.name().to_string_lossy(), &args),
                args,
            }
        })
        .collect()
}

/// The same naming rules as on Linux. Processes whose arguments are hidden keep their name.
pub fn display_name(name: &str, args: &[String]) -> String {
    let comm = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))
        .unwrap_or(name);
    if comm.is_empty() {
        return IDLE.into();
    }
    if args.is_empty() {
        return comm.to_string();
    }
    procs::name_from(comm, args.join("\0").as_bytes())
}

/// Tracks CPU time per process name between scans, like `procs::ProcTracker` on Linux.
pub struct SysTracker {
    sys: System,
    prev: HashMap<u32, (u64, u64, String)>,
    primed: bool,
}

impl SysTracker {
    pub fn new() -> Self {
        Self {
            sys: System::new(),
            prev: HashMap::new(),
            primed: false,
        }
    }

    /// CPU milliseconds each process name used since the previous scan.
    pub fn scan(&mut self) -> HashMap<String, u64> {
        let mut busy: HashMap<String, u64> = HashMap::new();
        let mut next = HashMap::with_capacity(self.prev.len());
        for p in processes(&mut self.sys) {
            let used = match self.prev.get(&p.pid) {
                Some((start, ms, _)) if *start == p.start => p.cpu_ms.saturating_sub(*ms),
                _ if self.primed => p.cpu_ms,
                _ => 0,
            };
            if used > 0 {
                *busy.entry(p.name.clone()).or_default() += used;
            }
            next.insert(p.pid, (p.start, p.cpu_ms, p.name));
        }
        self.prev = next;
        self.primed = true;
        busy
    }

    pub fn name_of(&self, pid: u32) -> String {
        self.prev
            .get(&pid)
            .map(|(_, _, n)| n.clone())
            .unwrap_or_else(|| format!("pid {pid}"))
    }
}

impl Default for SysTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// CPU load, clock and memory for the CPU panel.
pub struct CpuStats {
    sys: System,
}

impl CpuStats {
    pub fn new() -> Self {
        Self {
            sys: System::new_with_specifics(
                RefreshKind::nothing()
                    .with_cpu(CpuRefreshKind::everything())
                    .with_memory(MemoryRefreshKind::nothing().with_ram()),
            ),
        }
    }

    /// (utilization %, average clock MHz, RAM used MB, RAM total MB).
    pub fn read(&mut self) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
        self.sys.refresh_cpu_specifics(CpuRefreshKind::everything());
        self.sys
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        let cpus = self.sys.cpus();
        let util = (!cpus.is_empty()).then(|| self.sys.global_cpu_usage() as f64);
        let clocks: Vec<u64> = cpus
            .iter()
            .map(|c| c.frequency())
            .filter(|f| *f > 0)
            .collect();
        let clock =
            (!clocks.is_empty()).then(|| clocks.iter().sum::<u64>() as f64 / clocks.len() as f64);
        let mb = |b: u64| (b > 0).then(|| b as f64 / 1_048_576.0);
        (
            util,
            clock,
            mb(self.sys.used_memory()),
            mb(self.sys.total_memory()),
        )
    }
}

impl Default for CpuStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_windows_and_mac_processes() {
        assert_eq!(display_name("llama-server.exe", &[]), "llama-server");
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            display_name(
                "python.exe",
                &args(&["C:\\Python312\\python.exe", "C:\\ai\\ComfyUI\\main.py"])
            ),
            "ComfyUI/main.py"
        );
        assert_eq!(
            display_name(
                "Python",
                &args(&["/usr/bin/python3", "-m", "mlx_lm.server"])
            ),
            "mlx_lm"
        );
        assert_eq!(
            display_name("ollama", &args(&["ollama", "serve"])),
            "ollama"
        );
    }
}
