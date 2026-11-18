//! Per-process energy attribution.
//!
//! Energy cannot be measured per process, so it is split by activity:
//! CPU energy by each process's share of busy CPU time in the interval, GPU energy by each
//! process's share of SM utilization (or of GPU memory when the GPU is idle but held).
//! Processes are grouped by a readable name, not by PID, so restarts add up.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Name used for energy that no process can be blamed for.
pub const IDLE: &str = "(idle)";

/// Interpreters whose processes are named after the script they run.
const INTERPRETERS: &[&str] = &[
    "python", "python2", "python3", "node", "nodejs", "deno", "bun", "java", "ruby", "perl",
    "bash", "sh", "zsh", "dash", "fish", "php", "lua", "Rscript", "julia", "dotnet", "mono",
];

#[derive(Clone)]
struct ProcState {
    start_time: u64,
    ticks: u64,
    name: String,
}

/// Tracks CPU time of every process between scans.
pub struct ProcTracker {
    root: std::path::PathBuf,
    prev: HashMap<u32, ProcState>,
    primed: bool,
}

impl ProcTracker {
    pub fn new() -> Self {
        Self::with_root("/proc")
    }

    pub fn with_root(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
            prev: HashMap::new(),
            primed: false,
        }
    }

    /// CPU ticks each process name used since the previous scan.
    /// The first scan only records a baseline and returns nothing.
    pub fn scan(&mut self) -> HashMap<String, u64> {
        let mut next = HashMap::with_capacity(self.prev.len());
        let mut busy: HashMap<String, u64> = HashMap::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return busy;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let dir = entry.path();
            let Some(stat) = fs::read_to_string(dir.join("stat")).ok() else {
                continue;
            };
            let Some(parsed) = parse_stat(&stat) else {
                continue;
            };

            let state = match self.prev.get(&pid) {
                Some(p) if p.start_time == parsed.start_time => {
                    let delta = parsed.ticks.saturating_sub(p.ticks);
                    if delta > 0 {
                        *busy.entry(p.name.clone()).or_default() += delta;
                    }
                    ProcState {
                        ticks: parsed.ticks,
                        ..p.clone()
                    }
                }
                _ => {
                    let name = process_name(&dir, &parsed.comm);
                    // A process born after the previous scan used all its ticks in this interval.
                    if self.primed && parsed.ticks > 0 {
                        *busy.entry(name.clone()).or_default() += parsed.ticks;
                    }
                    ProcState {
                        start_time: parsed.start_time,
                        ticks: parsed.ticks,
                        name,
                    }
                }
            };
            next.insert(pid, state);
        }
        self.prev = next;
        self.primed = true;
        busy
    }

    /// The display name of a PID seen in the last scan, resolving it if unknown.
    pub fn name_of(&self, pid: u32) -> String {
        if let Some(p) = self.prev.get(&pid) {
            return p.name.clone();
        }
        let dir = self.root.join(pid.to_string());
        let comm = fs::read_to_string(dir.join("comm")).unwrap_or_default();
        let comm = comm.trim();
        if comm.is_empty() {
            format!("pid {pid}")
        } else {
            process_name(&dir, comm)
        }
    }
}

impl Default for ProcTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, PartialEq)]
struct Stat {
    comm: String,
    ticks: u64,
    start_time: u64,
}

/// Parses `/proc/<pid>/stat`. The command name may contain spaces and parentheses,
/// so fields are counted from the last ')'.
fn parse_stat(s: &str) -> Option<Stat> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = s.get(close + 1..)?.split_whitespace().collect();
    // rest[0] is field 3 (state); utime is field 14, stime 15, starttime 22.
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    let start_time: u64 = rest.get(19)?.parse().ok()?;
    Some(Stat {
        comm,
        ticks: utime + stime,
        start_time,
    })
}

fn process_name(dir: &Path, comm: &str) -> String {
    let cmdline = fs::read(dir.join("cmdline")).unwrap_or_default();
    name_from(comm, &cmdline)
}

/// Picks a readable name: kernel threads are grouped, interpreters show their script,
/// and names cut off at 15 characters are completed from the command line.
pub fn name_from(comm: &str, cmdline: &[u8]) -> String {
    if cmdline.is_empty() {
        return "[kernel]".into();
    }
    let args: Vec<String> = cmdline
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    // Some programs (Chrome, Electron) rewrite argv into one space separated string.
    let argv0 = args
        .first()
        .map(|a| basename(a.split(' ').next().unwrap_or(a)))
        .unwrap_or_default();

    let base = if comm.len() >= 15 && argv0.starts_with(comm) {
        argv0.clone()
    } else {
        comm.to_string()
    };

    // macOS framework builds call Python "Python".
    let lower = base.to_ascii_lowercase();
    let is_interpreter = INTERPRETERS.iter().any(|i| {
        let i = i.to_ascii_lowercase();
        lower == i
            || lower
                .strip_prefix(&i)
                .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
    });
    if is_interpreter {
        let mut it = args.iter().skip(1);
        while let Some(a) = it.next() {
            // `python -m vllm.entrypoints.openai.api_server` is "vllm".
            if a == "-m"
                && let Some(module) = it.next()
            {
                return module.split('.').next().unwrap_or(module).to_string();
            }
            if a.starts_with('-') {
                continue;
            }
            return script_name(a);
        }
    }
    base
}

/// A script's file name, with its folder when the name alone says nothing ("ComfyUI/main.py").
fn script_name(path: &str) -> String {
    const GENERIC: &[&str] = &[
        "main.py",
        "app.py",
        "server.py",
        "run.py",
        "__main__.py",
        "index.js",
        "main.js",
        "server.js",
        "app.js",
    ];
    let file = basename(path);
    if GENERIC.contains(&file.as_str()) {
        let mut parts = path.rsplit(['/', '\\']).skip(1);
        if let Some(dir) = parts.next().filter(|d| !d.is_empty() && *d != ".") {
            return format!("{}/{file}", basename(dir));
        }
    }
    file
}

fn basename(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    name.chars().take(40).collect()
}

/// The process tracker for this system: `/proc` on Linux, `sysinfo` elsewhere.
#[cfg(target_os = "linux")]
pub type Tracker = ProcTracker;
#[cfg(not(target_os = "linux"))]
pub type Tracker = crate::sysproc::SysTracker;

/// Splits `joules` across `weights` proportionally. With no positive weight, everything goes
/// to `fallback`.
pub fn split(joules: f64, weights: &HashMap<String, f64>, fallback: &str) -> Vec<(String, f64)> {
    let total: f64 = weights.values().filter(|w| **w > 0.0).sum();
    if total.is_nan() || total <= 0.0 || joules <= 0.0 {
        return if joules > 0.0 {
            vec![(fallback.to_string(), joules)]
        } else {
            Vec::new()
        };
    }
    weights
        .iter()
        .filter(|(_, w)| **w > 0.0)
        .map(|(name, w)| (name.clone(), joules * w / total))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_odd_comm() {
        let line = "1234 (my (weird) app) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 4 0 98765 1000 100 18446744073709551615";
        let s = parse_stat(line).unwrap();
        assert_eq!(s.comm, "my (weird) app");
        assert_eq!(s.ticks, 300);
        assert_eq!(s.start_time, 98765);
    }

    #[test]
    fn names_interpreters_after_their_script() {
        assert_eq!(
            name_from(
                "python3",
                b"/usr/bin/python3\0-u\0/opt/ComfyUI/main.py\0--listen\0"
            ),
            "ComfyUI/main.py"
        );
        assert_eq!(
            name_from("python3", b"python3\0train.py\0--epochs\x003\0"),
            "train.py"
        );
        assert_eq!(
            name_from(
                "python3",
                b"python3\0-m\0vllm.entrypoints.openai.api_server\0"
            ),
            "vllm"
        );
        assert_eq!(
            name_from("python3", b"/usr/bin/python3\0/opt/venv/bin/vllm\0serve\0"),
            "vllm"
        );
        assert_eq!(name_from("node", b"node\0server.js\0"), "server.js");
        assert_eq!(name_from("python3.12", b"python3.12\0"), "python3.12");
    }

    #[test]
    fn completes_truncated_names_and_groups_kernel_threads() {
        assert_eq!(
            name_from("llama-server", b"/opt/llama/llama-server\0-m\0x.gguf\0"),
            "llama-server"
        );
        assert_eq!(
            name_from("WebKitWebProces", b"/usr/lib/WebKitWebProcess\x0012\0"),
            "WebKitWebProcess"
        );
        assert_eq!(name_from("kworker/3:1", b""), "[kernel]");
        assert_eq!(
            name_from(
                "chrome-headless",
                b"/opt/chrome/chrome-headless-shell --type=gpu-process --no-sandbox\0"
            ),
            "chrome-headless-shell"
        );
    }

    #[test]
    fn split_is_proportional_and_falls_back() {
        let mut w = HashMap::new();
        w.insert("a".to_string(), 3.0);
        w.insert("b".to_string(), 1.0);
        let mut parts = split(8.0, &w, IDLE);
        parts.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(parts, vec![("a".into(), 6.0), ("b".into(), 2.0)]);
        assert_eq!(
            split(5.0, &HashMap::new(), IDLE),
            vec![(IDLE.to_string(), 5.0)]
        );
        assert!(split(0.0, &w, IDLE).is_empty());
    }

    #[test]
    fn tracker_reports_deltas_after_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("42");
        fs::create_dir_all(&p).unwrap();
        let stat = |ticks: u64| {
            format!("42 (worker) R 1 1 1 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 555 0 0 0")
        };
        fs::write(p.join("stat"), stat(100)).unwrap();
        fs::write(p.join("cmdline"), b"worker\0").unwrap();
        let mut t = ProcTracker::with_root(tmp.path());
        assert!(t.scan().is_empty(), "first scan is a baseline");
        fs::write(p.join("stat"), stat(130)).unwrap();
        assert_eq!(t.scan().get("worker"), Some(&30));
        assert_eq!(t.name_of(42), "worker");
    }
}
