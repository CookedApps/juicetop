//! User settings: electricity price, CO2 intensity, estimates for parts we cannot measure.
//!
//! The daemon only measures and stores joules. Everything that turns joules into money,
//! CO2 or "power at the wall" is applied when reading, so changing a setting re-prices
//! the whole history.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Symbol printed after amounts, e.g. "€" or "$".
    pub currency: String,
    pub price: PriceConfig,
    pub co2: Co2Config,
    pub estimate: EstimateConfig,
    pub data: DataConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceMode {
    /// One price per kWh, all day.
    Fixed,
    /// Two prices: a high tariff and a low tariff for a time window (HT/NT).
    TimeOfUse,
    /// Hourly exchange price plus fixed markup and VAT (aWATTar day-ahead data).
    Dynamic,
}

impl PriceMode {
    pub const ALL: [PriceMode; 3] = [PriceMode::Fixed, PriceMode::TimeOfUse, PriceMode::Dynamic];

    pub fn label(self) -> &'static str {
        match self {
            PriceMode::Fixed => "fixed",
            PriceMode::TimeOfUse => "time of use",
            PriceMode::Dynamic => "dynamic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PriceConfig {
    pub mode: PriceMode,
    /// Price per kWh in cents (or the smallest unit of your currency).
    pub fixed_ct_per_kwh: f64,
    pub time_of_use: TimeOfUseConfig,
    pub dynamic: DynamicConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeOfUseConfig {
    pub high_ct_per_kwh: f64,
    pub low_ct_per_kwh: f64,
    /// Local time "HH:MM" when the low tariff starts.
    pub low_start: String,
    /// Local time "HH:MM" when the low tariff ends. May be earlier than the start (overnight).
    pub low_end: String,
    /// Saturdays and Sundays use the low tariff all day.
    pub low_on_weekends: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DynamicConfig {
    /// Day-ahead market region: "de" or "at".
    pub region: String,
    /// Grid fees, levies, taxes and supplier margin per kWh, before VAT, in cents.
    pub markup_ct_per_kwh: f64,
    pub vat_percent: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Co2Config {
    /// Grams of CO2 per kWh. Take it from your supplier's fuel mix label.
    pub g_per_kwh: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EstimateConfig {
    /// Constant draw of everything without a sensor: mainboard, RAM, drives, fans.
    pub base_watts: f64,
    /// Power supply efficiency at typical load, in percent. 90 is a fair guess for 80 PLUS Gold.
    pub psu_efficiency_percent: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataConfig {
    /// Override the database location. Auto-detected when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_path: Option<PathBuf>,
    /// Override the live snapshot location. Auto-detected when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            currency: "€".into(),
            price: PriceConfig::default(),
            co2: Co2Config::default(),
            estimate: EstimateConfig::default(),
            data: DataConfig::default(),
        }
    }
}

impl Default for PriceConfig {
    fn default() -> Self {
        Self {
            mode: PriceMode::Fixed,
            fixed_ct_per_kwh: 35.0,
            time_of_use: TimeOfUseConfig::default(),
            dynamic: DynamicConfig::default(),
        }
    }
}

impl Default for TimeOfUseConfig {
    fn default() -> Self {
        Self {
            high_ct_per_kwh: 38.0,
            low_ct_per_kwh: 29.0,
            low_start: "22:00".into(),
            low_end: "06:00".into(),
            low_on_weekends: false,
        }
    }
}

impl Default for DynamicConfig {
    fn default() -> Self {
        Self {
            region: "de".into(),
            markup_ct_per_kwh: 18.0,
            vat_percent: 19.0,
        }
    }
}

impl Default for Co2Config {
    fn default() -> Self {
        // Roughly the German grid average in recent years. Replace with your tariff's label value.
        Self { g_per_kwh: 360.0 }
    }
}

impl Default for EstimateConfig {
    fn default() -> Self {
        Self {
            base_watts: 40.0,
            psu_efficiency_percent: 90.0,
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("juicetop").join("config.toml"))
    }

    /// Loads the config, falling back to defaults when the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Config =
                    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                Ok(cfg.sanitized())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Clamps values that would otherwise produce nonsense (division by zero, negative loads).
    pub fn sanitized(mut self) -> Self {
        let e = &mut self.estimate;
        if !(e.psu_efficiency_percent > 1.0 && e.psu_efficiency_percent <= 100.0) {
            e.psu_efficiency_percent = 90.0;
        }
        if e.base_watts.is_nan() || e.base_watts < 0.0 {
            e.base_watts = 0.0;
        }
        if self.co2.g_per_kwh.is_nan() || self.co2.g_per_kwh < 0.0 {
            self.co2.g_per_kwh = 0.0;
        }
        let r = self.price.dynamic.region.to_ascii_lowercase();
        self.price.dynamic.region = if r == "at" { "at".into() } else { "de".into() };
        self
    }
}

/// Parses "HH:MM" into minutes after midnight.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let cfg: Config = toml::from_str("[price]\nfixed_ct_per_kwh = 31.5\n").unwrap();
        assert_eq!(cfg.price.fixed_ct_per_kwh, 31.5);
        assert_eq!(cfg.price.mode, PriceMode::Fixed);
        assert_eq!(cfg.co2.g_per_kwh, Co2Config::default().g_per_kwh);
    }

    #[test]
    fn sanitize_fixes_bad_efficiency() {
        let mut cfg = Config::default();
        cfg.estimate.psu_efficiency_percent = 0.0;
        assert_eq!(cfg.sanitized().estimate.psu_efficiency_percent, 90.0);
    }

    #[test]
    fn parses_times() {
        assert_eq!(parse_hhmm("22:00"), Some(1320));
        assert_eq!(parse_hhmm(" 6:30"), Some(390));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("nope"), None);
    }
}
