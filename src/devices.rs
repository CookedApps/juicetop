//! What each device is doing besides drawing power: utilization, temperature, clock, memory
//! and power limit. Shown live only; nothing here is stored.

use std::fs;
use std::path::{Path, PathBuf};

use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};

use crate::live::DeviceStat;
use crate::sensors::{Sensor, SourceKind, read_trim, read_u64};

/// Keeps the state needed between reads (CPU time counters) and the paths found once.
pub struct DeviceStats {
    proc_root: PathBuf,
    prev_cpu: Option<(u64, u64)>,
    cpu_temp: Option<PathBuf>,
    /// CPU and memory figures where `/proc` does not exist.
    #[cfg(not(target_os = "linux"))]
    other: crate::sysproc::CpuStats,
}

impl DeviceStats {
    pub fn new() -> Self {
        Self {
            proc_root: "/proc".into(),
            prev_cpu: None,
            cpu_temp: find_cpu_temp(Path::new("/sys/class/hwmon")),
            #[cfg(not(target_os = "linux"))]
            other: crate::sysproc::CpuStats::new(),
        }
    }

    pub fn collect(&mut self, sensors: &[Sensor], nvml: Option<&Nvml>) -> Vec<DeviceStat> {
        let util = self.cpu_util();
        #[cfg(not(target_os = "linux"))]
        let other = self.other.read();
        sensors
            .iter()
            .map(|s| {
                let key = s.info().key.clone();
                let stat = match s {
                    Sensor::Nvidia(g) => nvidia(nvml, g.index, key),
                    s if s.info().kind == SourceKind::Cpu => DeviceStat {
                        key,
                        util_pct: util,
                        temp_c: self
                            .cpu_temp
                            .as_deref()
                            .and_then(read_u64)
                            .map(|m| m as f64 / 1000.0),
                        clock_mhz: cpu_clock_mhz(Path::new("/sys/devices/system/cpu")),
                        mem_used_mb: None,
                        mem_total_mb: None,
                        power_limit_w: s.rapl_dir().and_then(rapl_limit_w),
                    }
                    .with_ram(&self.proc_root),
                    s => s
                        .hwmon_dir()
                        .map(|d| amdgpu(d, key.clone()))
                        .unwrap_or(DeviceStat {
                            key,
                            ..Default::default()
                        }),
                };
                #[cfg(not(target_os = "linux"))]
                let stat = if s.info().kind == SourceKind::Cpu {
                    let (util_pct, clock_mhz, mem_used_mb, mem_total_mb) = other;
                    stat.or(DeviceStat {
                        key: String::new(),
                        util_pct,
                        clock_mhz,
                        mem_used_mb,
                        mem_total_mb,
                        ..Default::default()
                    })
                } else {
                    stat
                };
                match s {
                    Sensor::Platform(p) => match p.stat() {
                        Some(extra) => stat.or(extra),
                        None => stat,
                    },
                    _ => stat,
                }
            })
            .collect()
    }

    /// Busy share of all CPUs since the last call, from /proc/stat.
    fn cpu_util(&mut self) -> Option<f64> {
        let stat = fs::read_to_string(self.proc_root.join("stat")).ok()?;
        let (busy, total) = parse_cpu_line(stat.lines().next()?)?;
        let prev = self.prev_cpu.replace((busy, total));
        let (pb, pt) = prev?;
        let dt = total.saturating_sub(pt);
        (dt > 0).then(|| busy.saturating_sub(pb) as f64 / dt as f64 * 100.0)
    }
}

impl Default for DeviceStats {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceStat {
    /// Fills the fields this one lacks from `other`.
    fn or(self, other: DeviceStat) -> DeviceStat {
        DeviceStat {
            key: self.key,
            util_pct: self.util_pct.or(other.util_pct),
            temp_c: self.temp_c.or(other.temp_c),
            clock_mhz: self.clock_mhz.or(other.clock_mhz),
            mem_used_mb: self.mem_used_mb.or(other.mem_used_mb),
            mem_total_mb: self.mem_total_mb.or(other.mem_total_mb),
            power_limit_w: self.power_limit_w.or(other.power_limit_w),
        }
    }

    /// System RAM, shown on the CPU panel.
    fn with_ram(mut self, proc_root: &Path) -> Self {
        if let Some((used, total)) = fs::read_to_string(proc_root.join("meminfo"))
            .ok()
            .and_then(|m| parse_meminfo(&m))
        {
            self.mem_used_mb = Some(used);
            self.mem_total_mb = Some(total);
        }
        self
    }
}

/// (busy, total) jiffies from the aggregate "cpu" line.
fn parse_cpu_line(line: &str) -> Option<(u64, u64)> {
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
    if v.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal; guest time is already in user.
    let total: u64 = v.iter().take(8).sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// (used, total) in MB from /proc/meminfo.
fn parse_meminfo(text: &str) -> Option<(f64, f64)> {
    let field = |name: &str| {
        text.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
    };
    let total = field("MemTotal:")?;
    let avail = field("MemAvailable:")?;
    Some(((total - avail) / 1024.0, total / 1024.0))
}

fn find_cpu_temp(root: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    dirs.sort();
    for wanted in ["k10temp", "zenpower", "coretemp", "cpu_thermal"] {
        for d in &dirs {
            if read_trim(&d.join("name")).as_deref() == Some(wanted) {
                let p = d.join("temp1_input");
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn cpu_clock_mhz(root: &Path) -> Option<f64> {
    let mut sum = 0.0;
    let mut n = 0;
    for e in fs::read_dir(root).ok()?.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("cpu")
            || !name[3..].chars().all(|c| c.is_ascii_digit())
            || name.len() == 3
        {
            continue;
        }
        if let Some(khz) = read_u64(&e.path().join("cpufreq/scaling_cur_freq")) {
            sum += khz as f64 / 1000.0;
            n += 1;
        }
    }
    (n > 0).then(|| sum / n as f64)
}

/// The long-term RAPL limit, ignored when it is unset or implausible.
fn rapl_limit_w(dir: &Path) -> Option<f64> {
    let w = read_u64(&dir.join("constraint_0_power_limit_uw"))? as f64 / 1e6;
    (w > 1.0 && w < 2000.0).then_some(w)
}

fn nvidia(nvml: Option<&Nvml>, index: u32, key: String) -> DeviceStat {
    let mut s = DeviceStat {
        key,
        ..Default::default()
    };
    let Some(dev) = nvml.and_then(|n| n.device_by_index(index).ok()) else {
        return s;
    };
    s.util_pct = dev.utilization_rates().ok().map(|u| u.gpu as f64);
    s.temp_c = dev
        .temperature(TemperatureSensor::Gpu)
        .ok()
        .map(|t| t as f64);
    s.clock_mhz = dev.clock_info(Clock::Graphics).ok().map(|c| c as f64);
    if let Ok(m) = dev.memory_info() {
        s.mem_used_mb = Some(m.used as f64 / 1_048_576.0);
        s.mem_total_mb = Some(m.total as f64 / 1_048_576.0);
    }
    s.power_limit_w = dev.enforced_power_limit().ok().map(|mw| mw as f64 / 1000.0);
    s
}

fn amdgpu(hwmon: &Path, key: String) -> DeviceStat {
    let dev = hwmon.join("device");
    let mb = |f: &str| read_u64(&dev.join(f)).map(|b| b as f64 / 1_048_576.0);
    DeviceStat {
        key,
        util_pct: read_u64(&dev.join("gpu_busy_percent")).map(|v| v as f64),
        temp_c: read_u64(&hwmon.join("temp1_input")).map(|m| m as f64 / 1000.0),
        clock_mhz: read_u64(&hwmon.join("freq1_input")).map(|hz| hz as f64 / 1e6),
        mem_used_mb: mb("mem_info_vram_used"),
        mem_total_mb: mb("mem_info_vram_total"),
        power_limit_w: read_u64(&hwmon.join("power1_cap")).map(|uw| uw as f64 / 1e6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_and_memory() {
        assert_eq!(
            parse_cpu_line("cpu  100 0 50 800 50 0 0 0 0 0"),
            Some((150, 1000))
        );
        assert_eq!(parse_cpu_line("cpu0 1 2 3 4"), None);
        let mem = "MemTotal:       65536000 kB\nMemFree:  1 kB\nMemAvailable:   16384000 kB\n";
        assert_eq!(parse_meminfo(mem), Some((48000.0, 64000.0)));
    }

    #[test]
    fn cpu_util_needs_two_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let mut d = DeviceStats::new();
        d.proc_root = tmp.path().into();
        fs::write(tmp.path().join("stat"), "cpu  100 0 0 900 0 0 0 0\n").unwrap();
        assert_eq!(d.cpu_util(), None);
        fs::write(tmp.path().join("stat"), "cpu  150 0 0 950 0 0 0 0\n").unwrap();
        assert_eq!(d.cpu_util(), Some(50.0));
    }

    #[test]
    fn finds_k10temp_and_reads_amdgpu() {
        let tmp = tempfile::tempdir().unwrap();
        let k = tmp.path().join("hwmon0");
        fs::create_dir_all(&k).unwrap();
        fs::write(k.join("name"), "k10temp\n").unwrap();
        fs::write(k.join("temp1_input"), "61250\n").unwrap();
        assert_eq!(find_cpu_temp(tmp.path()), Some(k.join("temp1_input")));

        let g = tmp.path().join("hwmon1");
        fs::create_dir_all(g.join("device")).unwrap();
        fs::write(g.join("device/gpu_busy_percent"), "87\n").unwrap();
        fs::write(g.join("device/mem_info_vram_total"), "17179869184\n").unwrap();
        fs::write(g.join("power1_cap"), "300000000\n").unwrap();
        let s = amdgpu(&g, "amdgpu:0".into());
        assert_eq!(s.util_pct, Some(87.0));
        assert_eq!(s.mem_total_mb, Some(16384.0));
        assert_eq!(s.power_limit_w, Some(300.0));
    }
}
