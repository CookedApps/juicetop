//! Everything the screen shows, computed without any terminal code so it can be tested and
//! reused by `juicetop summary`. The TUI only draws a `Fast` and a `Slow` model.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate};

use crate::config::{Config, PriceMode};
use crate::pricing::{Price, PriceTable, Pricer};
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

    /// Graph resolution.
    pub fn bucket_secs(self) -> i64 {
        match self {
            Range::M5 => 1,
            Range::H1 => 10,
            Range::H24 => 120,
            Range::D7 => 900,
            Range::D30 => 3600,
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

    /// Bars: what unit, from when, and a title.
    fn bars(self, now: i64) -> (SlotUnit, i64, &'static str) {
        match self {
            Range::M5 | Range::H1 | Range::H24 => (SlotUnit::Hour, now - 23 * 3600, "per hour"),
            Range::D7 => (SlotUnit::Day, now - 6 * 86_400, "per day"),
            Range::D30 => (SlotUnit::Day, now - 29 * 86_400, "per day"),
            Range::Y1 => (SlotUnit::Month, now - 334 * 86_400, "per month"),
        }
    }
}

/// What the bars measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarMetric {
    #[default]
    Energy,
    Cost,
    Co2,
}

impl BarMetric {
    pub fn next(self) -> Self {
        match self {
            BarMetric::Energy => BarMetric::Cost,
            BarMetric::Cost => BarMetric::Co2,
            BarMetric::Co2 => BarMetric::Energy,
        }
    }
}

/// A measured source as the screen shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceView {
    pub id: i64,
    pub key: String,
    /// Short name, e.g. "RTX 4080".
    pub name: String,
    pub kind: SourceKind,
    /// Position among sources of the same kind, for colors.
    pub slot: usize,
}

/// One point of the power graph, in watts.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphPoint {
    /// Seconds relative to now (negative).
    pub x: f64,
    pub sources: BTreeMap<i64, f64>,
    pub base: f64,
    pub psu: f64,
    pub total: f64,
}

/// Power right now.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Now {
    pub wall: f64,
    pub sources: BTreeMap<i64, f64>,
    pub base: f64,
    pub psu: f64,
    pub cost_per_hour: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComponentRow {
    pub name: String,
    /// `None` for the estimates.
    pub source: Option<i64>,
    pub now_w: Option<f64>,
    pub avg_w: f64,
    pub kwh: f64,
    pub cost: f64,
    pub co2_g: f64,
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcRow {
    pub name: String,
    pub now_w: Option<f64>,
    pub avg_w: f64,
    pub kwh: f64,
    pub cost: f64,
    pub share: f64,
    /// Part of the energy that came from GPUs, 0 to 1. `None` if unknown.
    pub gpu_share: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub label: String,
    pub start: i64,
    pub end: i64,
    pub kwh: f64,
    pub cost: f64,
    pub co2_g: f64,
    /// Average price paid in this slot.
    pub ct_per_kwh: Option<f64>,
    pub current: bool,
}

impl Bar {
    pub fn value(&self, m: BarMetric) -> f64 {
        match m {
            BarMetric::Energy => self.kwh,
            BarMetric::Cost => self.cost,
            BarMetric::Co2 => self.co2_g,
        }
    }
}

/// A totals card: a period, the same span of the period before, or a projection.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub label: &'static str,
    pub totals: Totals,
    /// The previous period up to the same point, for a trend arrow.
    pub previous: Option<Totals>,
    pub previous_label: &'static str,
}

impl Card {
    /// Relative change of energy against the previous period.
    pub fn trend(&self) -> Option<f64> {
        let p = self.previous?;
        (p.joules > 0.0 && self.totals.joules > 0.0).then(|| self.totals.joules / p.joules - 1.0)
    }
}

/// A local model server with its tokens and the energy of its process.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub endpoint: String,
    pub process: String,
    pub model: String,
    /// Generated tokens per second right now.
    pub now_tps: Option<f64>,
    pub gen_tokens: u64,
    pub prompt_tokens: u64,
    /// Energy of the serving process in the range, at the wall.
    pub kwh: f64,
    pub cost: f64,
    pub wh_per_1k: Option<f64>,
    pub cost_per_1m: Option<f64>,
}

/// Upcoming prices for time of use and dynamic tariffs.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceOutlook {
    /// (slot start, ct per kWh), hourly.
    pub slots: Vec<(i64, f64)>,
    pub now: i64,
    /// Cheapest upcoming window: start, end, average ct per kWh.
    pub cheapest: Option<(i64, i64, f64)>,
    pub window_hours: u32,
}

/// Refreshed every second.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fast {
    pub now: i64,
    pub running: bool,
    pub sources: Vec<SourceView>,
    pub live: Option<Now>,
    pub price: Option<Price>,
    /// Price of the next hour, for a rising or falling hint.
    pub price_next: Option<f64>,
    pub price_desc: String,
    pub graph: Vec<GraphPoint>,
    pub peak_w: f64,
    pub avg_w: f64,
    /// Watts per source over the last minutes, one value per second.
    pub history: BTreeMap<i64, Vec<f64>>,
    /// Only for short ranges; long ranges fill this in `Slow`.
    pub components: Vec<ComponentRow>,
    pub range_totals: Option<Totals>,
    pub notes: Vec<String>,
}

impl Fast {
    pub fn now_wall_w(&self) -> Option<f64> {
        self.live.as_ref().map(|n| n.wall)
    }
}

/// Refreshed every few seconds: anything that scans weeks of data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Slow {
    pub cards: Vec<Card>,
    pub bars: Vec<Bar>,
    pub bars_title: &'static str,
    pub procs: Vec<ProcRow>,
    pub components: Vec<ComponentRow>,
    pub range_totals: Option<Totals>,
    pub first_data: Option<i64>,
    pub prices: Option<PriceOutlook>,
    pub models: Vec<ModelRow>,
}

impl Slow {
    pub fn card(&self, label: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.label == label)
    }
}

/// Cache for the expensive "all time before the recent window" sum.
#[derive(Default)]
pub struct AllTimeCache {
    key: Option<(i64, String, usize, Option<i64>)>,
    totals: Totals,
}

pub const BASE_LOAD: &str = "Base load (est.)";
pub const PSU_LOSS: &str = "PSU loss (est.)";

fn source_views(reader: &Reader) -> Vec<SourceView> {
    let mut counts: BTreeMap<SourceKind, usize> = BTreeMap::new();
    reader
        .sources
        .iter()
        .map(|s| {
            let slot = counts.entry(s.info.kind).or_default();
            let view = SourceView {
                id: s.id,
                key: s.info.key.clone(),
                name: short_label(&s.info.label),
                kind: s.info.kind,
                slot: *slot,
            };
            *slot += 1;
            view
        })
        .collect()
}

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
    let price = pricer.at(now);

    let mut fast = Fast {
        now,
        running,
        sources: source_views(reader),
        price: Some(price),
        price_desc: pricer.describe(now),
        ..Default::default()
    };
    if cfg.price.mode != PriceMode::Fixed {
        let next = pricer.at(now - now.rem_euclid(3600) + 3600);
        fast.price_next = next.exact.then_some(next.ct_per_kwh);
    }

    let measured = reader.now_watts();
    if running && !measured.is_empty() {
        let w = Wall::of(measured.values().sum(), 1.0, cfg);
        fast.live = Some(Now {
            wall: w.total(),
            sources: measured,
            base: w.base,
            psu: w.psu_loss,
            cost_per_hour: w.total() / 1000.0 * price.ct_per_kwh / 100.0,
        });
    }

    let (mut energy, mut secs) = (0.0, 0.0);
    for b in buckets.iter().filter(|b| b.secs > 0.0) {
        let w = Wall::of(b.measured(), b.secs, cfg);
        let total = w.total() / b.secs;
        fast.peak_w = fast.peak_w.max(total);
        energy += w.total();
        secs += b.secs;
        fast.graph.push(GraphPoint {
            x: (b.ts - now) as f64,
            sources: b.joules.iter().map(|(id, j)| (*id, j / b.secs)).collect(),
            base: w.base / b.secs,
            psu: w.psu_loss / b.secs,
            total,
        });
    }
    fast.avg_w = if secs > 0.0 { energy / secs } else { 0.0 };

    if let Some(live) = &reader.live {
        for (i, src) in live.sources.iter().enumerate() {
            let series = live
                .samples
                .iter()
                .filter_map(|s| s.joules.get(i).copied().flatten())
                .collect();
            fast.history.insert(src.id, series);
        }
    }

    if range.short() {
        let (rows, totals) = components(&buckets, reader, cfg, &pricer, fast.live.as_ref());
        fast.components = rows;
        fast.range_totals = Some(totals);
    }

    if !running {
        fast.notes.push(match &reader.live {
            Some(_) => "collector stopped: no fresh data (sudo juicetop service install)".into(),
            None => "no collector running: sudo juicetop service install".into(),
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

fn month_start(date: NaiveDate) -> NaiveDate {
    date.with_day(1).unwrap_or(date)
}

fn previous_month_start(date: NaiveDate) -> NaiveDate {
    let first = month_start(date);
    month_start(first - Duration::days(1))
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
    let today = local(now).date_naive();

    // One 15-minute pass covers the cards (and their previous periods), bars and long ranges.
    let prev_month = midnight(previous_month_start(today));
    let mut window_from = prev_month
        .min(cal.week - 7 * 86_400)
        .min(now - 8 * 86_400)
        .min(bars_from);
    if !range.short() {
        window_from = window_from.min(range_from);
    }
    window_from = midnight(local(window_from).date_naive());
    let fifteen = reader.buckets(window_from, now + 1, 900)?;
    let sum = |from: i64, to: i64| crate::report::sum(&fifteen, from, to, cfg, &pricer);
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
    all.merge(&sum(window_from, now + 1));

    let last7 = sum(now - 7 * 86_400, now + 1);
    let mut projected = Totals::default();
    if last7.secs > 0.0 {
        let f = 30.44 * 86_400.0 / last7.secs;
        projected = Totals {
            joules: last7.joules * f,
            cost: last7.cost * f,
            co2_g: last7.co2_g * f,
            secs: 30.44 * 86_400.0,
            price_fallback: last7.price_fallback,
        };
    }

    let elapsed_month = now - cal.month;
    let cards = vec![
        Card {
            label: "Today",
            totals: sum(cal.day, now + 1),
            previous: Some(sum(cal.day - 86_400, now - 86_400 + 1)),
            previous_label: "yesterday",
        },
        Card {
            label: "This week",
            totals: sum(cal.week, now + 1),
            previous: Some(sum(cal.week - 7 * 86_400, now - 7 * 86_400 + 1)),
            previous_label: "last week",
        },
        Card {
            label: "This month",
            totals: sum(cal.month, now + 1),
            previous: Some(sum(prev_month, prev_month + elapsed_month + 1)),
            previous_label: "last month",
        },
        Card {
            label: "All time",
            totals: all,
            previous: None,
            previous_label: "",
        },
        Card {
            label: "Projected month",
            totals: projected,
            previous: None,
            previous_label: "",
        },
    ];

    let bars = calendar_slots(bars_from, now + 1, bar_unit)
        .into_iter()
        .map(|(from, to, label)| {
            let t = sum(from, to);
            Bar {
                label,
                start: from,
                end: to,
                kwh: t.kwh(),
                cost: t.cost,
                co2_g: t.co2_g,
                ct_per_kwh: t.avg_ct_per_kwh(),
                current: from <= now && now < to,
            }
        })
        .collect();

    let mut slow = Slow {
        cards,
        bars,
        bars_title,
        first_data,
        ..Default::default()
    };

    let now_live = reader
        .daemon_running(now)
        .then(|| reader.now_watts())
        .filter(|m| !m.is_empty())
        .map(|m| {
            let w = Wall::of(m.values().sum(), 1.0, cfg);
            Now {
                wall: w.total(),
                sources: m,
                base: w.base,
                psu: w.psu_loss,
                cost_per_hour: 0.0,
            }
        });
    let range_totals = if range.short() {
        let short = reader.buckets(range_from, now + 1, range.bucket_secs())?;
        crate::report::sum(&short, range_from, now + 1, cfg, &pricer)
    } else {
        let in_range: Vec<Bucket> = fifteen
            .iter()
            .filter(|b| b.ts >= range_from)
            .cloned()
            .collect();
        let (rows, totals) = components(&in_range, reader, cfg, &pricer, now_live.as_ref());
        slow.components = rows;
        slow.range_totals = Some(totals);
        totals
    };

    slow.procs = processes(
        reader,
        cfg,
        range_from,
        now,
        &range_totals,
        pricer.at(now).ct_per_kwh,
    )?;
    slow.prices = price_outlook(cfg, &pricer, market, now);
    slow.models = models(reader, &slow.procs, range_from, now)?;
    Ok(slow)
}

/// Energy per token: the serving process's energy in the range divided by its generated tokens.
/// Idle time with the model loaded counts too, because it is part of what serving costs.
fn models(reader: &Reader, procs: &[ProcRow], from: i64, now: i64) -> Result<Vec<ModelRow>> {
    let live: Vec<crate::live::ModelLive> = reader
        .live
        .as_ref()
        .filter(|_| reader.daemon_running(now))
        .map(|l| l.models.clone())
        .unwrap_or_default();
    let mut rows: Vec<ModelRow> = reader
        .token_totals(from, now + 1)?
        .into_iter()
        .map(|t| {
            // `process` lists the server and its child processes, comma separated.
            let (kwh, cost) = procs
                .iter()
                .filter(|p| t.process.split(',').any(|n| n == p.name))
                .fold((0.0, 0.0), |(k, c), p| (k + p.kwh, c + p.cost));
            let per =
                |v: f64, n: f64| (t.gen_tokens > 0 && v > 0.0).then(|| v / t.gen_tokens as f64 * n);
            ModelRow {
                now_tps: live
                    .iter()
                    .find(|m| m.endpoint == t.endpoint)
                    .map(|m| m.gen_tps),
                wh_per_1k: per(kwh * 1000.0, 1000.0),
                cost_per_1m: per(cost, 1e6),
                endpoint: t.endpoint,
                process: t.process,
                model: t.model,
                gen_tokens: t.gen_tokens,
                prompt_tokens: t.prompt_tokens,
                kwh,
                cost,
            }
        })
        .collect();
    for m in live {
        if !rows.iter().any(|r| r.endpoint == m.endpoint) {
            rows.push(ModelRow {
                endpoint: m.endpoint,
                process: m.process,
                model: m.model,
                now_tps: Some(m.gen_tps),
                gen_tokens: 0,
                prompt_tokens: 0,
                kwh: 0.0,
                cost: 0.0,
                wh_per_1k: None,
                cost_per_1m: None,
            });
        }
    }
    Ok(rows)
}

fn processes(
    reader: &Reader,
    cfg: &Config,
    from: i64,
    now: i64,
    range_totals: &Totals,
    now_ct: f64,
) -> Result<Vec<ProcRow>> {
    let avg_ct = range_totals.avg_ct_per_kwh().unwrap_or(now_ct);
    let procs = reader.proc_totals(from, now + 1, cfg)?;
    let proc_sum: f64 = procs.iter().map(|p| p.joules).sum();
    let eff = cfg.estimate.psu_efficiency_percent / 100.0;
    let live: BTreeMap<&str, (f64, f64)> = reader
        .live
        .as_ref()
        .filter(|_| reader.daemon_running(now))
        .map(|l| {
            l.proc_watts()
                .into_iter()
                .map(|(n, total, gpu)| (n, (total / eff, gpu / eff)))
                .collect()
        })
        .unwrap_or_default();
    let covered = range_totals.secs.max(1.0);
    let mut rows: Vec<ProcRow> = procs
        .into_iter()
        .map(|p| {
            let kwh = p.joules / J_PER_KWH;
            ProcRow {
                now_w: live.get(p.name.as_str()).map(|l| l.0),
                avg_w: p.joules / covered,
                kwh,
                cost: kwh * avg_ct / 100.0,
                share: if proc_sum > 0.0 {
                    p.joules / proc_sum
                } else {
                    0.0
                },
                gpu_share: p
                    .gpu_joules
                    .map(|g| if p.joules > 0.0 { g / p.joules } else { 0.0 }),
                name: p.name,
            }
        })
        .collect();
    // Processes active right now but not yet in a flushed minute.
    for (name, (w, gpu)) in &live {
        if let Some(r) = rows.iter_mut().find(|r| r.name == *name) {
            if r.gpu_share.is_none() && *w > 0.0 {
                r.gpu_share = Some(gpu / w);
            }
        } else {
            rows.push(ProcRow {
                name: name.to_string(),
                now_w: Some(*w),
                avg_w: 0.0,
                kwh: 0.0,
                cost: 0.0,
                share: 0.0,
                gpu_share: (*w > 0.0).then(|| gpu / w),
            });
        }
    }
    Ok(rows)
}

/// Hourly prices from 12 hours ago to the last known hour, and the cheapest upcoming window.
pub fn price_outlook(
    cfg: &Config,
    pricer: &Pricer,
    market: Option<&PriceTable>,
    now: i64,
) -> Option<PriceOutlook> {
    let hour = now - now.rem_euclid(3600);
    let end = match cfg.price.mode {
        PriceMode::Fixed => return None,
        PriceMode::TimeOfUse => hour + 24 * 3600,
        PriceMode::Dynamic => {
            let last = market.and_then(|m| m.last_end())?;
            (last - last.rem_euclid(3600)).max(hour + 3600)
        }
    };
    let slots: Vec<(i64, f64)> = (0..)
        .map(|i| hour - 12 * 3600 + i * 3600)
        .take_while(|t| *t < end)
        .filter_map(|t| {
            // Average the quarter hours, in case the market trades 15-minute products.
            let prices: Vec<Price> = (0..4).map(|q| pricer.at(t + q * 900 + 1)).collect();
            prices
                .iter()
                .all(|p| p.exact)
                .then(|| (t, prices.iter().map(|p| p.ct_per_kwh).sum::<f64>() / 4.0))
        })
        .collect();
    if slots.is_empty() {
        return None;
    }
    let n = cfg.price.cheapest_window_hours.max(1) as usize;
    let upcoming: Vec<&(i64, f64)> = slots.iter().filter(|s| s.0 >= hour).collect();
    let cheapest = upcoming
        .windows(n)
        .filter(|w| w.windows(2).all(|p| p[1].0 - p[0].0 == 3600))
        .map(|w| {
            (
                w[0].0,
                w[n - 1].0 + 3600,
                w.iter().map(|s| s.1).sum::<f64>() / n as f64,
            )
        })
        .min_by(|a, b| a.2.total_cmp(&b.2));
    Some(PriceOutlook {
        slots,
        now,
        cheapest,
        window_hours: n as u32,
    })
}

/// Rows per measured source plus the two estimates, and the range total.
fn components(
    buckets: &[Bucket],
    reader: &Reader,
    cfg: &Config,
    pricer: &Pricer,
    now: Option<&Now>,
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

    let mut rows = Vec::new();
    for s in &reader.sources {
        let j = joules.get(&s.id).copied().unwrap_or(0.0);
        rows.push(ComponentRow {
            name: short_label(&s.info.label),
            source: Some(s.id),
            now_w: now.and_then(|n| n.sources.get(&s.id).copied()),
            avg_w: j / secs,
            kwh: j / J_PER_KWH,
            cost: cost.get(&s.id).copied().unwrap_or(0.0),
            co2_g: j * co2,
            share: j / total_j,
        });
    }
    for (name, j, c, now_w) in [
        (BASE_LOAD, base_j, base_cost, now.map(|n| n.base)),
        (PSU_LOSS, psu_j, psu_cost, now.map(|n| n.psu)),
    ] {
        rows.push(ComponentRow {
            name: name.into(),
            source: None,
            now_w,
            avg_w: j / secs,
            kwh: j / J_PER_KWH,
            cost: c,
            co2_g: j * co2,
            share: j / total_j,
        });
    }
    (rows, totals)
}

/// Strips vendor noise: "AMD Ryzen 9 7950X 16-Core Processor (RAPL)" becomes "Ryzen 9 7950X",
/// "NVIDIA GeForce RTX 4080" becomes "RTX 4080".
pub fn short_label(label: &str) -> String {
    let noise = [
        "AMD",
        "Intel(R)",
        "Intel",
        "Core(TM)",
        "NVIDIA",
        "GeForce",
        "Processor",
        "CPU",
        "(R)",
        "(TM)",
        "Workstation",
        "Edition",
    ];
    let words: Vec<&str> = label
        .split(['(', ')'])
        .next()
        .unwrap_or(label)
        .split_whitespace()
        .filter(|w| !noise.contains(w) && !w.ends_with("-Core") && !w.ends_with("-Cores"))
        .collect();
    if words.is_empty() {
        "CPU".into()
    } else {
        words.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_labels() {
        assert_eq!(
            short_label("AMD Ryzen 9 7950X 16-Core Processor (RAPL)"),
            "Ryzen 9 7950X"
        );
        assert_eq!(short_label("NVIDIA GeForce RTX 4080"), "RTX 4080");
        assert_eq!(short_label("CPU (socket PPT)"), "CPU");
    }

    #[test]
    fn cheapest_window_is_contiguous_and_upcoming() {
        use crate::pricing::Interval;
        let mut cfg = Config::default();
        cfg.price.mode = PriceMode::Dynamic;
        cfg.price.cheapest_window_hours = 2;
        cfg.price.dynamic.markup_ct_per_kwh = 0.0;
        cfg.price.dynamic.vat_percent = 0.0;
        let now = 100 * 3600 + 600;
        let mut table = PriceTable::default();
        // Cheapest hours are in the past (hour 95) and at hours 102 and 103.
        let prices = [
            (95, 10.0),
            (100, 300.0),
            (101, 200.0),
            (102, 50.0),
            (103, 60.0),
            (104, 400.0),
        ];
        table.merge((90..106).map(|h| {
            let p = prices.iter().find(|p| p.0 == h).map_or(500.0, |p| p.1);
            Interval {
                start: h * 3600,
                end: (h + 1) * 3600,
                eur_per_mwh: p,
            }
        }));
        let pricer = Pricer::new(&cfg, Some(&table));
        let o = price_outlook(&cfg, &pricer, Some(&table), now).unwrap();
        let (start, end, avg) = o.cheapest.unwrap();
        assert_eq!((start / 3600, end / 3600), (102, 104));
        assert!((avg - 5.5).abs() < 1e-9);
        assert_eq!(o.slots.first().unwrap().0, 90 * 3600);
        assert!(price_outlook(&Config::default(), &pricer, None, now).is_none());
    }

    #[test]
    fn trend_compares_energy() {
        let t = |j: f64| Totals {
            joules: j,
            ..Default::default()
        };
        let c = Card {
            label: "Today",
            totals: t(110.0),
            previous: Some(t(100.0)),
            previous_label: "yesterday",
        };
        assert!((c.trend().unwrap() - 0.1).abs() < 1e-9);
    }
}
