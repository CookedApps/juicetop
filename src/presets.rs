//! Country defaults for the setup: currency, a typical household price, VAT and the carbon
//! intensity of the grid. They are starting points the user can change, not quotes.
//!
//! Sources: Eurostat nrg_pc_204 (household band DC, all taxes, H1 2026), ElCom (CH 2026),
//! Ofgem price cap (GB, Oct to Dec 2026), EIA (US 2025), Ember via Our World in Data (grid
//! intensity 2025). Canada and Australia have no official national average; their prices are
//! rough approximations.

use std::path::Path;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Country {
    pub code: &'static str,
    pub name: &'static str,
    pub currency: &'static str,
    /// Name of the minor unit prices are entered in.
    pub minor: &'static str,
    /// Typical household price per kWh incl. taxes, in the minor unit.
    pub price: f64,
    pub vat: f64,
    pub co2_g: f64,
    /// aWATTar market region for dynamic tariffs, if any.
    pub market: Option<&'static str>,
}

#[allow(clippy::too_many_arguments)]
const fn c(
    code: &'static str,
    name: &'static str,
    currency: &'static str,
    minor: &'static str,
    price: f64,
    vat: f64,
    co2_g: f64,
    market: Option<&'static str>,
) -> Country {
    Country {
        code,
        name,
        currency,
        minor,
        price,
        vat,
        co2_g,
        market,
    }
}

pub const COUNTRIES: &[Country] = &[
    c("DE", "Germany", "€", "ct", 36.0, 19.0, 330.0, Some("de")),
    c("AT", "Austria", "€", "ct", 33.0, 20.0, 115.0, Some("at")),
    c("CH", "Switzerland", "CHF", "Rp", 28.0, 8.1, 40.0, None),
    c("NL", "Netherlands", "€", "ct", 24.0, 21.0, 255.0, None),
    c("BE", "Belgium", "€", "ct", 36.0, 6.0, 150.0, None),
    c("FR", "France", "€", "ct", 25.0, 20.0, 40.0, None),
    c("IT", "Italy", "€", "ct", 31.0, 10.0, 285.0, None),
    c("ES", "Spain", "€", "ct", 25.0, 21.0, 155.0, None),
    c("PL", "Poland", "zł", "gr", 127.0, 23.0, 590.0, None),
    c("DK", "Denmark", "kr", "øre", 177.0, 25.0, 115.0, None),
    c("SE", "Sweden", "kr", "öre", 330.0, 25.0, 35.0, None),
    c("NO", "Norway", "kr", "øre", 223.0, 25.0, 30.0, None),
    c("FI", "Finland", "€", "c", 27.0, 25.5, 55.0, None),
    c("GB", "United Kingdom", "£", "p", 26.0, 5.0, 215.0, None),
    c("IE", "Ireland", "€", "c", 39.0, 9.0, 255.0, None),
    c("US", "United States", "$", "¢", 17.0, 0.0, 385.0, None),
    c("CA", "Canada", "C$", "¢", 17.0, 5.0, 190.0, None),
    c("AU", "Australia", "A$", "c", 35.0, 10.0, 525.0, None),
    c("XX", "Other", "€", "ct", 30.0, 20.0, 400.0, None),
];

pub fn by_code(code: &str) -> Option<&'static Country> {
    COUNTRIES.iter().find(|c| c.code.eq_ignore_ascii_case(code))
}

pub fn other() -> &'static Country {
    COUNTRIES.last().expect("list is not empty")
}

/// The unit prices are shown in, e.g. "ct" or "p".
pub fn minor_unit(cfg: &Config) -> &'static str {
    by_code(&cfg.country).map_or("ct", |c| c.minor)
}

/// Fills in a country's defaults.
pub fn apply(cfg: &mut Config, c: &Country) {
    cfg.country = c.code.into();
    cfg.currency = c.currency.into();
    let p = &mut cfg.price;
    p.fixed_ct_per_kwh = c.price;
    p.time_of_use.high_ct_per_kwh = (c.price * 1.08).round();
    p.time_of_use.low_ct_per_kwh = (c.price * 0.8).round();
    if let Some(region) = c.market {
        p.dynamic.region = region.into();
        p.dynamic.vat_percent = c.vat;
        // Average exchange prices are around 10 ct; the rest of the price is markup.
        p.dynamic.markup_ct_per_kwh = (c.price / (1.0 + c.vat / 100.0) - 10.0).max(0.0).round();
    }
    cfg.co2.g_per_kwh = c.co2_g;
}

/// Guesses the country from the time zone, then from the locale.
pub fn detect() -> &'static Country {
    let tz = std::env::var("TZ")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            std::fs::read_link("/etc/localtime")
                .ok()
                .and_then(|p| {
                    p.to_str()
                        .and_then(|s| s.split("zoneinfo/").nth(1))
                        .map(str::to_string)
                })
                .or_else(|| {
                    std::fs::read_to_string(Path::new("/etc/timezone"))
                        .ok()
                        .map(|s| s.trim().to_string())
                })
        });
    if let Some(c) = tz.as_deref().and_then(from_timezone) {
        return c;
    }
    let lang = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    from_locale(&lang).unwrap_or_else(other)
}

pub fn from_timezone(tz: &str) -> Option<&'static Country> {
    let code = match tz.trim_start_matches(':') {
        "Europe/Berlin" | "Europe/Busingen" => "DE",
        "Europe/Vienna" => "AT",
        "Europe/Zurich" => "CH",
        "Europe/Amsterdam" => "NL",
        "Europe/Brussels" => "BE",
        "Europe/Paris" => "FR",
        "Europe/Rome" => "IT",
        "Europe/Madrid" | "Atlantic/Canary" => "ES",
        "Europe/Warsaw" => "PL",
        "Europe/Copenhagen" => "DK",
        "Europe/Stockholm" => "SE",
        "Europe/Oslo" => "NO",
        "Europe/Helsinki" => "FI",
        "Europe/London" => "GB",
        "Europe/Dublin" => "IE",
        t if t.starts_with("Australia/") => "AU",
        "America/Toronto" | "America/Vancouver" | "America/Edmonton" | "America/Winnipeg"
        | "America/Halifax" | "America/Regina" | "America/St_Johns" | "America/Montreal" => "CA",
        "America/New_York"
        | "America/Chicago"
        | "America/Denver"
        | "America/Los_Angeles"
        | "America/Phoenix"
        | "America/Anchorage"
        | "America/Detroit"
        | "Pacific/Honolulu" => "US",
        t if t.starts_with("US/") => "US",
        _ => return None,
    };
    by_code(code)
}

/// "de_AT.UTF-8" becomes Austria.
pub fn from_locale(lang: &str) -> Option<&'static Country> {
    let region = lang.split(['.', '@']).next()?.split('_').nth(1)?;
    by_code(region).filter(|c| c.code != "XX")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_from_timezone_and_locale() {
        assert_eq!(from_timezone("Europe/Berlin").unwrap().code, "DE");
        assert_eq!(from_timezone("America/Toronto").unwrap().code, "CA");
        assert_eq!(from_timezone("Australia/Sydney").unwrap().code, "AU");
        assert!(from_timezone("Asia/Tokyo").is_none());
        assert_eq!(from_locale("de_AT.UTF-8").unwrap().code, "AT");
        assert_eq!(from_locale("en_GB").unwrap().code, "GB");
        assert!(from_locale("C").is_none());
    }

    #[test]
    fn apply_sets_prices_and_markup() {
        let mut cfg = Config::default();
        apply(&mut cfg, by_code("DE").unwrap());
        assert_eq!(cfg.currency, "€");
        assert_eq!(cfg.price.fixed_ct_per_kwh, 36.0);
        assert_eq!(cfg.price.dynamic.markup_ct_per_kwh, 20.0);
        assert_eq!(cfg.co2.g_per_kwh, 330.0);
        assert_eq!(minor_unit(&cfg), "ct");
        apply(&mut cfg, by_code("GB").unwrap());
        assert_eq!(minor_unit(&cfg), "p");
    }
}
