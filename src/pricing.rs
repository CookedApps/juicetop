//! Electricity price per kWh at a point in time, for the three tariff models.

use chrono::{Datelike, Local, TimeZone, Timelike, Weekday};
use serde::{Deserialize, Serialize};

use crate::config::{Config, PriceMode, parse_hhmm};

/// One day-ahead market price interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Interval {
    /// Unix seconds, inclusive.
    pub start: i64,
    /// Unix seconds, exclusive.
    pub end: i64,
    pub eur_per_mwh: f64,
}

/// Market prices sorted by start, without overlaps.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PriceTable {
    pub intervals: Vec<Interval>,
}

impl PriceTable {
    pub fn lookup(&self, ts: i64) -> Option<f64> {
        let i = self.intervals.partition_point(|iv| iv.start <= ts);
        let iv = self.intervals.get(i.checked_sub(1)?)?;
        (ts < iv.end).then_some(iv.eur_per_mwh)
    }

    /// Adds intervals, newer data replacing older data with the same start.
    pub fn merge(&mut self, new: impl IntoIterator<Item = Interval>) {
        let mut map: std::collections::BTreeMap<i64, Interval> =
            self.intervals.drain(..).map(|iv| (iv.start, iv)).collect();
        for iv in new {
            if iv.end > iv.start {
                map.insert(iv.start, iv);
            }
        }
        self.intervals = map.into_values().collect();
    }

    pub fn first_start(&self) -> Option<i64> {
        self.intervals.first().map(|iv| iv.start)
    }

    pub fn last_end(&self) -> Option<i64> {
        self.intervals.last().map(|iv| iv.end)
    }
}

/// A price in cents per kWh and whether it is the real tariff price (false: a fallback).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub ct_per_kwh: f64,
    pub exact: bool,
}

pub struct Pricer<'a> {
    cfg: &'a Config,
    market: Option<&'a PriceTable>,
    low_window: Option<(u32, u32)>,
}

impl<'a> Pricer<'a> {
    pub fn new(cfg: &'a Config, market: Option<&'a PriceTable>) -> Self {
        let t = &cfg.price.time_of_use;
        let low_window = parse_hhmm(&t.low_start).zip(parse_hhmm(&t.low_end));
        Self {
            cfg,
            market,
            low_window,
        }
    }

    pub fn at(&self, ts: i64) -> Price {
        let p = &self.cfg.price;
        match p.mode {
            PriceMode::Fixed => Price {
                ct_per_kwh: p.fixed_ct_per_kwh,
                exact: true,
            },
            PriceMode::TimeOfUse => {
                let t = &p.time_of_use;
                let ct = if self.is_low(ts) {
                    t.low_ct_per_kwh
                } else {
                    t.high_ct_per_kwh
                };
                Price {
                    ct_per_kwh: ct,
                    exact: self.low_window.is_some(),
                }
            }
            PriceMode::Dynamic => match self.market.and_then(|m| m.lookup(ts)) {
                Some(eur_per_mwh) => {
                    let d = &p.dynamic;
                    // EUR/MWh equals 0.1 ct/kWh.
                    let ct =
                        (eur_per_mwh / 10.0 + d.markup_ct_per_kwh) * (1.0 + d.vat_percent / 100.0);
                    Price {
                        ct_per_kwh: ct,
                        exact: true,
                    }
                }
                None => Price {
                    ct_per_kwh: p.fixed_ct_per_kwh,
                    exact: false,
                },
            },
        }
    }

    /// Whether the low tariff applies at `ts` (local time).
    pub fn is_low(&self, ts: i64) -> bool {
        let Some((start, end)) = self.low_window else {
            return false;
        };
        let Some(t) = Local.timestamp_opt(ts, 0).single() else {
            return false;
        };
        if self.cfg.price.time_of_use.low_on_weekends
            && matches!(t.weekday(), Weekday::Sat | Weekday::Sun)
        {
            return true;
        }
        let m = t.hour() * 60 + t.minute();
        in_window(m, start, end)
    }

    /// Short description of the active tariff for headers.
    pub fn describe(&self, now: i64) -> String {
        let p = self.at(now);
        let cfg = &self.cfg.price;
        let what = match cfg.mode {
            PriceMode::Fixed => "fixed".to_string(),
            PriceMode::TimeOfUse => if self.is_low(now) {
                "low tariff"
            } else {
                "high tariff"
            }
            .to_string(),
            PriceMode::Dynamic if p.exact => {
                format!("dynamic {}", cfg.dynamic.region.to_uppercase())
            }
            PriceMode::Dynamic => "dynamic, no market data yet".to_string(),
        };
        format!("{:.1} ct/kWh {what}", p.ct_per_kwh)
    }
}

/// Minute-of-day window check that handles windows crossing midnight.
fn in_window(m: u32, start: u32, end: u32) -> bool {
    match start.cmp(&end) {
        std::cmp::Ordering::Less => m >= start && m < end,
        std::cmp::Ordering::Greater => m >= start || m < end,
        std::cmp::Ordering::Equal => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cross_midnight() {
        let (s, e) = (22 * 60, 6 * 60);
        assert!(in_window(23 * 60, s, e));
        assert!(in_window(0, s, e));
        assert!(in_window(5 * 60 + 59, s, e));
        assert!(!in_window(6 * 60, s, e));
        assert!(!in_window(12 * 60, s, e));
        assert!(in_window(12 * 60, 9 * 60, 17 * 60));
        assert!(!in_window(100, 100, 100));
    }

    #[test]
    fn fixed_price_is_constant() {
        let cfg = Config::default();
        let p = Pricer::new(&cfg, None).at(0);
        assert_eq!(
            p,
            Price {
                ct_per_kwh: 35.0,
                exact: true
            }
        );
    }

    #[test]
    fn time_of_use_follows_local_clock() {
        let mut cfg = Config::default();
        cfg.price.mode = PriceMode::TimeOfUse;
        let pricer = Pricer::new(&cfg, None);
        let local = |h| {
            Local
                .with_ymd_and_hms(2026, 11, 18, h, 30, 0)
                .single()
                .unwrap()
                .timestamp()
        };
        assert_eq!(pricer.at(local(23)).ct_per_kwh, 29.0);
        assert_eq!(pricer.at(local(3)).ct_per_kwh, 29.0);
        assert_eq!(pricer.at(local(12)).ct_per_kwh, 38.0);
    }

    #[test]
    fn weekends_can_be_low() {
        let mut cfg = Config::default();
        cfg.price.mode = PriceMode::TimeOfUse;
        cfg.price.time_of_use.low_on_weekends = true;
        let pricer = Pricer::new(&cfg, None);
        // 2026-11-21 is a Saturday.
        let sat_noon = Local
            .with_ymd_and_hms(2026, 11, 21, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(pricer.at(sat_noon).ct_per_kwh, 29.0);
    }

    #[test]
    fn dynamic_adds_markup_and_vat_and_falls_back() {
        let mut cfg = Config::default();
        cfg.price.mode = PriceMode::Dynamic;
        cfg.price.dynamic.markup_ct_per_kwh = 20.0;
        cfg.price.dynamic.vat_percent = 10.0;
        let mut table = PriceTable::default();
        table.merge([Interval {
            start: 3600,
            end: 7200,
            eur_per_mwh: 100.0,
        }]);
        let pricer = Pricer::new(&cfg, Some(&table));
        let p = pricer.at(4000);
        assert!((p.ct_per_kwh - 33.0).abs() < 1e-9); // (10 + 20) * 1.1
        assert!(p.exact);
        assert_eq!(
            pricer.at(7200),
            Price {
                ct_per_kwh: 35.0,
                exact: false
            }
        );
    }

    #[test]
    fn table_merge_and_lookup() {
        let mut t = PriceTable::default();
        t.merge([
            Interval {
                start: 0,
                end: 10,
                eur_per_mwh: 1.0,
            },
            Interval {
                start: 10,
                end: 20,
                eur_per_mwh: 2.0,
            },
        ]);
        t.merge([
            Interval {
                start: 10,
                end: 20,
                eur_per_mwh: 3.0,
            },
            Interval {
                start: 20,
                end: 30,
                eur_per_mwh: 4.0,
            },
        ]);
        assert_eq!(t.lookup(-1), None);
        assert_eq!(t.lookup(0), Some(1.0));
        assert_eq!(t.lookup(15), Some(3.0));
        assert_eq!(t.lookup(29), Some(4.0));
        assert_eq!(t.lookup(30), None);
        assert_eq!((t.first_start(), t.last_end()), (Some(0), Some(30)));
    }
}
