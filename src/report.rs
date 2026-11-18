//! Turns stored joules into what people want to read: watts, kWh, money and CO2,
//! including the parts without a sensor (base load, power supply losses).
//! Shared by the TUI and the `summary` command.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};

use crate::config::Config;
use crate::live::{DataPaths, LiveSnapshot};
use crate::pricing::Pricer;
use crate::store::{Bucket, ProcTotal, Store, StoredSource, TokenTotal};

pub const J_PER_KWH: f64 = 3_600_000.0;

/// A live snapshot older than this means the daemon is not running.
pub const LIVE_STALE_SECS: i64 = 10;

/// Energy of one bucket split into measured parts and estimates, all in joules.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Wall {
    pub measured: f64,
    pub base: f64,
    pub psu_loss: f64,
}

impl Wall {
    pub fn of(measured: f64, secs: f64, cfg: &Config) -> Self {
        let base = cfg.estimate.base_watts * secs;
        let dc = measured + base;
        let eff = cfg.estimate.psu_efficiency_percent / 100.0;
        Self {
            measured,
            base,
            psu_loss: dc / eff - dc,
        }
    }

    pub fn total(&self) -> f64 {
        self.measured + self.base + self.psu_loss
    }
}

/// Energy, money and CO2 summed over some buckets.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Totals {
    /// Joules at the wall (measured plus estimates).
    pub joules: f64,
    /// Cost in currency units (not cents).
    pub cost: f64,
    pub co2_g: f64,
    pub secs: f64,
    /// False when some cost used a fallback price.
    pub price_fallback: bool,
}

impl Totals {
    pub fn add(&mut self, b: &Bucket, cfg: &Config, pricer: &Pricer) {
        let wall = Wall::of(b.measured(), b.secs, cfg).total();
        let kwh = wall / J_PER_KWH;
        let price = pricer.at(b.ts);
        self.joules += wall;
        self.cost += kwh * price.ct_per_kwh / 100.0;
        self.co2_g += kwh * cfg.co2.g_per_kwh;
        self.secs += b.secs;
        self.price_fallback |= !price.exact;
    }

    pub fn merge(&mut self, o: &Totals) {
        self.joules += o.joules;
        self.cost += o.cost;
        self.co2_g += o.co2_g;
        self.secs += o.secs;
        self.price_fallback |= o.price_fallback;
    }

    pub fn kwh(&self) -> f64 {
        self.joules / J_PER_KWH
    }

    pub fn avg_watts(&self) -> f64 {
        if self.secs > 0.0 {
            self.joules / self.secs
        } else {
            0.0
        }
    }

    /// Average price actually paid, in cents per kWh.
    pub fn avg_ct_per_kwh(&self) -> Option<f64> {
        (self.joules > 0.0).then(|| self.cost * 100.0 / self.kwh())
    }
}

pub fn sum(buckets: &[Bucket], from: i64, to: i64, cfg: &Config, pricer: &Pricer) -> Totals {
    let mut t = Totals::default();
    for b in buckets.iter().filter(|b| b.ts >= from && b.ts < to) {
        t.add(b, cfg, pricer);
    }
    t
}

/// Reads the database and the live snapshot together.
pub struct Reader {
    pub paths: DataPaths,
    store: Option<Store>,
    pub live: Option<LiveSnapshot>,
    pub sources: Vec<StoredSource>,
    pub error: Option<String>,
}

impl Reader {
    pub fn new(paths: DataPaths) -> Self {
        let mut r = Self {
            paths,
            store: None,
            live: None,
            sources: Vec::new(),
            error: None,
        };
        r.refresh();
        r
    }

    /// Re-reads the live snapshot and (re)opens the database if needed.
    pub fn refresh(&mut self) {
        self.live = LiveSnapshot::read(&self.paths.live).ok();
        if self.store.is_none() && self.paths.db.exists() {
            match Store::open_ro(&self.paths.db) {
                Ok(s) => self.store = Some(s),
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
        if let Some(store) = &self.store {
            match store.sources() {
                Ok(s) => {
                    self.sources = s;
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
        // Without a database yet, the live snapshot still names the sources.
        if self.sources.is_empty()
            && let Some(live) = &self.live
        {
            self.sources = live
                .sources
                .iter()
                .map(|s| StoredSource {
                    id: s.id,
                    info: s.info.clone(),
                })
                .collect();
        }
    }

    pub fn daemon_running(&self, now: i64) -> bool {
        self.live
            .as_ref()
            .is_some_and(|l| now - l.updated <= LIVE_STALE_SECS && !l.samples.is_empty())
    }

    /// Everything before this is in the database; everything after comes from the live file.
    fn flushed_until(&self) -> i64 {
        self.live
            .as_ref()
            .map(|l| l.flushed_until)
            .unwrap_or(i64::MAX)
    }

    /// Buckets of `bucket_secs` over `[from, to)`. Below a minute raw samples are used, which
    /// exist only for the raw retention window.
    pub fn buckets(&self, from: i64, to: i64, bucket_secs: i64) -> Result<Vec<Bucket>> {
        let bucket_secs = bucket_secs.max(1);
        let split = self.flushed_until().clamp(from, to);
        let mut map: BTreeMap<i64, Bucket> = BTreeMap::new();
        if let Some(store) = &self.store {
            let db = if bucket_secs < 60 {
                store.second_buckets(from, split, bucket_secs)?
            } else {
                store.minute_buckets(from, split, bucket_secs)?
            };
            map.extend(db.into_iter().map(|b| (b.ts, b)));
        }
        if let Some(live) = &self.live {
            for s in live.samples.iter().filter(|s| s.ts >= split && s.ts < to) {
                let key = s.ts.div_euclid(bucket_secs) * bucket_secs;
                let b = map.entry(key).or_insert_with(|| Bucket::new(key));
                b.secs += 1.0;
                for (src, j) in live.sources.iter().zip(&s.joules) {
                    if let Some(j) = j {
                        *b.joules.entry(src.id).or_default() += j;
                    }
                }
            }
        }
        Ok(map.into_values().collect())
    }

    /// Process energy at the wall in `[from, to)`, largest first. Excludes the unflushed minute.
    pub fn proc_totals(&self, from: i64, to: i64, cfg: &Config) -> Result<Vec<ProcTotal>> {
        let Some(store) = &self.store else {
            return Ok(Vec::new());
        };
        let eff = cfg.estimate.psu_efficiency_percent / 100.0;
        Ok(store
            .proc_totals(from, to)?
            .into_iter()
            .map(|p| ProcTotal {
                joules: p.joules / eff,
                gpu_joules: p.gpu_joules.map(|g| g / eff),
                ..p
            })
            .collect())
    }

    /// Watts at the wall of one process per bucket, as (unix seconds, watts).
    pub fn proc_series(
        &self,
        name: &str,
        from: i64,
        to: i64,
        bucket_secs: i64,
        cfg: &Config,
    ) -> Result<Vec<(i64, f64)>> {
        let Some(store) = &self.store else {
            return Ok(Vec::new());
        };
        let eff = cfg.estimate.psu_efficiency_percent / 100.0;
        let b = bucket_secs.max(60) as f64;
        Ok(store
            .proc_series(name, from, to, bucket_secs)?
            .into_iter()
            .map(|(t, j)| (t, j / eff / b))
            .collect())
    }

    pub fn token_totals(&self, from: i64, to: i64) -> Result<Vec<TokenTotal>> {
        match &self.store {
            Some(store) => store.token_totals(from, to),
            None => Ok(Vec::new()),
        }
    }

    pub fn first_minute(&self) -> Option<i64> {
        self.store
            .as_ref()
            .and_then(|s| s.first_minute().ok().flatten())
    }

    /// The newest live sample's watts per source id.
    pub fn now_watts(&self) -> BTreeMap<i64, f64> {
        let mut out = BTreeMap::new();
        if let Some(live) = &self.live
            && let Some(s) = live.samples.last()
        {
            for (src, j) in live.sources.iter().zip(&s.joules) {
                if let Some(j) = j {
                    out.insert(src.id, *j);
                }
            }
        }
        out
    }
}

/// Local calendar boundaries used for the period table.
pub struct Calendar {
    pub hour: i64,
    pub day: i64,
    pub week: i64,
    pub month: i64,
}

impl Calendar {
    pub fn at(now: i64) -> Self {
        let t = local(now);
        let date = t.date_naive();
        let day = midnight(date);
        let week = midnight(date - Duration::days(date.weekday().num_days_from_monday() as i64));
        let month = midnight(date.with_day(1).unwrap_or(date));
        Self {
            hour: now - local_secs_into_hour(&t),
            day,
            week,
            month,
        }
    }
}

fn local_secs_into_hour(t: &DateTime<Local>) -> i64 {
    use chrono::Timelike;
    (t.minute() * 60 + t.second()) as i64
}

pub fn local(ts: i64) -> DateTime<Local> {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).unwrap())
}

/// Unix seconds of local midnight at the start of `date` (handles DST gaps).
pub fn midnight(date: NaiveDate) -> i64 {
    let naive = date.and_hms_opt(0, 0, 0).unwrap_or_default();
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => t.timestamp(),
        chrono::LocalResult::Ambiguous(a, _) => a.timestamp(),
        chrono::LocalResult::None => Local.from_utc_datetime(&naive).timestamp(),
    }
}

/// Splits `[from, to)` into local calendar units: "hour", "day" or "month".
pub fn calendar_slots(from: i64, to: i64, unit: SlotUnit) -> Vec<(i64, i64, String)> {
    let mut out = Vec::new();
    let mut start = match unit {
        SlotUnit::Hour => from - local_secs_into_hour(&local(from)),
        SlotUnit::Day => midnight(local(from).date_naive()),
        SlotUnit::Month => {
            let d = local(from).date_naive();
            midnight(d.with_day(1).unwrap_or(d))
        }
    };
    while start < to && out.len() < 1000 {
        let t = local(start);
        let (end, label) = match unit {
            SlotUnit::Hour => (start + 3600, t.format("%H").to_string()),
            SlotUnit::Day => (
                midnight(t.date_naive() + Duration::days(1)),
                t.format("%d").to_string(),
            ),
            SlotUnit::Month => {
                let d = t.date_naive();
                let next = if d.month() == 12 {
                    NaiveDate::from_ymd_opt(d.year() + 1, 1, 1)
                } else {
                    NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1)
                };
                (midnight(next.unwrap_or(d)), t.format("%b").to_string())
            }
        };
        out.push((start, end, label));
        start = end;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotUnit {
    Hour,
    Day,
    Month,
}

/// Formats watts compactly: "412 W", "1.2 kW".
pub fn fmt_watts(w: f64) -> String {
    if w >= 1000.0 {
        format!("{:.2} kW", w / 1000.0)
    } else {
        format!("{:.0} W", w)
    }
}

pub fn fmt_kwh(kwh: f64) -> String {
    if kwh >= 100.0 {
        format!("{kwh:.0} kWh")
    } else if kwh >= 1.0 {
        format!("{kwh:.2} kWh")
    } else {
        format!("{:.0} Wh", kwh * 1000.0)
    }
}

/// Money with two decimals (three below 1), the symbol before or after as is customary.
pub fn fmt_money(v: f64, currency: &str) -> String {
    let n = if v.abs() < 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.2}")
    };
    if ["$", "£", "¥", "A$", "C$"].contains(&currency) {
        format!("{currency}{n}")
    } else {
        format!("{n} {currency}")
    }
}

/// Large counts: 950, 12.3k, 148M, 2.1B.
pub fn fmt_count(n: f64) -> String {
    match n {
        n if n >= 1e9 => format!("{:.1}B", n / 1e9),
        n if n >= 1e6 => format!("{:.1}M", n / 1e6),
        n if n >= 1e4 => format!("{:.0}k", n / 1e3),
        n if n >= 1e3 => format!("{:.1}k", n / 1e3),
        n => format!("{n:.0}"),
    }
}

pub fn fmt_co2(g: f64) -> String {
    if g >= 1000.0 {
        format!("{:.2} kg", g / 1000.0)
    } else {
        format!("{g:.0} g")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PriceMode;

    fn bucket(ts: i64, secs: f64, j: f64) -> Bucket {
        let mut b = Bucket::new(ts);
        b.secs = secs;
        b.joules.insert(1, j);
        b
    }

    #[test]
    fn wall_adds_base_load_and_psu_loss() {
        let mut cfg = Config::default();
        cfg.estimate.base_watts = 40.0;
        cfg.estimate.psu_efficiency_percent = 80.0;
        let w = Wall::of(360.0, 1.0, &cfg);
        assert_eq!(w.base, 40.0);
        assert!((w.psu_loss - 100.0).abs() < 1e-9); // 400 / 0.8 - 400
        assert!((w.total() - 500.0).abs() < 1e-9);
    }

    #[test]
    fn totals_price_and_co2() {
        let mut cfg = Config::default();
        cfg.estimate.base_watts = 0.0;
        cfg.estimate.psu_efficiency_percent = 100.0;
        cfg.price.mode = PriceMode::Fixed;
        cfg.price.fixed_ct_per_kwh = 40.0;
        cfg.co2.g_per_kwh = 300.0;
        let pricer = Pricer::new(&cfg, None);
        // 1 kWh over one hour: 1000 W.
        let buckets = vec![
            bucket(0, 1800.0, J_PER_KWH / 2.0),
            bucket(1800, 1800.0, J_PER_KWH / 2.0),
        ];
        let t = sum(&buckets, 0, 3600, &cfg, &pricer);
        assert!((t.kwh() - 1.0).abs() < 1e-9);
        assert!((t.cost - 0.40).abs() < 1e-9);
        assert!((t.co2_g - 300.0).abs() < 1e-9);
        assert!((t.avg_watts() - 1000.0).abs() < 1e-9);
        assert_eq!(
            t.avg_ct_per_kwh().map(|c| (c * 1e6).round() / 1e6),
            Some(40.0)
        );
        assert_eq!(sum(&buckets, 0, 1800, &cfg, &pricer).secs, 1800.0);
    }

    #[test]
    fn calendar_slots_cover_range() {
        let now = Local
            .with_ymd_and_hms(2026, 11, 18, 14, 20, 0)
            .single()
            .unwrap()
            .timestamp();
        let hours = calendar_slots(now - 3 * 3600, now, SlotUnit::Hour);
        assert_eq!(hours.len(), 4);
        assert_eq!(hours[0].2, "11");
        let days = calendar_slots(now - 6 * 86400, now, SlotUnit::Day);
        assert_eq!(days.len(), 7);
        assert_eq!(days.last().unwrap().2, "18");
        let months = calendar_slots(now - 40 * 86400, now, SlotUnit::Month);
        assert_eq!(
            months.iter().map(|m| m.2.as_str()).collect::<Vec<_>>(),
            vec!["Oct", "Nov"]
        );
        let cal = Calendar::at(now);
        assert_eq!(now - cal.hour, 20 * 60);
        assert_eq!(
            cal.day,
            midnight(NaiveDate::from_ymd_opt(2026, 11, 18).unwrap())
        );
        assert_eq!(
            cal.week,
            midnight(NaiveDate::from_ymd_opt(2026, 11, 16).unwrap())
        );
        assert_eq!(
            cal.month,
            midnight(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap())
        );
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_watts(412.4), "412 W");
        assert_eq!(fmt_watts(1284.0), "1.28 kW");
        assert_eq!(fmt_money(4.5, "£"), "£4.50");
        assert_eq!(fmt_count(148_000_000.0), "148.0M");
        assert_eq!(fmt_kwh(0.25), "250 Wh");
        assert_eq!(fmt_kwh(3.456), "3.46 kWh");
        assert_eq!(fmt_co2(1500.0), "1.50 kg");
    }
}
