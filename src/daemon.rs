//! The collector: samples every sensor once per wall-clock second, attributes energy to
//! processes, publishes a live snapshot every second and writes the database once a minute.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use nvml_wrapper::Nvml;
use nvml_wrapper::enums::device::UsedGpuMemory;

use crate::devices::DeviceStats;
use crate::live::{LIVE_SECONDS, LiveProc, LiveSample, LiveSnapshot, LiveSource};
use crate::models::ModelWatcher;
use crate::procs::{self, Tracker};
use crate::sensors::{self, NvidiaGpu, Sensor, SourceKind};
use crate::store::{MinuteBatch, ProcEnergy, Store};

/// Process names kept per minute; the rest is summed into `OTHER`.
const TOP_PROCS_PER_MINUTE: usize = 30;
const OTHER: &str = "(other)";
const GPU_UNATTRIBUTED: &str = "(gpu)";
/// Seconds averaged for the live per-process watts.
const PROC_WINDOW: usize = 5;

pub struct DaemonOpts {
    pub db: PathBuf,
    pub live: PathBuf,
    pub raw_retention_hours: u64,
    pub procs: bool,
    /// Look for local model servers and count their tokens.
    pub models: bool,
    /// Extra Prometheus metrics URLs of model servers.
    pub model_endpoints: Vec<String>,
}

pub fn run(opts: DaemonOpts) -> Result<()> {
    #[cfg(windows)]
    crate::windows::detach_console();
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stop))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop))?;

    let nvml = Nvml::init().ok();
    let (mut sensors, notes) = sensors::probe(nvml.as_ref());
    for n in &notes {
        eprintln!("juicetop: {n}");
    }
    if sensors.is_empty() {
        bail!("no power sensors found");
    }

    let mut store = Store::open_rw(&opts.db)?;
    let infos: Vec<_> = sensors.iter().map(|s| s.info().clone()).collect();
    let ids = store.register_sources(&infos)?;
    for (info, id) in infos.iter().zip(&ids) {
        eprintln!("juicetop: source {id} {} = {}", info.key, info.label);
    }
    if let Some(dir) = opts.live.parent() {
        std::fs::create_dir_all(dir)?;
    }
    eprintln!(
        "juicetop: writing {} and {}",
        opts.db.display(),
        opts.live.display()
    );

    let live_sources: Vec<LiveSource> = infos
        .iter()
        .zip(&ids)
        .map(|(info, id)| LiveSource {
            id: *id,
            info: info.clone(),
        })
        .collect();
    let retention = opts.raw_retention_hours as i64 * 3600;
    let mut tracker = Tracker::new();
    if opts.procs {
        tracker.scan();
    }

    let mut devices = DeviceStats::new();
    let models = opts
        .models
        .then(|| ModelWatcher::start(opts.model_endpoints.clone()));
    let mut batch: Option<MinuteBatch> = None;
    let mut ring: VecDeque<LiveSample> = VecDeque::with_capacity(LIVE_SECONDS + 1);
    let mut proc_ring: VecDeque<HashMap<String, ProcEnergy>> =
        VecDeque::with_capacity(PROC_WINDOW + 1);
    let mut flushed_until = unix_now().0;
    let mut last = std::time::Instant::now();

    while !stop.load(Ordering::Relaxed) {
        if !sleep_to_next_second(&stop) {
            break;
        }
        let now = std::time::Instant::now();
        let dt = now.duration_since(last).as_secs_f64();
        last = now;
        let ts = unix_now().0;

        let readings: Vec<Option<f64>> = sensors
            .iter_mut()
            .map(|s| s.read(nvml.as_ref(), dt).map(|r| r.joules))
            .collect();

        let proc_joules = if opts.procs {
            attribute(&mut sensors, &readings, &mut tracker, nvml.as_ref())
        } else {
            HashMap::new()
        };

        // Close the previous minute when a new one starts.
        let minute = ts.div_euclid(60) * 60;
        if batch.as_ref().is_some_and(|b| b.minute != minute) {
            let mut done = batch.take().expect("checked above");
            if let Some(m) = &models {
                done.tokens = m.take_minute();
            }
            write_minute(&mut store, done, ts - retention);
            flushed_until = minute;
        }
        let b = batch.get_or_insert_with(|| MinuteBatch {
            minute,
            ..Default::default()
        });
        b.secs += 1;
        for (j, id) in readings.iter().zip(&ids) {
            if let Some(j) = j {
                b.samples.push((ts, *id, *j));
                *b.totals.entry(*id).or_default() += j;
            }
        }
        for (name, e) in &proc_joules {
            *b.procs.entry(name.clone()).or_default() += *e;
        }

        ring.push_back(LiveSample {
            ts,
            joules: readings,
        });
        while ring.len() > LIVE_SECONDS {
            ring.pop_front();
        }
        proc_ring.push_back(proc_joules);
        while proc_ring.len() > PROC_WINDOW {
            proc_ring.pop_front();
        }
        let split = live_proc_watts(&proc_ring);
        let snap = LiveSnapshot {
            pid: std::process::id(),
            updated: ts,
            flushed_until,
            sources: live_sources.clone(),
            samples: ring.iter().cloned().collect(),
            procs: split.iter().map(|p| (p.name.clone(), p.total_w)).collect(),
            notes: notes.clone(),
            proc_split: split,
            devices: devices.collect(&sensors, nvml.as_ref()),
            models: models.as_ref().map(|m| m.live()).unwrap_or_default(),
        };
        if let Err(e) = snap.write(&opts.live) {
            eprintln!("juicetop: {e:#}");
        }
    }

    if let Some(mut done) = batch.take() {
        if let Some(m) = &models {
            done.tokens = m.take_minute();
        }
        let until = unix_now().0;
        write_minute(&mut store, done, until - retention);
        let snap = LiveSnapshot {
            pid: std::process::id(),
            updated: until,
            flushed_until: until + 1,
            sources: live_sources,
            samples: Vec::new(),
            procs: Vec::new(),
            notes,
            proc_split: Vec::new(),
            devices: Vec::new(),
            models: Vec::new(),
        };
        let _ = snap.write(&opts.live);
    }
    eprintln!("juicetop: stopped");
    Ok(())
}

fn write_minute(store: &mut Store, mut batch: MinuteBatch, raw_cutoff: i64) {
    batch.procs = top_n(std::mem::take(&mut batch.procs), TOP_PROCS_PER_MINUTE);
    if let Err(e) = store.flush(&batch, raw_cutoff) {
        eprintln!("juicetop: database write failed: {e:#}");
    }
}

/// Keeps the `n` largest entries and sums the rest into `OTHER`.
fn top_n(
    procs: std::collections::BTreeMap<String, ProcEnergy>,
    n: usize,
) -> std::collections::BTreeMap<String, ProcEnergy> {
    let mut v: Vec<(String, ProcEnergy)> = procs.into_iter().collect();
    v.sort_by(|a, b| b.1.total.total_cmp(&a.1.total));
    let mut rest = ProcEnergy::default();
    for (_, e) in v.iter().skip(n) {
        rest += *e;
    }
    let mut out: std::collections::BTreeMap<String, ProcEnergy> = v.into_iter().take(n).collect();
    if rest.total > 0.0 {
        *out.entry(OTHER.to_string()).or_default() += rest;
    }
    out
}

/// Splits this second's CPU and GPU energy across processes.
fn attribute(
    sensors: &mut [Sensor],
    readings: &[Option<f64>],
    tracker: &mut Tracker,
    nvml: Option<&Nvml>,
) -> HashMap<String, ProcEnergy> {
    let busy = tracker.scan();
    let cpu_weights: HashMap<String, f64> = busy.into_iter().map(|(k, v)| (k, v as f64)).collect();
    let mut out: HashMap<String, ProcEnergy> = HashMap::new();
    for (sensor, j) in sensors.iter_mut().zip(readings) {
        let Some(j) = *j else { continue };
        let is_gpu = sensor.info().kind == SourceKind::Gpu;
        let parts = match sensor {
            Sensor::Nvidia(gpu) => {
                let weights = nvml
                    .map(|n| gpu_weights(n, gpu, tracker))
                    .unwrap_or_default();
                procs::split(j, &weights, procs::IDLE)
            }
            s if s.info().kind == SourceKind::Cpu => procs::split(j, &cpu_weights, procs::IDLE),
            _ => procs::split(j, &HashMap::new(), GPU_UNATTRIBUTED),
        };
        for (name, j) in parts {
            *out.entry(name).or_default() += ProcEnergy {
                total: j,
                gpu: if is_gpu { j } else { 0.0 },
            };
        }
    }
    out
}

/// GPU weights per process: SM utilization since the last call, or, when the GPU is idle,
/// the memory each process holds (it keeps the GPU powered up).
fn gpu_weights(nvml: &Nvml, gpu: &mut NvidiaGpu, tracker: &Tracker) -> HashMap<String, f64> {
    let mut w: HashMap<String, f64> = HashMap::new();
    let Ok(dev) = nvml.device_by_index(gpu.index) else {
        return w;
    };
    let samples = dev
        .process_utilization_stats(gpu.last_util_ts)
        .unwrap_or_default();
    for s in samples {
        gpu.last_util_ts = gpu.last_util_ts.max(s.timestamp);
        if s.sm_util > 0 {
            *w.entry(tracker.name_of(s.pid)).or_default() += s.sm_util as f64;
        }
    }
    if w.is_empty() {
        let compute = dev.running_compute_processes().unwrap_or_default();
        let graphics = dev.running_graphics_processes().unwrap_or_default();
        for p in compute.into_iter().chain(graphics) {
            if let UsedGpuMemory::Used(bytes) = p.used_gpu_memory {
                *w.entry(tracker.name_of(p.pid)).or_default() += bytes as f64;
            }
        }
    }
    w
}

fn live_proc_watts(ring: &VecDeque<HashMap<String, ProcEnergy>>) -> Vec<LiveProc> {
    let secs = ring.len().max(1) as f64;
    let mut sum: HashMap<&str, ProcEnergy> = HashMap::new();
    for second in ring {
        for (name, e) in second {
            *sum.entry(name).or_default() += *e;
        }
    }
    let mut v: Vec<LiveProc> = sum
        .into_iter()
        .map(|(n, e)| LiveProc {
            name: n.to_string(),
            total_w: e.total / secs,
            gpu_w: e.gpu / secs,
        })
        .collect();
    v.sort_by(|a, b| b.total_w.total_cmp(&a.total_w));
    v.truncate(25);
    v
}

/// Sleeps until just after the next whole second, waking early if asked to stop.
/// Returns false when stopping.
fn sleep_to_next_second(stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let (_, nanos) = unix_now();
        let remaining = Duration::from_nanos(1_000_000_000 - nanos as u64);
        // Aim 5 ms past the boundary so `ts` is the new second.
        let target = remaining + Duration::from_millis(5);
        if target <= Duration::from_millis(250) {
            std::thread::sleep(target);
            return !stop.load(Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn unix_now() -> (i64, u32) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(total: f64, gpu: f64) -> ProcEnergy {
        ProcEnergy { total, gpu }
    }

    #[test]
    fn top_n_lumps_the_tail() {
        let procs = [("a", 5.0), ("b", 3.0), ("c", 1.0), ("d", 0.5)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), e(v, v / 2.0)))
            .collect();
        let out = top_n(procs, 2);
        assert_eq!(out.len(), 3);
        assert_eq!(out[OTHER], e(1.5, 0.75));
        assert_eq!(out["a"].total, 5.0);
    }

    #[test]
    fn live_watts_average_over_window() {
        let mut ring = VecDeque::new();
        ring.push_back(HashMap::from([("x".to_string(), e(10.0, 10.0))]));
        ring.push_back(HashMap::from([
            ("x".to_string(), e(30.0, 0.0)),
            ("y".to_string(), e(2.0, 0.0)),
        ]));
        let w = live_proc_watts(&ring);
        assert_eq!(
            w[0],
            LiveProc {
                name: "x".into(),
                total_w: 20.0,
                gpu_w: 5.0
            }
        );
        assert_eq!(
            w[1],
            LiveProc {
                name: "y".into(),
                total_w: 1.0,
                gpu_w: 0.0
            }
        );
    }
}
