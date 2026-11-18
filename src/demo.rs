//! Generated example data: an AI workstation with a 96-core CPU and two 96 GB GPUs. One GPU
//! serves a 70B model with vLLM all day, the other fine-tunes overnight and renders images in
//! the afternoon; a small model runs on the CPU and code gets compiled now and then.
//!
//! Used by `juicetop demo`, the screenshots and the render tests. Everything is derived from
//! the timestamp, so the same moment always produces the same numbers.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use chrono::{Datelike, Timelike, Weekday};

use crate::config::Config;
use crate::live::{
    DeviceStat, LIVE_SECONDS, LiveProc, LiveSample, LiveSnapshot, LiveSource, ModelLive,
};
use crate::pricing::{Interval, PriceTable};
use crate::report::local;
use crate::sensors::{SourceInfo, SourceKind};
use crate::store::{MinuteBatch, ProcEnergy, Store, TokenMinute};

const CPU: &str = "AMD Ryzen Threadripper PRO 9995WX 96-Cores (RAPL)";
const GPU: &str = "NVIDIA RTX PRO 6000 Blackwell Workstation Edition";
const CPU_IDLE: f64 = 92.0;
const CPU_LIMIT: f64 = 350.0;
const GPU_IDLE: f64 = 22.0;
const GPU_LIMIT: f64 = 600.0;
const GPU_MEM: f64 = 97_887.0;

const VLLM: &str = "127.0.0.1:8000";
const LLAMA: &str = "127.0.0.1:8080";

/// One process's draw: (name, CPU watts, GPU 0 watts, GPU 1 watts).
pub type ProcLoad = (&'static str, f64, f64, f64);

/// Average draw of one minute and who caused it.
#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    pub cpu_w: f64,
    pub gpu_w: [f64; 2],
    pub procs: Vec<ProcLoad>,
    /// Generated tokens per second (vLLM, llama.cpp).
    pub tps: [f64; 2],
    pub gpu_mem_mb: [f64; 2],
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

/// Smooth-ish value in [0, 1) that changes every `period` seconds.
fn wave(ts: i64, period: i64, salt: i64) -> f64 {
    let i = ts.div_euclid(period);
    let f = ts.rem_euclid(period) as f64 / period as f64;
    let (a, b) = (noise(&[i, salt]), noise(&[i + 1, salt]));
    a + (b - a) * (f * f * (3.0 - 2.0 * f))
}

/// The workload at the minute containing `ts`.
pub fn load_at(ts: i64) -> Load {
    let t = local(ts);
    let day = t.num_days_from_ce() as i64;
    let hour = t.hour() as f64 + t.minute() as f64 / 60.0;
    let minute = ts.div_euclid(60);
    let weekend = matches!(t.weekday(), Weekday::Sat | Weekday::Sun);
    let awake = (7.5..23.5).contains(&hour);

    let mut procs: Vec<ProcLoad> = vec![
        ("[kernel]", 4.0, 0.0, 0.0),
        (
            "gnome-shell",
            if awake { 3.5 } else { 1.0 },
            if awake { 3.0 } else { 0.5 },
            0.0,
        ),
        ("postgres", 2.0 + 2.0 * noise(&[minute, 3]), 0.0, 0.0),
    ];
    if awake {
        procs.push(("firefox", 4.0 + 6.0 * noise(&[minute, 1]), 2.0, 0.0));
        procs.push(("code", 3.0 + 5.0 * noise(&[minute, 4]), 0.5, 0.0));
        procs.push(("pipewire", 1.5, 0.0, 0.0));
    }
    procs.push(("dockerd", 2.5 + 1.5 * noise(&[minute, 5]), 0.0, 0.0));

    // vLLM on GPU 0: a 70B model, always loaded; traffic follows the working day.
    let traffic = if weekend {
        0.25
    } else if (8.5..19.0).contains(&hour) {
        0.95
    } else if (19.0..23.0).contains(&hour) {
        0.55
    } else {
        0.15
    };
    let demand = (traffic * (0.55 + 0.6 * wave(ts, 1200, 12))).min(1.0);
    let vllm_gpu = 65.0 + 480.0 * demand + 25.0 * noise(&[minute, 13]);
    procs.push(("vllm", 6.0 + 30.0 * demand, vllm_gpu, 0.0));
    let vllm_tps = 2600.0 * demand;

    // GPU 1: fine-tuning overnight on most days, image generation in the afternoon.
    let training = noise(&[day, 20]) < 0.8 && (hour >= 22.0 || hour < 7.0)
        || weekend && noise(&[day, 21]) < 0.5 && (10.0..20.0).contains(&hour);
    let mut gpu1_mem = 2_000.0;
    if training {
        procs.push((
            "train.py",
            55.0 + 10.0 * noise(&[minute, 22]),
            0.0,
            545.0 + 40.0 * noise(&[minute, 23]),
        ));
        gpu1_mem = 81_000.0;
    } else if !weekend && (13.0..18.5).contains(&hour) && wave(ts, 900, 24) > 0.35 {
        procs.push((
            "ComfyUI/main.py",
            14.0,
            0.0,
            330.0 + 140.0 * noise(&[minute, 25]),
        ));
        gpu1_mem = 31_000.0;
    }

    // A small coding model on the CPU with llama.cpp, used while working.
    let working = !weekend && (9.0..18.5).contains(&hour);
    let mut llama_tps = 0.0;
    if working && wave(ts, 600, 30) > 0.45 {
        let busy = wave(ts, 120, 31);
        procs.push(("llama-server", 40.0 + 110.0 * busy, 0.0, 0.0));
        llama_tps = 55.0 * busy;
    }

    // Compiling: short bursts.
    if working && noise(&[minute.div_euclid(3), 40]) < 0.12 {
        procs.push(("rustc", 160.0 + 70.0 * noise(&[minute, 41]), 0.0, 0.0));
    }

    let cpu_w = (CPU_IDLE + 6.0 * noise(&[minute, 2]) + procs.iter().map(|p| p.1).sum::<f64>())
        .min(CPU_LIMIT);
    let gpu_w = [
        (GPU_IDLE + procs.iter().map(|p| p.2).sum::<f64>()).min(GPU_LIMIT),
        (GPU_IDLE + procs.iter().map(|p| p.3).sum::<f64>()).min(GPU_LIMIT),
    ];
    Load {
        cpu_w,
        gpu_w,
        procs,
        tps: [vllm_tps, llama_tps],
        gpu_mem_mb: [88_400.0, gpu1_mem],
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
        SourceInfo {
            key: "nvidia:1".into(),
            label: GPU.into(),
            kind: SourceKind::Gpu,
        },
    ]
}

/// Per-second wobble around the minute average.
fn second_factor(ts: i64, salt: i64) -> f64 {
    0.92 + 0.16 * noise(&[ts, salt])
}

/// Splits a minute's energy across processes the way the collector would; a device nobody
/// uses counts as "(idle)".
fn proc_energy(load: &Load, secs: f64) -> Vec<(&'static str, ProcEnergy)> {
    let sum = |i: usize| load.procs.iter().map(|p| [p.1, p.2, p.3][i]).sum::<f64>();
    let (c, g0, g1) = (sum(0), sum(1), sum(2));
    let share = |w: f64, part: f64, total: f64| if total > 0.0 { w * part / total } else { 0.0 };
    let mut out: Vec<(&'static str, ProcEnergy)> = load
        .procs
        .iter()
        .map(|(name, pc, p0, p1)| {
            let gpu = share(load.gpu_w[0], *p0, g0) + share(load.gpu_w[1], *p1, g1);
            let total = share(load.cpu_w, *pc, c) + gpu;
            (
                *name,
                ProcEnergy {
                    total: total * secs,
                    gpu: gpu * secs,
                },
            )
        })
        .collect();
    let idle_gpu = [(g0, load.gpu_w[0]), (g1, load.gpu_w[1])]
        .iter()
        .filter(|(s, _)| *s <= 0.0)
        .map(|(_, w)| w)
        .sum::<f64>();
    let idle_cpu = if c > 0.0 { 0.0 } else { load.cpu_w };
    if idle_gpu + idle_cpu > 0.0 {
        out.push((
            "(idle)",
            ProcEnergy {
                total: (idle_gpu + idle_cpu) * secs,
                gpu: idle_gpu * secs,
            },
        ));
    }
    out
}

fn minute_batch(minute: i64, ids: &[i64], with_samples: bool) -> MinuteBatch {
    let load = load_at(minute);
    let mut b = MinuteBatch {
        minute,
        secs: 60,
        ..Default::default()
    };
    b.totals.insert(ids[0], load.cpu_w * 60.0);
    b.totals.insert(ids[1], load.gpu_w[0] * 60.0);
    b.totals.insert(ids[2], load.gpu_w[1] * 60.0);
    for (name, e) in proc_energy(&load, 60.0) {
        *b.procs.entry(name.to_string()).or_default() += e;
    }
    for (endpoint, process, model, tps, prompt_ratio) in [
        (VLLM, "vllm", "Llama-3.3-70B-Instruct-FP8", load.tps[0], 3.2),
        (
            LLAMA,
            "llama-server",
            "Qwen3-Coder-30B-A3B-Q8_0",
            load.tps[1],
            6.0,
        ),
    ] {
        b.tokens.push(TokenMinute {
            endpoint: endpoint.into(),
            process: process.into(),
            model: model.into(),
            prompt_tokens: (tps * prompt_ratio * 60.0) as u64,
            gen_tokens: (tps * 60.0) as u64,
        });
    }
    if with_samples {
        for s in 0..60 {
            let ts = minute + s;
            let l = load_at(ts);
            b.samples.push((ts, ids[0], l.cpu_w * second_factor(ts, 1)));
            b.samples
                .push((ts, ids[1], l.gpu_w[0] * second_factor(ts, 2)));
            b.samples
                .push((ts, ids[2], l.gpu_w[1] * second_factor(ts, 3)));
        }
    }
    b
}

fn devices(load: &Load) -> Vec<DeviceStat> {
    let cpu_util = ((load.cpu_w - CPU_IDLE) / (CPU_LIMIT - CPU_IDLE) * 100.0).clamp(1.0, 100.0);
    let mut out = vec![DeviceStat {
        key: "cpu".into(),
        util_pct: Some(cpu_util),
        temp_c: Some(41.0 + cpu_util * 0.38),
        clock_mhz: Some(2900.0 + cpu_util * 21.0),
        mem_used_mb: Some(183_500.0),
        mem_total_mb: Some(257_600.0),
        power_limit_w: Some(CPU_LIMIT),
    }];
    for i in 0..2 {
        let util = ((load.gpu_w[i] - GPU_IDLE) / (GPU_LIMIT - 120.0) * 100.0).clamp(0.0, 100.0);
        out.push(DeviceStat {
            key: format!("nvidia:{i}"),
            util_pct: Some(util),
            temp_c: Some(34.0 + util * 0.42),
            clock_mhz: Some(if util < 2.0 {
                210.0
            } else {
                1650.0 + util * 9.5
            }),
            mem_used_mb: Some(load.gpu_mem_mb[i]),
            mem_total_mb: Some(GPU_MEM),
            power_limit_w: Some(GPU_LIMIT),
        });
    }
    out
}

fn live_snapshot(ids: &[i64], from: i64, now: i64, flushed_until: i64) -> LiveSnapshot {
    let samples = (from.max(now - LIVE_SECONDS as i64 + 1)..=now)
        .map(|ts| {
            let l = load_at(ts);
            LiveSample {
                ts,
                joules: vec![
                    Some(l.cpu_w * second_factor(ts, 1)),
                    Some(l.gpu_w[0] * second_factor(ts, 2)),
                    Some(l.gpu_w[1] * second_factor(ts, 3)),
                ],
            }
        })
        .collect();
    let l = load_at(now);
    let mut split: Vec<LiveProc> = proc_energy(&l, 1.0)
        .into_iter()
        .map(|(n, e)| LiveProc {
            name: n.into(),
            total_w: e.total,
            gpu_w: e.gpu,
        })
        .collect();
    split.sort_by(|a, b| b.total_w.total_cmp(&a.total_w));
    let models = vec![
        ModelLive {
            endpoint: VLLM.into(),
            process: "vllm".into(),
            model: "Llama-3.3-70B-Instruct-FP8".into(),
            gen_tps: l.tps[0] * second_factor(now, 7),
            prompt_tps: l.tps[0] * 3.2,
        },
        ModelLive {
            endpoint: LLAMA.into(),
            process: "llama-server".into(),
            model: "Qwen3-Coder-30B-A3B-Q8_0".into(),
            gen_tps: l.tps[1],
            prompt_tps: l.tps[1] * 6.0,
        },
    ];
    LiveSnapshot {
        pid: std::process::id(),
        updated: now,
        flushed_until,
        sources: sources()
            .into_iter()
            .zip(ids)
            .map(|(info, id)| LiveSource { id: *id, info })
            .collect(),
        samples,
        procs: split.iter().map(|p| (p.name.clone(), p.total_w)).collect(),
        notes: Vec::new(),
        proc_split: split,
        devices: devices(&l),
        models,
    }
}

/// Watts per process for each of the last `secs` seconds, as the dashboard records them.
pub fn proc_history(now: i64, secs: i64) -> std::collections::HashMap<String, Vec<f64>> {
    let mut out: std::collections::HashMap<String, Vec<f64>> = std::collections::HashMap::new();
    for ts in now - secs + 1..=now {
        let l = load_at(ts);
        let wobble = second_factor(ts, 9);
        for (name, e) in proc_energy(&l, 1.0) {
            out.entry(name.to_string())
                .or_default()
                .push(e.total * wobble);
        }
    }
    out
}

/// Plausible day-ahead prices around `now`: cheap at night and midday, expensive in the
/// evening peak. Covers the last 400 days and tomorrow.
pub fn market(now: i64) -> PriceTable {
    let mut t = PriceTable::default();
    let start = now - now.rem_euclid(3600) - 400 * 86_400;
    let end = now - now.rem_euclid(86_400) + 2 * 86_400;
    t.merge(
        (0..)
            .map(|h| start + h * 3600)
            .take_while(|s| *s < end)
            .map(|s| {
                let hour = local(s).hour();
                let base = match hour {
                    0..=5 => 72.0,
                    6..=8 => 128.0,
                    9..=15 => 82.0,
                    16..=20 => 168.0,
                    _ => 110.0,
                };
                Interval {
                    start: s,
                    end: s + 3600,
                    eur_per_mwh: base + 30.0 * noise(&[s / 3600, 50]),
                }
            }),
    );
    t
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
    live_snapshot(&ids, now - LIVE_SECONDS as i64, now, this_minute).write(&live)?;

    let mut cfg = Config::default();
    crate::presets::apply(
        &mut cfg,
        crate::presets::by_code("DE").expect("known country"),
    );
    cfg.estimate.base_watts = 85.0;
    cfg.estimate.psu_efficiency_percent = 92.0;
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
        assert_eq!(load_at(ts).procs, load_at(ts).procs);
        for i in 0..3000 {
            let l = load_at(ts + i * 431);
            assert!((CPU_IDLE..=CPU_LIMIT).contains(&l.cpu_w), "{l:?}");
            for g in l.gpu_w {
                assert!((GPU_IDLE..=GPU_LIMIT).contains(&g), "{l:?}");
            }
            let split: f64 = proc_energy(&l, 1.0).iter().map(|p| p.1.total).sum();
            assert!(
                (split - l.cpu_w - l.gpu_w[0] - l.gpu_w[1]).abs() < 1e-6,
                "energy is conserved"
            );
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
        assert!(store.token_totals(now - 86_400, now).unwrap()[0].gen_tokens > 0);
        let live = LiveSnapshot::read(cfg.data.live_path.as_ref().unwrap()).unwrap();
        assert_eq!(live.updated, now);
        assert_eq!(live.devices.len(), 3);
        assert!(market(now).lookup(now).is_some());
    }
}
