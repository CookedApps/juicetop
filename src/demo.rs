//! Generated example data: a desktop with a CPU and a GPU that runs a local LLM server
//! during working hours, renders images in the evening and compiles code now and then.
//!
//! Used by `juicetop demo`, the screenshots and the render tests. Everything is derived
//! from the timestamp, so the same moment always produces the same numbers.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use chrono::{Datelike, Timelike, Weekday};

use crate::config::Config;
use crate::live::{LIVE_SECONDS, LiveSample, LiveSnapshot, LiveSource};
use crate::report::local;
use crate::sensors::{SourceInfo, SourceKind};
use crate::store::{MinuteBatch, Store};

const CPU: &str = "AMD Ryzen 9 7950X 16-Core Processor (RAPL)";
const GPU: &str = "NVIDIA GeForce RTX 4080";

/// Average draw of one minute and who caused it.
#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    pub cpu_w: f64,
    pub gpu_w: f64,
    /// (process, CPU watts, GPU watts)
    pub procs: Vec<(&'static str, f64, f64)>,
}

/// Deterministic noise in [0, 1) from any number of keys (splitmix64).
fn noise(keys: &[i64]) -> f64 {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for k in keys {
        x ^= *k as u64;
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
    }
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// The workload at the minute containing `ts`.
pub fn load_at(ts: i64) -> Load {
    let t = local(ts);
    let day = t.num_days_from_ce() as i64;
    let hour = t.hour() as f64 + t.minute() as f64 / 60.0;
    let minute = ts.div_euclid(60);
    let weekend = matches!(t.weekday(), Weekday::Sat | Weekday::Sun);

    let mut procs: Vec<(&'static str, f64, f64)> = Vec::new();
    let awake = (8.0..23.5).contains(&hour);
    procs.push(("[kernel]", 1.5, 0.0));
    procs.push((
        "gnome-shell",
        if awake { 2.5 } else { 0.8 },
        if awake { 4.0 } else { 1.5 },
    ));
    if awake {
        procs.push(("firefox", 3.0 + 4.0 * noise(&[minute, 1]), 1.5));
    }

    // A local LLM server answering requests during working hours, idle but loaded otherwise.
    let llm_day = !weekend && noise(&[day, 10]) < 0.85;
    let work = llm_day && ((9.0..12.5).contains(&hour) || (13.5..18.5).contains(&hour));
    let overnight = noise(&[day, 11]) < 0.3 && (1.0..5.0).contains(&hour);
    if work || overnight {
        // Requests come in waves of about 20 minutes.
        let wave = minute.div_euclid(20);
        let busy =
            noise(&[wave, 12]) < if overnight { 0.9 } else { 0.6 } && noise(&[minute, 14]) < 0.9;
        if busy {
            procs.push(("llama-server", 22.0, 235.0 + 50.0 * noise(&[minute, 13])));
        } else {
            procs.push(("llama-server", 2.0, 14.0));
        }
    } else if llm_day && (8.0..20.0).contains(&hour) {
        procs.push(("llama-server", 0.5, 12.0));
    }

    // Image generation in some evenings.
    if noise(&[day, 20]) < 0.45 && (19.5..22.5).contains(&hour) && noise(&[minute, 21]) < 0.7 {
        procs.push(("python3:main.py", 18.0, 270.0 + 40.0 * noise(&[minute, 22])));
    }

    // Compiling code: short bursts while working.
    if work && noise(&[minute, 30]) < 0.06 {
        procs.push(("rustc", 150.0 + 40.0 * noise(&[minute, 31]), 0.0));
    }

    // Package idle power plus what the processes add; the GPU idles at 17 W.
    let cpu_w = 38.0 + 4.0 * noise(&[minute, 2]) + procs.iter().map(|p| p.1).sum::<f64>();
    let gpu_w = 17.0 + procs.iter().map(|p| p.2).sum::<f64>();
    Load {
        cpu_w,
        gpu_w,
        procs,
    }
}

fn sources() -> Vec<SourceInfo> {
    vec![
        SourceInfo {
            key: "cpu".into(),
            label: CPU.into(),
            kind: SourceKind::Cpu,
        },
        SourceInfo {
            key: "nvidia:0".into(),
            label: GPU.into(),
            kind: SourceKind::Gpu,
        },
    ]
}

/// Per-second wobble around the minute average.
fn second_factor(ts: i64, salt: i64) -> f64 {
    0.9 + 0.2 * noise(&[ts, salt])
}

fn minute_batch(minute: i64, ids: &[i64], with_samples: bool) -> MinuteBatch {
    let load = load_at(minute);
    let mut b = MinuteBatch {
        minute,
        secs: 60,
        ..Default::default()
    };
    b.totals.insert(ids[0], load.cpu_w * 60.0);
    b.totals.insert(ids[1], load.gpu_w * 60.0);
    let busy_cpu: f64 = load.procs.iter().map(|p| p.1).sum();
    let busy_gpu: f64 = load.procs.iter().map(|p| p.2).sum();
    for (name, c, g) in &load.procs {
        // Processes get the whole package energy in proportion to their activity.
        let j =
            load.cpu_w * 60.0 * c / busy_cpu.max(1e-9) + load.gpu_w * 60.0 * g / busy_gpu.max(1e-9);
        *b.procs.entry(name.to_string()).or_default() += j;
    }
    if with_samples {
        for s in 0..60 {
            let ts = minute + s;
            b.samples
                .push((ts, ids[0], load.cpu_w * second_factor(ts, 1)));
            b.samples
                .push((ts, ids[1], load.gpu_w * second_factor(ts, 2)));
        }
    }
    b
}

fn live_snapshot(ids: &[i64], from: i64, now: i64, flushed_until: i64) -> LiveSnapshot {
    let infos = sources();
    let samples = (from.max(now - LIVE_SECONDS as i64 + 1)..=now)
        .map(|ts| {
            let l = load_at(ts);
            LiveSample {
                ts,
                joules: vec![
                    Some(l.cpu_w * second_factor(ts, 1)),
                    Some(l.gpu_w * second_factor(ts, 2)),
                ],
            }
        })
        .collect();
    let l = load_at(now);
    let busy_cpu: f64 = l.procs.iter().map(|p| p.1).sum();
    let busy_gpu: f64 = l.procs.iter().map(|p| p.2).sum();
    let mut procs: Vec<(String, f64)> = l
        .procs
        .iter()
        .map(|(n, c, g)| {
            (
                n.to_string(),
                l.cpu_w * c / busy_cpu.max(1e-9) + l.gpu_w * g / busy_gpu.max(1e-9),
            )
        })
        .collect();
    procs.sort_by(|a, b| b.1.total_cmp(&a.1));
    LiveSnapshot {
        pid: std::process::id(),
        updated: now,
        flushed_until,
        sources: infos
            .into_iter()
            .zip(ids)
            .map(|(info, id)| LiveSource { id: *id, info })
            .collect(),
        samples,
        procs,
        notes: Vec::new(),
    }
}

/// Writes `days` of history ending at `now` into `dir` and returns a config pointing at it.
pub fn seed(dir: &Path, days: i64, now: i64) -> Result<Config> {
    std::fs::create_dir_all(dir)?;
    let db = dir.join("juicetop.db");
    let live = dir.join("live.json");
    let _ = std::fs::remove_file(&db);
    let mut store = Store::open_rw(&db)?;
    store.unsafe_fast_writes();
    let ids = store.register_sources(&sources())?;

    let this_minute = now - now.rem_euclid(60);
    let raw_from = this_minute - 70 * 60;
    let mut batches = Vec::new();
    let mut minute = this_minute - days * 86_400;
    while minute < this_minute {
        batches.push(minute_batch(minute, &ids, minute >= raw_from));
        if batches.len() >= 10_000 {
            store.flush_all(&batches, 0)?;
            batches.clear();
        }
        minute += 60;
    }
    store.flush_all(&batches, 0)?;
    live_snapshot(&ids, this_minute, now, this_minute).write(&live)?;

    let mut cfg = Config::default();
    cfg.data.db_path = Some(db);
    cfg.data.live_path = Some(live);
    Ok(cfg)
}

/// Keeps the demo alive like a running collector: a new sample every second and a database
/// write every minute. Stops when `stop` is set.
pub fn simulate(cfg: &Config, stop: Arc<AtomicBool>) -> Result<()> {
    let (Some(db), Some(live)) = (cfg.data.db_path.clone(), cfg.data.live_path.clone()) else {
        anyhow::bail!("demo config without data paths");
    };
    let mut store = Store::open_rw(&db)?;
    store.unsafe_fast_writes();
    let ids = store.register_sources(&sources())?;
    let start = unix_now();
    let mut flushed_until = start - start.rem_euclid(60);
    let mut ring: VecDeque<i64> = VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        let now = unix_now();
        let minute = now - now.rem_euclid(60);
        if minute > flushed_until {
            let batches: Vec<MinuteBatch> = (0..(minute - flushed_until) / 60)
                .map(|i| minute_batch(flushed_until + i * 60, &ids, true))
                .collect();
            store.flush_all(&batches, now - 48 * 3600)?;
            flushed_until = minute;
        }
        ring.push_back(now);
        while ring.len() > LIVE_SECONDS {
            ring.pop_front();
        }
        live_snapshot(
            &ids,
            flushed_until.min(*ring.front().unwrap_or(&now)),
            now,
            flushed_until,
        )
        .write(&live)?;
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
    Ok(())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_is_deterministic_and_plausible() {
        let ts = 1_795_000_000;
        assert_eq!(load_at(ts), load_at(ts + 5));
        for i in 0..2000 {
            let l = load_at(ts + i * 431);
            assert!((38.0..=300.0).contains(&l.cpu_w), "{l:?}");
            assert!((17.0..=360.0).contains(&l.gpu_w), "{l:?}");
        }
    }

    #[test]
    fn seed_writes_history_and_live_data() {
        let tmp = tempfile::tempdir().unwrap();
        let now = 1_795_000_000;
        let cfg = seed(tmp.path(), 2, now).unwrap();
        let store = Store::open_ro(cfg.data.db_path.as_ref().unwrap()).unwrap();
        assert_eq!(
            store.first_minute().unwrap(),
            Some(now - now.rem_euclid(60) - 2 * 86_400)
        );
        assert!(
            !store
                .second_buckets(now - 3600, now, 60)
                .unwrap()
                .is_empty()
        );
        let live = LiveSnapshot::read(cfg.data.live_path.as_ref().unwrap()).unwrap();
        assert_eq!(live.updated, now);
    }
}
