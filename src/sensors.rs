//! Hardware energy sensors. Each sensor reports joules consumed since its previous read.
//!
//! * CPU: RAPL package energy counters (`/sys/class/powercap`, root only since kernel 5.10),
//!   falling back to the AMD APU socket power (PPT) that amdgpu exposes to every user.
//! * NVIDIA GPUs: NVML total energy counter, falling back to instantaneous power.
//! * AMD discrete GPUs: amdgpu hwmon power.
//! * macOS (Apple Silicon): CPU, GPU and Neural Engine energy from `powermetrics` (root only).
//! * Windows: CPU package power from the Energy Meter performance counters.

use std::fs;
use std::path::{Path, PathBuf};

use nvml_wrapper::Nvml;
use serde::{Deserialize, Serialize};

use crate::live::DeviceStat;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Cpu,
    Gpu,
}

/// A measured component as stored in the database.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Stable identifier, e.g. "cpu" or "nvidia:0".
    pub key: String,
    /// Human readable name, e.g. "CPU (RAPL)" or "NVIDIA GeForce RTX 4080".
    pub label: String,
    pub kind: SourceKind,
}

/// Energy of one source since the previous read.
pub struct Reading {
    pub joules: f64,
}

pub enum Sensor {
    Rapl(Rapl),
    Hwmon(HwmonPower),
    Nvidia(NvidiaGpu),
    /// A sensor from the macOS or Windows module.
    Platform(Box<dyn PlatformSensor>),
}

/// A sensor that only exists on one operating system.
pub trait PlatformSensor {
    fn info(&self) -> &SourceInfo;
    /// Joules since the last call; `dt` is the wall time since then.
    fn read(&mut self, dt: f64) -> Option<Reading>;
    /// Live details the sensor knows besides power (utilization, clock).
    fn stat(&self) -> Option<DeviceStat> {
        None
    }
}

impl Sensor {
    /// The hwmon directory of an amdgpu sensor.
    pub fn hwmon_dir(&self) -> Option<&Path> {
        match self {
            Sensor::Hwmon(h) => h.power_path.parent(),
            _ => None,
        }
    }

    /// The RAPL package directory, for its power limit.
    pub fn rapl_dir(&self) -> Option<&Path> {
        match self {
            Sensor::Rapl(r) => r.domains.first().and_then(|d| d.energy_path.parent()),
            _ => None,
        }
    }

    pub fn info(&self) -> &SourceInfo {
        match self {
            Sensor::Rapl(s) => &s.info,
            Sensor::Hwmon(s) => &s.info,
            Sensor::Nvidia(s) => &s.info,
            Sensor::Platform(s) => s.info(),
        }
    }

    /// Joules since the last call. `dt` is the wall time since then, used by power-based sensors.
    /// Returns `None` when the sensor failed this round (it is then treated as no data).
    pub fn read(&mut self, nvml: Option<&Nvml>, dt: f64) -> Option<Reading> {
        match self {
            Sensor::Rapl(s) => s.read(),
            Sensor::Hwmon(s) => s.read(dt),
            Sensor::Nvidia(s) => nvml.and_then(|n| s.read(n, dt)),
            Sensor::Platform(s) => s.read(dt),
        }
    }
}

/// Probes all supported sensors. Problems are returned as human readable notes.
pub fn probe(nvml: Option<&Nvml>) -> (Vec<Sensor>, Vec<String>) {
    let mut sensors = Vec::new();
    let mut notes = Vec::new();

    #[cfg(target_os = "linux")]
    probe_linux(&mut sensors, &mut notes);
    #[cfg(target_os = "macos")]
    crate::macos::probe(&mut sensors, &mut notes);
    #[cfg(windows)]
    crate::windows::probe(&mut sensors, &mut notes);

    match nvml {
        Some(nvml) => {
            let count = nvml.device_count().unwrap_or(0);
            for index in 0..count {
                match NvidiaGpu::probe(nvml, index) {
                    Ok(g) => sensors.push(Sensor::Nvidia(g)),
                    Err(e) => notes.push(format!("NVIDIA GPU {index}: {e}")),
                }
            }
        }
        None => notes.push("NVML not available (no NVIDIA driver)".into()),
    }

    (sensors, notes)
}

#[cfg(target_os = "linux")]
fn probe_linux(sensors: &mut Vec<Sensor>, notes: &mut Vec<String>) {
    match Rapl::probe(Path::new("/sys/class/powercap")) {
        Ok(Some(r)) => sensors.push(Sensor::Rapl(r)),
        Ok(None) => notes.push("no RAPL package domain found".into()),
        Err(e) => notes.push(e),
    }

    let hwmons = HwmonPower::probe(Path::new("/sys/class/hwmon"));
    let have_cpu = sensors.iter().any(|s| s.info().kind == SourceKind::Cpu);
    for h in hwmons {
        match h.role {
            HwmonRole::ApuSocket if have_cpu => {}
            HwmonRole::ApuSocket => {
                notes.push(
                    "using AMD APU socket power (PPT) for the CPU; run as root for RAPL".into(),
                );
                sensors.push(Sensor::Hwmon(h));
            }
            HwmonRole::DiscreteGpu => sensors.push(Sensor::Hwmon(h)),
        }
    }
}

// ---------------------------------------------------------------- RAPL

struct RaplDomain {
    energy_path: PathBuf,
    max_range_uj: u64,
    last_uj: Option<u64>,
}

pub struct Rapl {
    info: SourceInfo,
    domains: Vec<RaplDomain>,
}

impl Rapl {
    /// Finds every top-level `package-N` domain. Multi-socket machines are summed.
    pub fn probe(root: &Path) -> Result<Option<Rapl>, String> {
        let Ok(entries) = fs::read_dir(root) else {
            return Ok(None);
        };
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        dirs.sort();
        let mut domains = Vec::new();
        let mut unreadable = false;
        for dir in dirs {
            let Some(name) = read_trim(&dir.join("name")) else {
                continue;
            };
            if !name.starts_with("package-") {
                continue;
            }
            let energy_path = dir.join("energy_uj");
            if read_u64(&energy_path).is_none() {
                unreadable = true;
                continue;
            }
            let max_range_uj = read_u64(&dir.join("max_energy_range_uj")).unwrap_or(u64::MAX);
            domains.push(RaplDomain {
                energy_path,
                max_range_uj,
                last_uj: None,
            });
        }
        if domains.is_empty() {
            return if unreadable {
                Err("RAPL counters exist but need root to read".into())
            } else {
                Ok(None)
            };
        }
        let mut rapl = Rapl {
            info: SourceInfo {
                key: "cpu".into(),
                label: cpu_label("RAPL"),
                kind: SourceKind::Cpu,
            },
            domains,
        };
        rapl.read(); // prime the counters
        Ok(Some(rapl))
    }

    fn read(&mut self) -> Option<Reading> {
        let mut joules = 0.0;
        let mut any = false;
        for d in &mut self.domains {
            let Some(now) = read_u64(&d.energy_path) else {
                continue;
            };
            if let Some(prev) = d.last_uj {
                joules += counter_delta(prev, now, d.max_range_uj) as f64 / 1e6;
                any = true;
            }
            d.last_uj = Some(now);
        }
        any.then_some(Reading { joules })
    }
}

/// Difference between two readings of a counter that wraps back to zero after `max`.
pub fn counter_delta(prev: u64, now: u64, max: u64) -> u64 {
    if now >= prev {
        now - prev
    } else {
        max.saturating_sub(prev).saturating_add(now)
    }
}

// ---------------------------------------------------------------- hwmon (amdgpu)

// The hwmon tree exists only on Linux; the code stays portable for its tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HwmonRole {
    /// An APU's integrated GPU: amdgpu reports the whole socket (CPU + iGPU) as PPT.
    ApuSocket,
    DiscreteGpu,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct HwmonPower {
    info: SourceInfo,
    role: HwmonRole,
    power_path: PathBuf,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl HwmonPower {
    fn probe(root: &Path) -> Vec<HwmonPower> {
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        dirs.sort();
        let mut out = Vec::new();
        let mut gpu_index = 0;
        for dir in dirs {
            if read_trim(&dir.join("name")).as_deref() != Some("amdgpu") {
                continue;
            }
            let power_path = ["power1_average", "power1_input"]
                .iter()
                .map(|f| dir.join(f))
                .find(|p| read_u64(p).is_some());
            let Some(power_path) = power_path else {
                continue;
            };
            let is_apu = (0..4)
                .any(|i| read_trim(&dir.join(format!("in{i}_label"))).as_deref() == Some("vddnb"));
            let (role, info) = if is_apu {
                let info = SourceInfo {
                    key: "cpu".into(),
                    label: cpu_label("socket PPT"),
                    kind: SourceKind::Cpu,
                };
                (HwmonRole::ApuSocket, info)
            } else {
                let info = SourceInfo {
                    key: format!("amdgpu:{gpu_index}"),
                    label: "AMD GPU".into(),
                    kind: SourceKind::Gpu,
                };
                gpu_index += 1;
                (HwmonRole::DiscreteGpu, info)
            };
            out.push(HwmonPower {
                info,
                role,
                power_path,
            });
        }
        out
    }

    fn read(&mut self, dt: f64) -> Option<Reading> {
        let microwatts = read_u64(&self.power_path)?;
        Some(Reading {
            joules: microwatts as f64 / 1e6 * dt,
        })
    }
}

// ---------------------------------------------------------------- NVIDIA

pub struct NvidiaGpu {
    info: SourceInfo,
    pub index: u32,
    last_mj: Option<u64>,
    use_counter: bool,
    /// NVML timestamp (µs) of the newest process utilization sample already seen.
    pub last_util_ts: u64,
}

impl NvidiaGpu {
    fn probe(nvml: &Nvml, index: u32) -> Result<NvidiaGpu, String> {
        let dev = nvml.device_by_index(index).map_err(|e| e.to_string())?;
        let name = dev.name().unwrap_or_else(|_| format!("NVIDIA GPU {index}"));
        let counter = dev.total_energy_consumption().ok();
        if counter.is_none() {
            dev.power_usage()
                .map_err(|e| format!("no power reading: {e}"))?;
        }
        Ok(NvidiaGpu {
            info: SourceInfo {
                key: format!("nvidia:{index}"),
                label: name,
                kind: SourceKind::Gpu,
            },
            index,
            last_mj: counter,
            use_counter: counter.is_some(),
            last_util_ts: 0,
        })
    }

    fn read(&mut self, nvml: &Nvml, dt: f64) -> Option<Reading> {
        let dev = nvml.device_by_index(self.index).ok()?;
        if self.use_counter {
            let now = dev.total_energy_consumption().ok()?;
            let prev = self.last_mj.replace(now)?;
            return Some(Reading {
                joules: now.saturating_sub(prev) as f64 / 1e3,
            });
        }
        let milliwatts = dev.power_usage().ok()?;
        Some(Reading {
            joules: milliwatts as f64 / 1e3 * dt,
        })
    }
}

// ---------------------------------------------------------------- helpers

pub(crate) fn cpu_label(method: &str) -> String {
    let model = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .or_else(other_cpu_model);
    match model {
        Some(m) => format!("{m} ({method})"),
        None => format!("CPU ({method})"),
    }
}

#[cfg(target_os = "linux")]
fn other_cpu_model() -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
fn other_cpu_model() -> Option<String> {
    use sysinfo::{CpuRefreshKind, RefreshKind, System};
    let sys =
        System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()));
    sys.cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
}

pub(crate) fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub(crate) fn read_u64(path: &Path) -> Option<u64> {
    read_trim(path)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_delta_handles_wraparound() {
        assert_eq!(counter_delta(100, 250, 1000), 150);
        assert_eq!(counter_delta(900, 50, 1000), 150);
        assert_eq!(counter_delta(5, 5, 1000), 0);
    }

    #[test]
    fn rapl_probe_reads_fake_powercap_tree() {
        // The kernel names these `intel-rapl:0`; Windows forbids ':' in file names, and the
        // probe goes by the `name` file anyway.
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("intel-rapl_0");
        let core = tmp.path().join("intel-rapl_0_0");
        fs::create_dir_all(&pkg).unwrap();
        fs::create_dir_all(&core).unwrap();
        fs::write(pkg.join("name"), "package-0\n").unwrap();
        fs::write(pkg.join("energy_uj"), "1000000\n").unwrap();
        fs::write(pkg.join("max_energy_range_uj"), "5000000\n").unwrap();
        fs::write(core.join("name"), "core\n").unwrap();
        fs::write(core.join("energy_uj"), "1\n").unwrap();

        let mut rapl = Rapl::probe(tmp.path()).unwrap().unwrap();
        assert_eq!(rapl.domains.len(), 1, "only package domains count");
        fs::write(pkg.join("energy_uj"), "3500000\n").unwrap();
        assert!((rapl.read().unwrap().joules - 2.5).abs() < 1e-9);
        fs::write(pkg.join("energy_uj"), "500000\n").unwrap(); // wrapped
        assert!((rapl.read().unwrap().joules - 2.0).abs() < 1e-9);
    }

    #[test]
    fn hwmon_probe_tells_apu_from_dgpu() {
        let tmp = tempfile::tempdir().unwrap();
        let apu = tmp.path().join("hwmon1");
        let dgpu = tmp.path().join("hwmon2");
        let other = tmp.path().join("hwmon3");
        for d in [&apu, &dgpu, &other] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(apu.join("name"), "amdgpu\n").unwrap();
        fs::write(apu.join("power1_input"), "48000000\n").unwrap();
        fs::write(apu.join("in0_label"), "vddgfx\n").unwrap();
        fs::write(apu.join("in1_label"), "vddnb\n").unwrap();
        fs::write(dgpu.join("name"), "amdgpu\n").unwrap();
        fs::write(dgpu.join("power1_average"), "150000000\n").unwrap();
        fs::write(dgpu.join("in0_label"), "vddgfx\n").unwrap();
        fs::write(other.join("name"), "k10temp\n").unwrap();

        let mut found = HwmonPower::probe(tmp.path());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].role, HwmonRole::ApuSocket);
        assert_eq!(found[0].info.key, "cpu");
        assert_eq!(found[1].role, HwmonRole::DiscreteGpu);
        assert!((found[0].read(2.0).unwrap().joules - 96.0).abs() < 1e-9);
        assert!((found[1].read(1.0).unwrap().joules - 150.0).abs() < 1e-9);
    }
}
