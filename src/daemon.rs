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

use crate::live::{LIVE_SECONDS, LiveSample, LiveSnapshot, LiveSource};
use crate::procs::{self, ProcTracker};
use crate::sensors::{self, NvidiaGpu, Sensor, SourceKind};
use crate::store::{MinuteBatch, Store};

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
}

pub fn run(opts: DaemonOpts) -> Result<()> {
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
    let mut tracker = ProcTracker::new();
    if opts.procs {
        tracker.scan();
    }

    let mut batch: Option<MinuteBatch> = None;
    let mut ring: VecDeque<LiveSample> = VecDeque::with_capacity(LIVE_SECONDS + 1);
    let mut proc_ring: VecDeque<HashMap<String, f64>> = VecDeque::with_capacity(PROC_WINDOW + 1);
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
            let done = batch.take().expect("checked above");
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
        for (name, j) in &proc_joules {
            *b.procs.entry(name.clone()).or_default() += j;
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
        let snap = LiveSnapshot {
            pid: std::process::id(),
            updated: ts,
            flushed_until,
            sources: live_sources.clone(),
            samples: ring.iter().cloned().collect(),
            procs: live_proc_watts(&proc_ring),
            notes: notes.clone(),
        };
        if let Err(e) = snap.write(&opts.live) {
            eprintln!("juicetop: {e:#}");
        }
    }

    if let Some(done) = batch.take() {
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
    procs: std::collections::BTreeMap<String, f64>,
    n: usize,
) -> std::collections::BTreeMap<String, f64> {
    let mut v: Vec<(String, f64)> = procs.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    let rest: f64 = v.iter().skip(n).map(|(_, j)| j).sum();
    let mut out: std::collections::BTreeMap<String, f64> = v.into_iter().take(n).collect();
    if rest > 0.0 {
        *out.entry(OTHER.to_string()).or_default() += rest;
    }
    out
}

/// Splits this second's CPU and GPU energy across processes.
fn attribute(
    sensors: &mut [Sensor],
    readings: &[Option<f64>],
    tracker: &mut ProcTracker,
    nvml: Option<&Nvml>,
) -> HashMap<String, f64> {
    let busy = tracker.scan();
    let cpu_weights: HashMap<String, f64> = busy.into_iter().map(|(k, v)| (k, v as f64)).collect();
    let mut out: HashMap<String, f64> = HashMap::new();
    for (sensor, j) in sensors.iter_mut().zip(readings) {
        let Some(j) = *j else { continue };
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
            *out.entry(name).or_default() += j;
        }
    }
    out
}

/// GPU weights per process: SM utilization since the last call, or, when the GPU is idle,
/// the memory each process holds (it keeps the GPU powered up).
fn gpu_weights(nvml: &Nvml, gpu: &mut NvidiaGpu, tracker: &ProcTracker) -> HashMap<String, f64> {
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

fn live_proc_watts(ring: &VecDeque<HashMap<String, f64>>) -> Vec<(String, f64)> {
    let secs = ring.len().max(1) as f64;
    let mut sum: HashMap<&str, f64> = HashMap::new();
    for second in ring {
        for (name, j) in second {
            *sum.entry(name).or_default() += j;
        }
    }
    let mut v: Vec<(String, f64)> = sum
        .into_iter()
        .map(|(n, j)| (n.to_string(), j / secs))
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
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

    #[test]
    fn top_n_lumps_the_tail() {
        let procs = [("a", 5.0), ("b", 3.0), ("c", 1.0), ("d", 0.5)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let out = top_n(procs, 2);
        assert_eq!(out.len(), 3);
        assert_eq!(out[OTHER], 1.5);
        assert_eq!(out["a"], 5.0);
    }

    #[test]
    fn live_watts_average_over_window() {
        let mut ring = VecDeque::new();
        ring.push_back(HashMap::from([("x".to_string(), 10.0)]));
        ring.push_back(HashMap::from([
            ("x".to_string(), 30.0),
            ("y".to_string(), 2.0),
        ]));
        let w = live_proc_watts(&ring);
        assert_eq!(w[0], ("x".to_string(), 20.0));
        assert_eq!(w[1], ("y".to_string(), 1.0));
    }
}
