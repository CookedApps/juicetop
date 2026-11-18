//! Colors. True color when the terminal supports it, the nearest xterm-256 color otherwise.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

/// An RGB color that degrades to 256 colors on terminals without true color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

fn truecolor() -> bool {
    static TC: OnceLock<bool> = OnceLock::new();
    *TC.get_or_init(|| {
        if std::env::var_os("JUICETOP_TRUECOLOR").is_some() {
            return true;
        }
        let ct = std::env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let term = std::env::var("TERM_PROGRAM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        ct.contains("truecolor")
            || ct.contains("24bit")
            || ["wezterm", "iterm.app", "ghostty", "vscode", "kitty"]
                .iter()
                .any(|t| term.contains(t))
    })
}

impl Rgb {
    pub fn color(self) -> Color {
        if truecolor() {
            Color::Rgb(self.0, self.1, self.2)
        } else {
            Color::Indexed(xterm256(self))
        }
    }

    pub fn lerp(self, other: Rgb, t: f64) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
        Rgb(
            mix(self.0, other.0),
            mix(self.1, other.1),
            mix(self.2, other.2),
        )
    }
}

impl From<Rgb> for Color {
    fn from(c: Rgb) -> Self {
        c.color()
    }
}

/// Nearest color in the xterm 6x6x6 cube or gray ramp.
fn xterm256(Rgb(r, g, b): Rgb) -> u8 {
    let level = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as u16 - 35) / 40) as u8
        }
    };
    let steps = [0u8, 95, 135, 175, 215, 255];
    let (lr, lg, lb) = (level(r), level(g), level(b));
    let cube = (steps[lr as usize], steps[lg as usize], steps[lb as usize]);
    let dist = |(x, y, z): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    let avg = (r as u16 + g as u16 + b as u16) / 3;
    let gray_idx = if avg > 238 {
        23
    } else {
        (avg.saturating_sub(3) / 10).min(23) as u8
    };
    let gray_v = 8 + gray_idx * 10;
    if dist((gray_v, gray_v, gray_v)) < dist(cube) {
        232 + gray_idx
    } else {
        16 + 36 * lr + 6 * lg + lb
    }
}

/// A color ramp sampled by a value between 0 and 1.
#[derive(Debug, Clone, Copy)]
pub struct Gradient(pub &'static [Rgb]);

impl Gradient {
    pub fn at(&self, t: f64) -> Rgb {
        let stops = self.0;
        if stops.len() == 1 {
            return stops[0];
        }
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let pos = t * (stops.len() - 1) as f64;
        let i = (pos.floor() as usize).min(stops.len() - 2);
        stops[i].lerp(stops[i + 1], pos - i as f64)
    }

    pub fn color(&self, t: f64) -> Color {
        self.at(t).color()
    }
}

// The "juice" palette.
pub const TEXT: Rgb = Rgb(0xdd, 0xe3, 0xf2);
pub const DIM: Rgb = Rgb(0x6c, 0x73, 0x93);
pub const FAINT: Rgb = Rgb(0x3a, 0x3f, 0x58);
pub const MUTED: Rgb = Rgb(0x9a, 0xa3, 0xc4);
pub const SELECT_BG: Rgb = Rgb(0x2b, 0x30, 0x4a);
pub const MONEY: Rgb = Rgb(0xff, 0xd1, 0x66);
pub const CO2: Rgb = Rgb(0x7b, 0xe0, 0x9a);
pub const GOOD: Rgb = Rgb(0x5e, 0xf1, 0xa6);
pub const WARN: Rgb = Rgb(0xff, 0xb3, 0x47);
pub const BAD: Rgb = Rgb(0xff, 0x5c, 0x8a);
pub const BRAND: Rgb = Rgb(0xff, 0x9f, 0x43);
pub const CPU: Rgb = Rgb(0x4c, 0xc9, 0xf0);
pub const ESTIMATE: Rgb = Rgb(0x7f, 0x8c, 0xb8);
pub const AI: Rgb = Rgb(0x8f, 0xa8, 0xff);

/// Box border colors.
pub const BORDER_POWER: Rgb = Rgb(0xff, 0x9f, 0x43);
pub const BORDER_PROCS: Rgb = Rgb(0xff, 0x6f, 0x9a);
pub const BORDER_ENERGY: Rgb = Rgb(0xff, 0xd1, 0x66);
pub const BORDER_TOTALS: Rgb = Rgb(0x5e, 0xf1, 0xa6);
pub const BORDER_PRICES: Rgb = Rgb(0x7b, 0xe0, 0x9a);

/// Power level: mint at idle, through lime and amber, to hot pink at full load.
pub const POWER: Gradient = Gradient(&[
    Rgb(0x3d, 0xd6, 0xa8),
    Rgb(0x9d, 0xe8, 0x4f),
    Rgb(0xff, 0xd1, 0x66),
    Rgb(0xff, 0x8c, 0x5a),
    Rgb(0xff, 0x4f, 0x8b),
]);

/// Price level: green when cheap, red when expensive.
pub const PRICE: Gradient = Gradient(&[
    Rgb(0x3d, 0xd6, 0xa8),
    Rgb(0xd7, 0xe3, 0x5b),
    Rgb(0xff, 0x9f, 0x43),
    Rgb(0xff, 0x4f, 0x8b),
]);

/// The logo.
pub const BRAND_GRADIENT: Gradient = Gradient(&[
    Rgb(0xff, 0xd1, 0x66),
    Rgb(0xff, 0x8c, 0x5a),
    Rgb(0xff, 0x4f, 0x8b),
]);

/// GPUs, in order.
const GPUS: [Rgb; 4] = [
    Rgb(0xb3, 0x88, 0xff),
    Rgb(0xff, 0x7a, 0xd9),
    Rgb(0x4d, 0xe0, 0xd0),
    Rgb(0x7a, 0xa2, 0xf7),
];

pub fn gpu(i: usize) -> Rgb {
    GPUS[i % GPUS.len()]
}

pub fn fg(c: Rgb) -> Style {
    Style::new().fg(c.color())
}

pub fn bold(c: Rgb) -> Style {
    fg(c).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_hits_its_stops() {
        assert_eq!(POWER.at(0.0), POWER.0[0]);
        assert_eq!(POWER.at(1.0), *POWER.0.last().unwrap());
        assert_eq!(POWER.at(f64::NAN), POWER.0[0]);
        assert_eq!(Gradient(&[TEXT]).at(0.7), TEXT);
    }

    #[test]
    fn maps_to_xterm256() {
        assert_eq!(xterm256(Rgb(0, 0, 0)), 16);
        assert_eq!(xterm256(Rgb(255, 255, 255)), 231);
        assert_eq!(xterm256(Rgb(255, 0, 0)), 196);
        assert_eq!(xterm256(Rgb(128, 128, 128)), 244);
    }
}
