//! The settings form: every field reads from and writes to `Config` directly.

use crate::config::{Config, PriceMode, parse_hhmm};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Mode,
    Currency,
    Fixed,
    TouHigh,
    TouLow,
    TouStart,
    TouEnd,
    TouWeekends,
    DynRegion,
    DynMarkup,
    DynVat,
    Co2,
    BaseWatts,
    PsuEfficiency,
}

impl Field {
    pub const ALL: [Field; 14] = [
        Field::Mode,
        Field::Currency,
        Field::Fixed,
        Field::TouHigh,
        Field::TouLow,
        Field::TouStart,
        Field::TouEnd,
        Field::TouWeekends,
        Field::DynRegion,
        Field::DynMarkup,
        Field::DynVat,
        Field::Co2,
        Field::BaseWatts,
        Field::PsuEfficiency,
    ];

    pub fn section(self) -> &'static str {
        match self {
            Field::Mode | Field::Currency => "Price",
            Field::Fixed => "Fixed price",
            Field::TouHigh
            | Field::TouLow
            | Field::TouStart
            | Field::TouEnd
            | Field::TouWeekends => "Time of use (HT/NT)",
            Field::DynRegion | Field::DynMarkup | Field::DynVat => "Dynamic (aWATTar day-ahead)",
            Field::Co2 => "CO2",
            Field::BaseWatts | Field::PsuEfficiency => "Estimates",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Field::Mode => "Tariff model",
            Field::Currency => "Currency symbol",
            Field::Fixed => "Price per kWh (cents)",
            Field::TouHigh => "High tariff (cents/kWh)",
            Field::TouLow => "Low tariff (cents/kWh)",
            Field::TouStart => "Low tariff starts",
            Field::TouEnd => "Low tariff ends",
            Field::TouWeekends => "Low tariff on weekends",
            Field::DynRegion => "Market region",
            Field::DynMarkup => "Markup before VAT (cents/kWh)",
            Field::DynVat => "VAT (%)",
            Field::Co2 => "Grid intensity (g CO2/kWh)",
            Field::BaseWatts => "Base load w/o sensor (W)",
            Field::PsuEfficiency => "PSU efficiency (%)",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Field::Mode => "fixed: one price. time of use: HT/NT. dynamic: hourly exchange price.",
            Field::Currency => "Shown after amounts.",
            Field::Fixed => {
                "Your energy price per kWh incl. VAT, in cents. Also the fallback price."
            }
            Field::TouHigh | Field::TouLow => "Price per kWh incl. VAT, in cents.",
            Field::TouStart | Field::TouEnd => "Local time HH:MM. The window may cross midnight.",
            Field::TouWeekends => "Saturday and Sunday use the low tariff all day.",
            Field::DynRegion => {
                "de or at. Prices come from api.awattar.de / .at, free, cached locally."
            }
            Field::DynMarkup => "Grid fees, levies, taxes and supplier margin per kWh, before VAT.",
            Field::DynVat => "Applied to exchange price plus markup.",
            Field::Co2 => {
                "From your supplier's fuel mix disclosure on the bill or website. 0 for none."
            }
            Field::BaseWatts => {
                "Mainboard, RAM, drives, fans. A wall plug meter tells you the truth."
            }
            Field::PsuEfficiency => "About 90 for 80 PLUS Gold at typical load.",
        }
    }

    /// Whether the field matters for the selected tariff model.
    pub fn active(self, cfg: &Config) -> bool {
        match self {
            Field::TouHigh
            | Field::TouLow
            | Field::TouStart
            | Field::TouEnd
            | Field::TouWeekends => cfg.price.mode == PriceMode::TimeOfUse,
            Field::DynRegion | Field::DynMarkup | Field::DynVat => {
                cfg.price.mode == PriceMode::Dynamic
            }
            _ => true,
        }
    }

    /// Fields changed with left/right or Enter instead of typing.
    pub fn is_choice(self) -> bool {
        matches!(self, Field::Mode | Field::TouWeekends | Field::DynRegion)
    }

    pub fn get(self, cfg: &Config) -> String {
        let p = &cfg.price;
        match self {
            Field::Mode => p.mode.label().into(),
            Field::Currency => cfg.currency.clone(),
            Field::Fixed => num(p.fixed_ct_per_kwh),
            Field::TouHigh => num(p.time_of_use.high_ct_per_kwh),
            Field::TouLow => num(p.time_of_use.low_ct_per_kwh),
            Field::TouStart => p.time_of_use.low_start.clone(),
            Field::TouEnd => p.time_of_use.low_end.clone(),
            Field::TouWeekends => if p.time_of_use.low_on_weekends {
                "yes"
            } else {
                "no"
            }
            .into(),
            Field::DynRegion => p.dynamic.region.to_uppercase(),
            Field::DynMarkup => num(p.dynamic.markup_ct_per_kwh),
            Field::DynVat => num(p.dynamic.vat_percent),
            Field::Co2 => num(cfg.co2.g_per_kwh),
            Field::BaseWatts => num(cfg.estimate.base_watts),
            Field::PsuEfficiency => num(cfg.estimate.psu_efficiency_percent),
        }
    }

    /// Steps a choice field forward or backward.
    pub fn cycle(self, cfg: &mut Config, forward: bool) {
        match self {
            Field::Mode => {
                let all = PriceMode::ALL;
                let i = all.iter().position(|m| *m == cfg.price.mode).unwrap_or(0);
                let n = all.len();
                cfg.price.mode = all[if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                }];
            }
            Field::TouWeekends => {
                cfg.price.time_of_use.low_on_weekends = !cfg.price.time_of_use.low_on_weekends;
            }
            Field::DynRegion => {
                let r = &mut cfg.price.dynamic.region;
                *r = if r == "de" { "at".into() } else { "de".into() };
            }
            _ => {}
        }
    }

    /// Parses typed input into the config.
    pub fn set(self, cfg: &mut Config, input: &str) -> Result<(), String> {
        let input = input.trim();
        let number = |min: f64, max: f64| -> Result<f64, String> {
            let v: f64 = input
                .replace(',', ".")
                .parse()
                .map_err(|_| format!("'{input}' is not a number"))?;
            if v < min || v > max {
                return Err(format!("must be between {min} and {max}"));
            }
            Ok(v)
        };
        let time = || -> Result<String, String> {
            parse_hhmm(input)
                .map(|m| format!("{:02}:{:02}", m / 60, m % 60))
                .ok_or_else(|| "use HH:MM".to_string())
        };
        let p = &mut cfg.price;
        match self {
            Field::Currency => {
                if input.is_empty() || input.chars().count() > 4 {
                    return Err("1 to 4 characters".into());
                }
                cfg.currency = input.to_string();
            }
            Field::Fixed => p.fixed_ct_per_kwh = number(0.0, 1000.0)?,
            Field::TouHigh => p.time_of_use.high_ct_per_kwh = number(0.0, 1000.0)?,
            Field::TouLow => p.time_of_use.low_ct_per_kwh = number(0.0, 1000.0)?,
            Field::TouStart => p.time_of_use.low_start = time()?,
            Field::TouEnd => p.time_of_use.low_end = time()?,
            Field::DynMarkup => p.dynamic.markup_ct_per_kwh = number(-100.0, 1000.0)?,
            Field::DynVat => p.dynamic.vat_percent = number(0.0, 100.0)?,
            Field::Co2 => cfg.co2.g_per_kwh = number(0.0, 2000.0)?,
            Field::BaseWatts => cfg.estimate.base_watts = number(0.0, 2000.0)?,
            Field::PsuEfficiency => cfg.estimate.psu_efficiency_percent = number(50.0, 100.0)?,
            Field::Mode | Field::TouWeekends | Field::DynRegion => {}
        }
        Ok(())
    }
}

fn num(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// State of the open settings popup.
pub struct SettingsForm {
    pub selected: usize,
    /// Text being typed, when editing a text field.
    pub editing: Option<String>,
    pub error: Option<String>,
    /// The config as it was when the form opened, to tell whether to save.
    pub original: Config,
}

impl SettingsForm {
    pub fn new(cfg: &Config) -> Self {
        Self {
            selected: 0,
            editing: None,
            error: None,
            original: cfg.clone(),
        }
    }

    pub fn field(&self) -> Field {
        Field::ALL[self.selected]
    }

    pub fn move_by(&mut self, delta: isize) {
        let n = Field::ALL.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_validates_and_normalizes() {
        let mut cfg = Config::default();
        Field::Fixed.set(&mut cfg, "31,9").unwrap();
        assert_eq!(cfg.price.fixed_ct_per_kwh, 31.9);
        assert!(Field::Fixed.set(&mut cfg, "abc").is_err());
        Field::TouStart.set(&mut cfg, "7:5").unwrap();
        assert_eq!(cfg.price.time_of_use.low_start, "07:05");
        assert!(Field::PsuEfficiency.set(&mut cfg, "20").is_err());
        assert_eq!(Field::Fixed.get(&cfg), "31.9");
    }

    #[test]
    fn cycle_mode_wraps() {
        let mut cfg = Config::default();
        Field::Mode.cycle(&mut cfg, false);
        assert_eq!(cfg.price.mode, PriceMode::Dynamic);
        Field::Mode.cycle(&mut cfg, true);
        assert_eq!(cfg.price.mode, PriceMode::Fixed);
        assert!(!Field::DynVat.active(&cfg));
    }
}
