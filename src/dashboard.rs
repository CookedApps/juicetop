//! Everything the screen shows, computed without any terminal code so it can be tested and
//! reused by `juicetop summary`. The TUI only draws a `Fast` and a `Slow` model.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::Duration;

use crate::config::Config;
use crate::pricing::{PriceTable, Pricer};
use crate::report::{
    Calendar, J_PER_KWH, Reader, SlotUnit, Totals, Wall, calendar_slots, local, midnight,
};
use crate::sensors::SourceKind;
use crate::store::Bucket;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    M5,
    H1,
    H24,
    D7,
    D30,
    Y1,
}

impl Range {
    pub const ALL: [Range; 6] = [
        Range::M5,
        Range::H1,
        Range::H24,
        Range::D7,
        Range::D30,
        Range::Y1,
    ];

    pub fn secs(self) -> i64 {
        match self {
            Range::M5 => 300,
            Range::H1 => 3600,
            Range::H24 => 86_400,
            Range::D7 => 7 * 86_400,
            Range::D30 => 30 * 86_400,
            Range::Y1 => 365 * 86_400,
        }
    }

    /// Chart resolution.
    pub fn bucket_secs(self) -> i64 {
        match self {
            Range::M5 => 1,
            Range::H1 => 10,
            Range::H24 => 300,
            Range::D7 => 1800,
            Range::D30 => 7200,
            Range::Y1 => 86_400,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Range::M5 => "5m",
            Range::H1 => "1h",
            Range::H24 => "24h",
            Range::D7 => "7d",
            Range::D30 => "30d",
            Range::Y1 => "1y",
        }
    }

    fn short(self) -> bool {
        self.secs() <= 3600
    }

    /// Bars below the chart: what unit, over which span.
    fn bars(self, now: i64) -> (SlotUnit, i64, &'static str) {
        match self {
            Range::M5 | Range::H1 | Range::H24 => {
                (SlotUnit::Hour, now - 23 * 3600, "per hour, last 24 h")
            }
            Range::D7 => (SlotUnit::Day, now - 6 * 86_400, "per day, last 7 days"),
            Range::D30 => (SlotUnit::Day, now - 29 * 86_400, "per day, last 30 days"),
            Range::Y1 => (
                SlotUnit::Month,
                now - 334 * 86_400,
                "per month, last 12 months",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    /// Index into the palette: 0 = total, then one per source.
    pub color: usize,
    /// (seconds relative to now, watts)
    pub points: Vec<(f64, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComponentRow {
    pub name: String,
    pub color: usize,
    pub now_w: Option<f64>,
    pub avg_w: f64,
    pub kwh: f64,
    pub cost: f64,
    pub co2_g: f64,
    pub share: f64,
    pub estimated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcRow {
    pub name: String,
    pub now_w: Option<f64>,
    pub avg_w: f64,
    pub kwh: f64,
    pub cost: f64,
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub label: String,
    pub kwh: f64,
    pub cost: f64,
}

/// Refreshed every second.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fast {
    pub now: i64,
    pub running: bool,
    pub now_wall_w: Option<f64>,
    pub cost_per_hour: Option<f64>,
    pub price_desc: String,
    pub chart: Vec<Series>,
    pub y_max: f64,
    /// Only for short ranges; long ranges fill this in `Slow`.
    pub components: Vec<ComponentRow>,
    pub range_totals: Option<Totals>,
    pub notes: Vec<String>,
}

/// Refreshed every few seconds: anything that scans weeks of data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Slow {
    pub periods: Vec<(&'static str, Totals)>,
    pub bars: Vec<Bar>,
    pub bars_title: &'static str,
    pub procs: Vec<ProcRow>,
    pub components: Vec<ComponentRow>,
    pub range_totals: Option<Totals>,
    pub first_data: Option<i64>,
}

/// Cache for the expensive "all time before the recent window" sum.
#[derive(Default)]
pub struct AllTimeCache {
    key: Option<(i64, String, usize, Option<i64>)>,
    totals: Totals,
}

const ESTIMATE_BASE: &str = "Base load (est.)";
const ESTIMATE_PSU: &str = "PSU loss (est.)";

pub fn build_fast(
    reader: &Reader,
    cfg: &Config,
    market: Option<&PriceTable>,
    range: Range,
    now: i64,
) -> Result<Fast> {
    let pricer = Pricer::new(cfg, market);
    let from = now - range.secs();
    let buckets = reader.buckets(from, now + 1, range.bucket_secs())?;
    let running = reader.daemon_running(now);

    let mut fast = Fast {
        now,
        running,
        price_desc: pricer.describe(now),
        ..Default::default()
    };

    let now_measured = reader.now_watts();
    if running && !now_measured.is_empty() {
        let w = Wall::of(now_measured.values().sum(), 1.0, cfg).total();
        fast.now_wall_w = Some(w);
        fast.cost_per_hour = Some(w / 1000.0 * pricer.at(now).ct_per_kwh / 100.0);
    }

    // Chart: total at the wall plus each measured source.
    let mut total = Vec::with_capacity(buckets.len());
    let mut per_source: BTreeMap<i64, Vec<(f64, f64)>> = BTreeMap::new();
    for b in buckets.iter().filter(|b| b.secs > 0.0) {
        let x = (b.ts - now) as f64;
        total.push((x, Wall::of(b.measured(), b.secs, cfg).total() / b.secs));
        for (id, j) in &b.joules {
            per_source.entry(*id).or_default().push((x, j / b.secs));
        }
    }
    fast.y_max = total.iter().map(|p| p.1).fold(0.0, f64::max);
    fast.chart.push(Series {
        name: "Total (wall)".into(),
        color: 0,
        points: total,
    });
    for (i, s) in reader.sources.iter().enumerate() {
        if let Some(points) = per_source.remove(&s.id) {
            fast.chart.push(Series {
                name: short_label(&s.info.label, s.info.kind),
                color: i + 1,
                points,
            });
        }
    }

    if range.short() {
        let (rows, totals) = components(&buckets, reader, cfg, &pricer, Some(&now_measured));
        fast.components = rows;
        fast.range_totals = Some(totals);
    }

    if !running {
        fast.notes.push(match &reader.live {
            Some(_) => "collector stopped: no fresh data (start it with `juicetop daemon`)".into(),
            None => "no collector running: start it with `juicetop daemon` or install the service"
                .into(),
        });
    }
    if let Some(live) = &reader.live {
        fast.notes.extend(live.notes.iter().cloned());
    }
    if let Some(e) = &reader.error {
        fast.notes.push(e.clone());
    }
    Ok(fast)
}

pub fn build_slow(
    reader: &Reader,
    cfg: &Config,
    market: Option<&PriceTable>,
    range: Range,
    now: i64,
    cache: &mut AllTimeCache,
) -> Result<Slow> {
    let pricer = Pricer::new(cfg, market);
    let cal = Calendar::at(now);
    let (bar_unit, bars_from, bars_title) = range.bars(now);
    let range_from = now - range.secs();

    // One 15-minute pass covers the periods, the bars and long-range components.
    let seven_days = midnight(local(now).date_naive() - Duration::days(7));
    let mut window_from = cal.month.min(cal.week).min(seven_days).min(bars_from);
    if !range.short() {
        window_from = window_from.min(range_from);
    }
    window_from = midnight(local(window_from).date_naive());
    let fifteen = reader.buckets(window_from, now + 1, 900)?;

    let sum = |from: i64| crate::report::sum(&fifteen, from, now + 1, cfg, &pricer);
    let first_data = reader.first_minute();

    // All time = cached history before the window + the window.
    let fingerprint = toml::to_string(&(&cfg.price, &cfg.co2, &cfg.estimate)).unwrap_or_default();
    let key = (
        window_from,
        fingerprint,
        market.map_or(0, |m| m.intervals.len()),
        first_data,
    );
    if cache.key.as_ref() != Some(&key) {
        cache.totals = match first_data {
            Some(first) if first < window_from => {
                let old = reader.buckets(first, window_from, 900)?;
                crate::report::sum(&old, first, window_from, cfg, &pricer)
            }
            _ => Totals::default(),
        };
        cache.key = Some(key);
    }
    let mut all = cache.totals;
    all.merge(&sum(window_from));

    let last7 = sum(now - 7 * 86_400);
    let mut pace = Totals::default();
    if last7.secs > 0.0 {
        // 30.44 days of the last week's average.
        let f = 30.44 * 86_400.0 / last7.secs;
        pace = Totals {
            joules: last7.joules * f,
            cost: last7.cost * f,
            co2_g: last7.co2_g * f,
            secs: 30.44 * 86_400.0,
            price_fallback: last7.price_fallback,
        };
    }

    let periods = vec![
        ("This hour", sum(cal.hour)),
        ("Today", sum(cal.day)),
        ("This week", sum(cal.week)),
        ("This month", sum(cal.month)),
        ("All time", all),
        ("Month at 7-day pace", pace),
    ];

    let bars = calendar_slots(bars_from, now + 1, bar_unit)
        .into_iter()
        .map(|(from, to, label)| {
            let t = crate::report::sum(&fifteen, from, to, cfg, &pricer);
            Bar {
                label,
                kwh: t.kwh(),
                cost: t.cost,
            }
        })
        .collect();

    let mut slow = Slow {
        periods,
        bars,
        bars_title,
        first_data,
        ..Default::default()
    };

    let range_totals = if range.short() {
        let short = reader.buckets(range_from, now + 1, range.bucket_secs())?;
        crate::report::sum(&short, range_from, now + 1, cfg, &pricer)
    } else {
        let in_range: Vec<Bucket> = fifteen
            .iter()
            .filter(|b| b.ts >= range_from)
            .cloned()
            .collect();
        let (rows, totals) = components(
            &in_range,
            reader,
            cfg,
            &pricer,
            reader
                .daemon_running(now)
                .then(|| reader.now_watts())
                .as_ref(),
        );
        slow.components = rows;
        slow.range_totals = Some(totals);
        totals
    };

    let avg_ct = range_totals
        .avg_ct_per_kwh()
        .unwrap_or_else(|| pricer.at(now).ct_per_kwh);
    let procs = reader.proc_totals(range_from, now + 1, cfg)?;
    let proc_sum: f64 = procs.iter().map(|p| p.1).sum();
    let eff = cfg.estimate.psu_efficiency_percent / 100.0;
    let live: BTreeMap<&str, f64> = reader
        .live
        .as_ref()
        .filter(|_| reader.daemon_running(now))
        .map(|l| l.procs.iter().map(|(n, w)| (n.as_str(), w / eff)).collect())
        .unwrap_or_default();
    let covered = range_totals.secs.max(1.0);
    let mut rows: Vec<ProcRow> = procs
        .into_iter()
        .map(|(name, j)| {
            let kwh = j / J_PER_KWH;
            ProcRow {
                now_w: live.get(name.as_str()).copied(),
                avg_w: j / covered,
                kwh,
                cost: kwh * avg_ct / 100.0,
                share: if proc_sum > 0.0 { j / proc_sum } else { 0.0 },
                name,
            }
        })
        .collect();
    // Processes active right now but not yet in a flushed minute.
    for (name, w) in &live {
        if !rows.iter().any(|r| r.name == *name) {
            rows.push(ProcRow {
                name: name.to_string(),
                now_w: Some(*w),
                avg_w: 0.0,
                kwh: 0.0,
                cost: 0.0,
                share: 0.0,
            });
        }
    }
    slow.procs = rows;
    Ok(slow)
}

/// Rows per measured source plus the two estimates, and the range total.
fn components(
    buckets: &[Bucket],
    reader: &Reader,
    cfg: &Config,
    pricer: &Pricer,
    now_measured: Option<&BTreeMap<i64, f64>>,
) -> (Vec<ComponentRow>, Totals) {
    let mut joules: BTreeMap<i64, f64> = BTreeMap::new();
    let mut cost: BTreeMap<i64, f64> = BTreeMap::new();
    let (mut base_j, mut base_cost, mut psu_j, mut psu_cost) = (0.0, 0.0, 0.0, 0.0);
    let mut totals = Totals::default();
    for b in buckets {
        let ct = pricer.at(b.ts).ct_per_kwh / 100.0 / J_PER_KWH;
        for (id, j) in &b.joules {
            *joules.entry(*id).or_default() += j;
            *cost.entry(*id).or_default() += j * ct;
        }
        let w = Wall::of(b.measured(), b.secs, cfg);
        base_j += w.base;
        base_cost += w.base * ct;
        psu_j += w.psu_loss;
        psu_cost += w.psu_loss * ct;
        totals.add(b, cfg, pricer);
    }
    let secs = totals.secs.max(1.0);
    let total_j = totals.joules.max(f64::MIN_POSITIVE);
    let co2 = cfg.co2.g_per_kwh / J_PER_KWH;

    let now_sum: Option<f64> = now_measured
        .filter(|m| !m.is_empty())
        .map(|m| m.values().sum());
    let now_wall = now_sum.map(|s| Wall::of(s, 1.0, cfg));

    let mut rows = Vec::new();
    for (i, s) in reader.sources.iter().enumerate() {
        let j = joules.get(&s.id).copied().unwrap_or(0.0);
        rows.push(ComponentRow {
            name: short_label(&s.info.label, s.info.kind),
            color: i + 1,
            now_w: now_measured.and_then(|m| m.get(&s.id).copied()),
            avg_w: j / secs,
            kwh: j / J_PER_KWH,
            cost: cost.get(&s.id).copied().unwrap_or(0.0),
            co2_g: j * co2,
            share: j / total_j,
            estimated: false,
        });
    }
    for (name, j, c, now) in [
        (ESTIMATE_BASE, base_j, base_cost, now_wall.map(|w| w.base)),
        (ESTIMATE_PSU, psu_j, psu_cost, now_wall.map(|w| w.psu_loss)),
    ] {
        rows.push(ComponentRow {
            name: name.into(),
            color: usize::MAX,
            now_w: now,
            avg_w: j / secs,
            kwh: j / J_PER_KWH,
            cost: c,
            co2_g: j * co2,
            share: j / total_j,
            estimated: true,
        });
    }
    (rows, totals)
}

/// "AMD Ryzen 9 7950X 16-Core Processor (RAPL)" becomes "CPU Ryzen 9 7950X";
/// "NVIDIA GeForce RTX 4080" becomes "GPU RTX 4080".
pub fn short_label(label: &str, kind: SourceKind) -> String {
    let noise = [
        "AMD",
        "Intel(R)",
        "Core(TM)",
        "NVIDIA",
        "GeForce",
        "Processor",
        "CPU",
        "(R)",
        "(TM)",
    ];
    let words: Vec<&str> = label
        .split(['(', ')'])
        .next()
        .unwrap_or(label)
        .split_whitespace()
        .filter(|w| !noise.contains(w) && !w.ends_with("-Core"))
        .collect();
    let prefix = match kind {
        SourceKind::Cpu => "CPU",
        SourceKind::Gpu => "GPU",
    };
    if words.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix} {}", words.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_labels() {
        assert_eq!(
            short_label(
                "AMD Ryzen 9 7950X 16-Core Processor (RAPL)",
                SourceKind::Cpu
            ),
            "CPU Ryzen 9 7950X"
        );
        assert_eq!(
            short_label("NVIDIA GeForce RTX 4080", SourceKind::Gpu),
            "GPU RTX 4080"
        );
        assert_eq!(short_label("CPU (socket PPT)", SourceKind::Cpu), "CPU");
    }
}
