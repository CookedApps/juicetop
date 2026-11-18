//! macOS: CPU, GPU and Neural Engine energy from `powermetrics`.
//!
//! `powermetrics` ships with macOS and needs root, so the system
//! service (a launchd daemon) gets these readings and a user service does not. It runs as a child
//! process that prints one plist per second; the energy fields (`cpu_energy`, `gpu_energy`,
//! `ane_energy`, in millijoules per sample) are summed until the collector reads them. Apple
//! Silicon only: Intel Macs report package power under other keys and get no sensor yet.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::live::{DeviceStat, is_root};
use crate::powermetrics::{Sample, parse_sample};
use crate::sensors::{PlatformSensor, Reading, Sensor, SourceInfo, SourceKind, cpu_label};

const POWERMETRICS: &str = "/usr/bin/powermetrics";

pub fn probe(sensors: &mut Vec<Sensor>, notes: &mut Vec<String>) {
    if !is_root() {
        notes.push(
            "macOS gives power readings only to root: run this with sudo \
             (the collector service already runs as root)"
                .into(),
        );
        return;
    }
    match PowerMetrics::start() {
        Ok(pm) => {
            let pm = Arc::new(pm);
            let cpu = cpu_label("powermetrics");
            let chip = cpu.split(" (").next().unwrap_or("Apple").to_string();
            sensors.push(Sensor::Platform(Box::new(AppleSensor {
                info: SourceInfo {
                    key: "cpu".into(),
                    label: cpu,
                    kind: SourceKind::Cpu,
                },
                part: Part::Cpu,
                pm: Arc::clone(&pm),
            })));
            sensors.push(Sensor::Platform(Box::new(AppleSensor {
                info: SourceInfo {
                    key: "applegpu:0".into(),
                    label: format!("{chip} GPU"),
                    kind: SourceKind::Gpu,
                },
                part: Part::Gpu,
                pm,
            })));
        }
        Err(e) => notes.push(e),
    }
}

#[derive(Default)]
struct Totals {
    cpu_mj: f64,
    gpu_mj: f64,
    gpu_util: Option<f64>,
    gpu_mhz: Option<f64>,
    alive: bool,
}

struct PowerMetrics {
    child: Mutex<Child>,
    totals: Arc<Mutex<Totals>>,
}

impl PowerMetrics {
    fn start() -> Result<Self, String> {
        let mut child = Command::new(POWERMETRICS)
            .args([
                "--samplers",
                "cpu_power,gpu_power",
                "-i",
                "1000",
                "-f",
                "plist",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {POWERMETRICS}: {e}"))?;
        let mut out = child.stdout.take().ok_or("powermetrics has no output")?;
        let totals = Arc::new(Mutex::new(Totals {
            alive: true,
            ..Default::default()
        }));
        let (first_tx, first_rx) = mpsc::channel::<Sample>();
        let t = Arc::clone(&totals);
        std::thread::Builder::new()
            .name("juicetop-powermetrics".into())
            .spawn(move || {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 8192];
                let mut first = Some(first_tx);
                while let Ok(n) = out.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(end) = find(&buf, b"</plist>") {
                        let end = end + b"</plist>".len();
                        // Every plist after the first starts with a NUL byte.
                        let text = String::from_utf8_lossy(&buf[..end]).replace('\0', "");
                        buf.drain(..end);
                        let s = parse_sample(&text);
                        if let Some(tx) = first.take() {
                            let _ = tx.send(s);
                        }
                        if let Ok(mut t) = t.lock() {
                            t.cpu_mj += s.cpu_mj.unwrap_or(0.0);
                            t.gpu_mj += s.gpu_mj.unwrap_or(0.0) + s.ane_mj.unwrap_or(0.0);
                            t.gpu_util = s.gpu_idle.map(|i| ((1.0 - i) * 100.0).clamp(0.0, 100.0));
                            t.gpu_mhz = s.gpu_mhz;
                        }
                    }
                }
                if let Ok(mut t) = t.lock() {
                    t.alive = false;
                }
            })
            .map_err(|e| e.to_string())?;

        let pm = Self {
            child: Mutex::new(child),
            totals,
        };
        match first_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(s) if s.cpu_mj.is_some() => {
                // The first sample covers the time before the collector started.
                if let Ok(mut t) = pm.totals.lock() {
                    t.cpu_mj = 0.0;
                    t.gpu_mj = 0.0;
                }
                Ok(pm)
            }
            Ok(_) => Err("powermetrics reports no CPU energy on this Mac \
                          (Intel Macs are not supported yet)"
                .into()),
            Err(_) => Err("powermetrics printed nothing within 5 seconds".into()),
        }
    }
}

impl Drop for PowerMetrics {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Cpu,
    /// The GPU and the Neural Engine.
    Gpu,
}

struct AppleSensor {
    info: SourceInfo,
    part: Part,
    pm: Arc<PowerMetrics>,
}

impl PlatformSensor for AppleSensor {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn read(&mut self, _dt: f64) -> Option<Reading> {
        let mut t = self.pm.totals.lock().ok()?;
        if !t.alive {
            return None;
        }
        let mj = match self.part {
            Part::Cpu => std::mem::take(&mut t.cpu_mj),
            Part::Gpu => std::mem::take(&mut t.gpu_mj),
        };
        Some(Reading {
            joules: mj / 1000.0,
        })
    }

    fn stat(&self) -> Option<DeviceStat> {
        if self.part != Part::Gpu {
            return None;
        }
        let t = self.pm.totals.lock().ok()?;
        Some(DeviceStat {
            key: self.info.key.clone(),
            util_pct: t.gpu_util,
            clock_mhz: t.gpu_mhz,
            ..Default::default()
        })
    }
}
