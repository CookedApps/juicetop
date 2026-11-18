//! Day-ahead market prices from the free aWATTar API (Germany and Austria, no key needed),
//! cached on disk so history is fetched only once.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::pricing::{Interval, PriceTable};

/// Days per request. The API answers multi-day ranges; keep requests small and polite.
const CHUNK_SECS: i64 = 7 * 86_400;
/// Never backfill further than this.
const MAX_BACKFILL_SECS: i64 = 400 * 86_400;

#[derive(Deserialize)]
struct Response {
    data: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    start_timestamp: i64,
    end_timestamp: i64,
    marketprice: f64,
    unit: String,
}

fn parse(body: &str) -> Result<Vec<Interval>> {
    let r: Response = serde_json::from_str(body).context("unexpected aWATTar response")?;
    Ok(r.data
        .into_iter()
        .filter(|e| e.unit.eq_ignore_ascii_case("eur/mwh"))
        .map(|e| Interval {
            start: e.start_timestamp / 1000,
            end: e.end_timestamp / 1000,
            eur_per_mwh: e.marketprice,
        })
        .collect())
}

fn fetch(region: &str, from: i64, to: i64) -> Result<Vec<Interval>> {
    // The API only returns intervals that start inside the range, so align to the hour or the
    // interval that is running at `from` would be missed.
    let from = from - from.rem_euclid(3600);
    let host = if region == "at" {
        "api.awattar.at"
    } else {
        "api.awattar.de"
    };
    let url = format!(
        "https://{host}/v1/marketdata?start={}&end={}",
        from * 1000,
        to * 1000
    );
    let body = ureq::get(&url)
        .header(
            "User-Agent",
            concat!("juicetop/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .with_context(|| format!("fetching {url}"))?
        .body_mut()
        .read_to_string()?;
    parse(&body)
}

pub fn cache_path(region: &str) -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("juicetop").join(format!("awattar-{region}.json")))
}

pub fn load_cache(region: &str) -> PriceTable {
    cache_path(region)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_cache(region: &str, table: &PriceTable) -> Result<()> {
    let Some(path) = cache_path(region) else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec(table)?)?;
    Ok(())
}

/// Fills gaps between `need_from` and tomorrow evening. Returns the updated table.
/// Network errors after a partial fetch still keep (and cache) what arrived.
pub fn refresh(region: &str, need_from: i64, now: i64) -> Result<PriceTable> {
    let mut table = load_cache(region);
    let need_from = need_from.max(now - MAX_BACKFILL_SECS);
    let want_to = now + 2 * 86_400;

    let mut ranges = Vec::new();
    match (table.first_start(), table.last_end()) {
        (Some(first), Some(last)) => {
            if need_from < first {
                ranges.push((need_from, first));
            }
            // Tomorrow's prices appear in the early afternoon; ask again until they are here.
            if last < now + 86_400 {
                ranges.push((last, want_to));
            }
        }
        _ => ranges.push((need_from, want_to)),
    }

    let mut result = Ok(());
    'outer: for (from, to) in ranges {
        let mut start = from;
        while start < to {
            let end = (start + CHUNK_SECS).min(to);
            match fetch(region, start, end) {
                Ok(iv) => table.merge(iv),
                Err(e) => {
                    result = Err(e);
                    break 'outer;
                }
            }
            start = end;
        }
    }
    save_cache(region, &table)?;
    result.map(|_| table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_payload() {
        let body = r#"{"object":"list","data":[
            {"start_timestamp":1794297600000,"end_timestamp":1794301200000,"marketprice":464.44,"unit":"Eur/MWh"},
            {"start_timestamp":1794301200000,"end_timestamp":1794304800000,"marketprice":-3.5,"unit":"Eur/MWh"}
        ],"url":"/de/v1/marketdata"}"#;
        let iv = parse(body).unwrap();
        assert_eq!(iv.len(), 2);
        assert_eq!(
            iv[0],
            Interval {
                start: 1794297600,
                end: 1794301200,
                eur_per_mwh: 464.44
            }
        );
        assert_eq!(iv[1].eur_per_mwh, -3.5);
    }
}
