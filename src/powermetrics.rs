//! Parsing of macOS `powermetrics` plist output. Kept apart from `macos.rs` so it builds and is
//! tested on every system.

/// One parsed `powermetrics` sample.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Length of the sample in nanoseconds.
    pub elapsed_ns: Option<f64>,
    pub cpu_mj: Option<f64>,
    pub gpu_mj: Option<f64>,
    pub ane_mj: Option<f64>,
    pub gpu_idle: Option<f64>,
    pub gpu_mhz: Option<f64>,
}

/// Reads the fields juicetop needs from one plist. The processor dict comes before the gpu dict,
/// and both carry a `gpu_energy`, so each search starts at its dict.
pub fn parse_sample(xml: &str) -> Sample {
    let processor = xml.find("<key>processor</key>").map(|i| &xml[i..]);
    let gpu = xml.find("<key>gpu</key>").map(|i| &xml[i..]);
    Sample {
        elapsed_ns: value_after(xml, "elapsed_ns"),
        cpu_mj: processor.and_then(|s| value_after(s, "cpu_energy")),
        gpu_mj: processor.and_then(|s| value_after(s, "gpu_energy")),
        ane_mj: processor.and_then(|s| value_after(s, "ane_energy")),
        gpu_idle: gpu.and_then(|s| value_after(s, "idle_ratio")),
        // Despite its name this field is in MHz.
        gpu_mhz: gpu.and_then(|s| value_after(s, "freq_hz")),
    }
}

impl Sample {
    /// Average CPU power over the sample.
    pub fn cpu_watts(&self) -> Option<f64> {
        self.watts(self.cpu_mj?)
    }

    /// Average GPU plus Neural Engine power over the sample.
    pub fn gpu_watts(&self) -> Option<f64> {
        self.watts(self.gpu_mj? + self.ane_mj.unwrap_or(0.0))
    }

    fn watts(&self, mj: f64) -> Option<f64> {
        let secs = self.elapsed_ns? / 1e9;
        (secs > 0.0).then(|| mj / 1000.0 / secs)
    }
}

/// The number in the `<integer>` or `<real>` element right after `<key>key</key>`.
fn value_after(s: &str, key: &str) -> Option<f64> {
    let k = format!("<key>{key}</key>");
    let rest = s[s.find(&k)? + k.len()..].trim_start();
    let rest = rest
        .strip_prefix("<integer>")
        .or_else(|| rest.strip_prefix("<real>"))?;
    rest[..rest.find('<')?].trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
  <dict>
    <key>elapsed_ns</key><integer>1004178750</integer>
    <key>processor</key>
    <dict>
      <key>clusters</key>
      <array><dict><key>idle_ratio</key><real>0.9</real></dict></array>
      <key>cpu_energy</key>
      <integer>4400</integer>
      <key>gpu_energy</key>
      <integer>1500</integer>
      <key>ane_energy</key>
      <integer>0</integer>
      <key>combined_power</key>
      <real>5930.1</real>
    </dict>
    <key>gpu</key>
    <dict>
      <key>freq_hz</key><real>714.836</real>
      <key>idle_ratio</key><real>0.25</real>
      <key>gpu_energy</key><integer>99</integer>
    </dict>
  </dict>
</plist>"#;

    #[test]
    fn parses_a_powermetrics_plist() {
        let s = parse_sample(SAMPLE);
        assert_eq!(s.elapsed_ns, Some(1004178750.0));
        assert!((s.cpu_watts().unwrap() - 4.3818).abs() < 1e-3);
        assert!((s.gpu_watts().unwrap() - 1.4938).abs() < 1e-3);
        assert_eq!(s.cpu_mj, Some(4400.0));
        assert_eq!(s.gpu_mj, Some(1500.0));
        assert_eq!(s.ane_mj, Some(0.0));
        assert_eq!(s.gpu_idle, Some(0.25));
        assert_eq!(s.gpu_mhz, Some(714.836));
        assert_eq!(parse_sample("<plist></plist>"), Sample::default());
    }
}
