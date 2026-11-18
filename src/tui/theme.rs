//! Themes. Every color the dashboard uses comes from the active `Theme`.
//!
//! Most themes keep the frame (borders, titles, labels) neutral and spend color on data:
//! graphs, meters, money, device identity and status. True color when the terminal supports
//! it, the nearest xterm-256 color otherwise; the "terminal" theme uses the 16 ANSI colors of
//! your terminal's own scheme.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::style::{Color, Modifier, Style};

/// A theme color: an exact RGB value, or one of the terminal's ANSI colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Rgb(u8, u8, u8),
    Ansi(Color),
}

pub const fn hex(v: u32) -> Tone {
    Tone::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

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

impl Tone {
    pub fn color(self) -> Color {
        match self {
            Tone::Ansi(c) => c,
            Tone::Rgb(r, g, b) if truecolor() => Color::Rgb(r, g, b),
            Tone::Rgb(r, g, b) => Color::Indexed(xterm256(r, g, b)),
        }
    }

    /// Mixes two colors. ANSI colors cannot be mixed, so they switch at the halfway point.
    pub fn lerp(self, other: Tone, t: f64) -> Tone {
        let t = t.clamp(0.0, 1.0);
        match (self, other) {
            (Tone::Rgb(r1, g1, b1), Tone::Rgb(r2, g2, b2)) => {
                let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
                Tone::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
            }
            _ if t < 0.5 => self,
            _ => other,
        }
    }
}

/// Nearest color in the xterm 6x6x6 cube or gray ramp.
fn xterm256(r: u8, g: u8, b: u8) -> u8 {
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
pub struct Gradient(pub &'static [Tone]);

impl Gradient {
    pub fn at(&self, t: f64) -> Tone {
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

pub struct Theme {
    pub name: &'static str,
    pub label: &'static str,
    /// Main text and values.
    pub text: Tone,
    /// Labels, axes, hints.
    pub dim: Tone,
    /// Rules, empty meter cells, inactive things.
    pub faint: Tone,
    /// Secondary values.
    pub muted: Tone,
    /// Panel titles and borders.
    pub title: Tone,
    pub border: Tone,
    pub select_bg: Tone,
    pub popup_bg: Tone,
    /// Highlight: active tab, selection marker, setup.
    pub accent: Tone,
    /// Text on top of `accent`.
    pub on_accent: Tone,
    pub money: Tone,
    pub co2: Tone,
    pub good: Tone,
    pub warn: Tone,
    pub bad: Tone,
    pub cpu: Tone,
    pub gpus: [Tone; 4],
    pub estimate: Tone,
    /// Per-panel colors, used for borders and titles when `colored_panels` is set.
    pub colored_panels: bool,
    pub panel_power: Tone,
    pub panel_procs: Tone,
    pub panel_energy: Tone,
    pub panel_totals: Tone,
    pub panel_prices: Tone,
    pub panel_models: Tone,
    pub power: Gradient,
    pub price: Gradient,
    pub logo: Gradient,
}

impl Theme {
    pub fn gpu(&self, i: usize) -> Tone {
        self.gpus[i % self.gpus.len()]
    }

    /// `color` mixed out of the background: dark at `t = 0`, full color at `t = 1`.
    /// ANSI colors cannot be mixed and come back unchanged.
    pub fn rise(&self, color: Tone, t: f64) -> Tone {
        match (self.popup_bg, color) {
            (Tone::Rgb(..), Tone::Rgb(..)) => self.popup_bg.lerp(color, 0.28 + 0.72 * t),
            _ => color,
        }
    }
}

const POWER_JUICE: &[Tone] = &[
    hex(0x3dd6a8),
    hex(0x9de84f),
    hex(0xffd166),
    hex(0xff8c5a),
    hex(0xff4f8b),
];
const PRICE_JUICE: &[Tone] = &[hex(0x3dd6a8), hex(0xd7e35b), hex(0xff9f43), hex(0xff4f8b)];

/// The default: a neutral frame, color for data.
const JUICETOP: Theme = Theme {
    name: "juicetop",
    label: "juicetop",
    text: hex(0xd8dce8),
    dim: hex(0x6e7487),
    faint: hex(0x353948),
    muted: hex(0x9aa0b2),
    title: hex(0xc9cede),
    border: hex(0x3d4252),
    select_bg: hex(0x2a2e3b),
    popup_bg: hex(0x16171e),
    accent: hex(0xff9f43),
    on_accent: hex(0x16161e),
    money: hex(0xf2c46d),
    co2: hex(0xb7bccb),
    good: hex(0x6fd3a0),
    warn: hex(0xe8b05a),
    bad: hex(0xf07a8a),
    cpu: hex(0x5fc4e8),
    gpus: [hex(0xb39cf0), hex(0xf08fc8), hex(0x5ed3c6), hex(0x86a8f0)],
    estimate: hex(0x8a90a6),
    colored_panels: false,
    panel_power: hex(0x3d4252),
    panel_procs: hex(0x3d4252),
    panel_energy: hex(0x3d4252),
    panel_totals: hex(0x3d4252),
    panel_prices: hex(0x3d4252),
    panel_models: hex(0x3d4252),
    power: Gradient(POWER_JUICE),
    price: Gradient(PRICE_JUICE),
    logo: Gradient(&[hex(0xff9f43)]),
};

/// Builds an IDE-style theme from its palette.
#[allow(clippy::too_many_arguments)]
const fn ide(
    name: &'static str,
    label: &'static str,
    fg: Tone,
    comment: Tone,
    line: Tone,
    selection: Tone,
    bg: Tone,
    accent: Tone,
    yellow: Tone,
    green: Tone,
    red: Tone,
    cyan: Tone,
    gpus: [Tone; 4],
    power: &'static [Tone],
    price: &'static [Tone],
) -> Theme {
    Theme {
        name,
        label,
        text: fg,
        dim: comment,
        faint: line,
        muted: fg,
        title: fg,
        border: line,
        select_bg: selection,
        popup_bg: bg,
        accent,
        on_accent: bg,
        money: yellow,
        co2: fg,
        good: green,
        warn: yellow,
        bad: red,
        cpu: cyan,
        gpus,
        estimate: comment,
        colored_panels: false,
        panel_power: line,
        panel_procs: line,
        panel_energy: line,
        panel_totals: line,
        panel_prices: line,
        panel_models: line,
        power: Gradient(power),
        price: Gradient(price),
        logo: Gradient(&[]),
    }
}

macro_rules! ide_theme {
    ($name:literal, $label:literal, fg: $fg:expr, comment: $comment:expr, line: $line:expr, selection: $sel:expr,
     bg: $bg:expr, accent: $accent:expr, yellow: $y:expr, green: $g:expr, red: $r:expr, orange: $o:expr,
     magenta: $m:expr, cyan: $c:expr, gpus: $gpus:expr) => {
        Theme {
            logo: Gradient(&[$accent]),
            ..ide(
                $name,
                $label,
                $fg,
                $comment,
                $line,
                $sel,
                $bg,
                $accent,
                $y,
                $g,
                $r,
                $c,
                $gpus,
                &[$g, $y, $o, $r, $m],
                &[$g, $y, $o, $r],
            )
        }
    };
}

pub static THEMES: &[Theme] = &[
    JUICETOP,
    Theme {
        name: "juicetop-vivid",
        label: "juicetop vivid",
        text: hex(0xdde3f2),
        dim: hex(0x6c7393),
        faint: hex(0x3a3f58),
        muted: hex(0x9aa3c4),
        title: hex(0xdde3f2),
        border: hex(0x3a3f58),
        select_bg: hex(0x2b304a),
        popup_bg: hex(0x141522),
        co2: hex(0x7be09a),
        good: hex(0x5ef1a6),
        warn: hex(0xffb347),
        bad: hex(0xff5c8a),
        money: hex(0xffd166),
        cpu: hex(0x4cc9f0),
        gpus: [hex(0xb388ff), hex(0xff7ad9), hex(0x4de0d0), hex(0x7aa2f7)],
        estimate: hex(0x7f8cb8),
        colored_panels: true,
        panel_power: hex(0xff9f43),
        panel_procs: hex(0xff6f9a),
        panel_energy: hex(0xffd166),
        panel_totals: hex(0x5ef1a6),
        panel_prices: hex(0x7be09a),
        panel_models: hex(0x8fa8ff),
        logo: Gradient(&[hex(0xffd166), hex(0xff8c5a), hex(0xff4f8b)]),
        ..JUICETOP
    },
    Theme {
        name: "juicetop-muted",
        label: "juicetop muted",
        text: hex(0xcfd2da),
        dim: hex(0x6b6f7b),
        faint: hex(0x33363f),
        muted: hex(0x9599a4),
        title: hex(0xc4c7cf),
        border: hex(0x3a3d46),
        select_bg: hex(0x2a2c33),
        popup_bg: hex(0x17181c),
        accent: hex(0xc9a27e),
        on_accent: hex(0x17181c),
        money: hex(0xd4b88a),
        co2: hex(0xb0b3bb),
        good: hex(0x8fb59a),
        warn: hex(0xc8a978),
        bad: hex(0xc48a8a),
        cpu: hex(0x8fb0bf),
        gpus: [hex(0xa99cc2), hex(0xc39bb1), hex(0x8fb8b0), hex(0x9aa8c4)],
        estimate: hex(0x868a96),
        power: Gradient(&[
            hex(0x6f9e8a),
            hex(0x9fb07a),
            hex(0xc8b27a),
            hex(0xc69477),
            hex(0xb9808e),
        ]),
        price: Gradient(&[hex(0x6f9e8a), hex(0xb8b27e), hex(0xc69477), hex(0xb9808e)]),
        logo: Gradient(&[hex(0xc9a27e)]),
        ..JUICETOP
    },
    Theme {
        name: "greyscale",
        label: "Greyscale",
        text: hex(0xe0e0e0),
        dim: hex(0x7a7a7a),
        faint: hex(0x3a3a3a),
        muted: hex(0xa8a8a8),
        title: hex(0xd0d0d0),
        border: hex(0x444444),
        select_bg: hex(0x2e2e2e),
        popup_bg: hex(0x161616),
        accent: hex(0xe8e8e8),
        on_accent: hex(0x161616),
        money: hex(0xf2f2f2),
        co2: hex(0xbdbdbd),
        good: hex(0xd0d0d0),
        warn: hex(0xbdbdbd),
        bad: hex(0x9a9a9a),
        cpu: hex(0xd6d6d6),
        gpus: [hex(0xa6a6a6), hex(0xececec), hex(0x8c8c8c), hex(0xbcbcbc)],
        estimate: hex(0x7a7a7a),
        panel_power: hex(0x444444),
        power: Gradient(&[hex(0x4d4d4d), hex(0x9a9a9a), hex(0xf2f2f2)]),
        price: Gradient(&[hex(0x5a5a5a), hex(0xa0a0a0), hex(0xf2f2f2)]),
        logo: Gradient(&[hex(0xe8e8e8)]),
        ..JUICETOP
    },
    ide_theme!("tokyo-night", "Tokyo Night",
        fg: hex(0xc0caf5), comment: hex(0x565f89), line: hex(0x3b4261), selection: hex(0x283457),
        bg: hex(0x1a1b26), accent: hex(0x7aa2f7), yellow: hex(0xe0af68), green: hex(0x9ece6a),
        red: hex(0xf7768e), orange: hex(0xff9e64), magenta: hex(0xbb9af7), cyan: hex(0x7dcfff),
        gpus: [hex(0xbb9af7), hex(0xff9e64), hex(0x1abc9c), hex(0x7aa2f7)]),
    ide_theme!("catppuccin-mocha", "Catppuccin Mocha",
        fg: hex(0xcdd6f4), comment: hex(0x6c7086), line: hex(0x45475a), selection: hex(0x313244),
        bg: hex(0x181825), accent: hex(0xcba6f7), yellow: hex(0xf9e2af), green: hex(0xa6e3a1),
        red: hex(0xf38ba8), orange: hex(0xfab387), magenta: hex(0xf5c2e7), cyan: hex(0x89dceb),
        gpus: [hex(0xcba6f7), hex(0xf5c2e7), hex(0x94e2d5), hex(0x89b4fa)]),
    ide_theme!("dracula", "Dracula",
        fg: hex(0xf8f8f2), comment: hex(0x6272a4), line: hex(0x44475a), selection: hex(0x44475a),
        bg: hex(0x21222c), accent: hex(0xbd93f9), yellow: hex(0xf1fa8c), green: hex(0x50fa7b),
        red: hex(0xff5555), orange: hex(0xffb86c), magenta: hex(0xff79c6), cyan: hex(0x8be9fd),
        gpus: [hex(0xbd93f9), hex(0xff79c6), hex(0x8be9fd), hex(0xffb86c)]),
    ide_theme!("nord", "Nord",
        fg: hex(0xd8dee9), comment: hex(0x616e88), line: hex(0x434c5e), selection: hex(0x3b4252),
        bg: hex(0x2e3440), accent: hex(0x88c0d0), yellow: hex(0xebcb8b), green: hex(0xa3be8c),
        red: hex(0xbf616a), orange: hex(0xd08770), magenta: hex(0xb48ead), cyan: hex(0x8fbcbb),
        gpus: [hex(0xb48ead), hex(0x81a1c1), hex(0xd08770), hex(0x5e81ac)]),
    ide_theme!("gruvbox-dark", "Gruvbox Dark",
        fg: hex(0xebdbb2), comment: hex(0x928374), line: hex(0x504945), selection: hex(0x3c3836),
        bg: hex(0x282828), accent: hex(0xfe8019), yellow: hex(0xfabd2f), green: hex(0xb8bb26),
        red: hex(0xfb4934), orange: hex(0xfe8019), magenta: hex(0xd3869b), cyan: hex(0x8ec07c),
        gpus: [hex(0xd3869b), hex(0x83a598), hex(0xfe8019), hex(0x8ec07c)]),
    ide_theme!("one-dark", "One Dark",
        fg: hex(0xabb2bf), comment: hex(0x5c6370), line: hex(0x4b5263), selection: hex(0x3e4451),
        bg: hex(0x21252b), accent: hex(0x61afef), yellow: hex(0xe5c07b), green: hex(0x98c379),
        red: hex(0xe06c75), orange: hex(0xd19a66), magenta: hex(0xc678dd), cyan: hex(0x56b6c2),
        gpus: [hex(0xc678dd), hex(0xe06c75), hex(0x56b6c2), hex(0x61afef)]),
    ide_theme!("monokai", "Monokai",
        fg: hex(0xf8f8f2), comment: hex(0x75715e), line: hex(0x49483e), selection: hex(0x3e3d32),
        bg: hex(0x1e1f1c), accent: hex(0xfd971f), yellow: hex(0xe6db74), green: hex(0xa6e22e),
        red: hex(0xf92672), orange: hex(0xfd971f), magenta: hex(0xae81ff), cyan: hex(0x66d9ef),
        gpus: [hex(0xae81ff), hex(0xf92672), hex(0x66d9ef), hex(0xfd971f)]),
    ide_theme!("solarized-dark", "Solarized Dark",
        fg: hex(0x93a1a1), comment: hex(0x586e75), line: hex(0x0d4654), selection: hex(0x073642),
        bg: hex(0x002b36), accent: hex(0x268bd2), yellow: hex(0xb58900), green: hex(0x859900),
        red: hex(0xdc322f), orange: hex(0xcb4b16), magenta: hex(0xd33682), cyan: hex(0x2aa198),
        gpus: [hex(0x6c71c4), hex(0xd33682), hex(0x2aa198), hex(0xcb4b16)]),
    ide_theme!("solarized-light", "Solarized Light",
        fg: hex(0x586e75), comment: hex(0x93a1a1), line: hex(0xe4ddc8), selection: hex(0xeee8d5),
        bg: hex(0xfdf6e3), accent: hex(0x268bd2), yellow: hex(0xb58900), green: hex(0x859900),
        red: hex(0xdc322f), orange: hex(0xcb4b16), magenta: hex(0xd33682), cyan: hex(0x2aa198),
        gpus: [hex(0x6c71c4), hex(0xd33682), hex(0x2aa198), hex(0xcb4b16)]),
    ide_theme!("github-light", "GitHub Light",
        fg: hex(0x1f2328), comment: hex(0x6e7781), line: hex(0xd0d7de), selection: hex(0xddf4ff),
        bg: hex(0xffffff), accent: hex(0x0969da), yellow: hex(0x9a6700), green: hex(0x1a7f37),
        red: hex(0xcf222e), orange: hex(0xbc4c00), magenta: hex(0x8250df), cyan: hex(0x1b7c83),
        gpus: [hex(0x8250df), hex(0xbf3989), hex(0x1b7c83), hex(0x0969da)]),
    Theme {
        name: "terminal",
        label: "Terminal colors",
        text: Tone::Ansi(Color::Reset),
        dim: Tone::Ansi(Color::DarkGray),
        faint: Tone::Ansi(Color::DarkGray),
        muted: Tone::Ansi(Color::Gray),
        title: Tone::Ansi(Color::Reset),
        border: Tone::Ansi(Color::DarkGray),
        select_bg: Tone::Ansi(Color::DarkGray),
        popup_bg: Tone::Ansi(Color::Reset),
        accent: Tone::Ansi(Color::Yellow),
        on_accent: Tone::Ansi(Color::Black),
        money: Tone::Ansi(Color::Yellow),
        co2: Tone::Ansi(Color::Reset),
        good: Tone::Ansi(Color::Green),
        warn: Tone::Ansi(Color::Yellow),
        bad: Tone::Ansi(Color::Red),
        cpu: Tone::Ansi(Color::Cyan),
        gpus: [
            Tone::Ansi(Color::Magenta),
            Tone::Ansi(Color::LightMagenta),
            Tone::Ansi(Color::LightCyan),
            Tone::Ansi(Color::Blue),
        ],
        estimate: Tone::Ansi(Color::Gray),
        colored_panels: false,
        panel_power: Tone::Ansi(Color::DarkGray),
        panel_procs: Tone::Ansi(Color::DarkGray),
        panel_energy: Tone::Ansi(Color::DarkGray),
        panel_totals: Tone::Ansi(Color::DarkGray),
        panel_prices: Tone::Ansi(Color::DarkGray),
        panel_models: Tone::Ansi(Color::DarkGray),
        power: Gradient(&[
            Tone::Ansi(Color::Green),
            Tone::Ansi(Color::LightGreen),
            Tone::Ansi(Color::Yellow),
            Tone::Ansi(Color::LightRed),
            Tone::Ansi(Color::Red),
        ]),
        price: Gradient(&[
            Tone::Ansi(Color::Green),
            Tone::Ansi(Color::Yellow),
            Tone::Ansi(Color::LightRed),
            Tone::Ansi(Color::Red),
        ]),
        logo: Gradient(&[Tone::Ansi(Color::Yellow)]),
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The active theme.
pub fn t() -> &'static Theme {
    &THEMES[CURRENT.load(Ordering::Relaxed).min(THEMES.len() - 1)]
}

/// Activates a theme by name; unknown names fall back to the default. Returns the name used.
pub fn set(name: &str) -> &'static str {
    let i = THEMES.iter().position(|th| th.name == name).unwrap_or(0);
    CURRENT.store(i, Ordering::Relaxed);
    THEMES[i].name
}

pub fn label_of(name: &str) -> &'static str {
    THEMES
        .iter()
        .find(|th| th.name == name)
        .map_or(THEMES[0].label, |th| th.label)
}

/// The theme after (or before) `name`.
pub fn cycle(name: &str, forward: bool) -> &'static str {
    let i = THEMES.iter().position(|th| th.name == name).unwrap_or(0);
    let n = THEMES.len();
    THEMES[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }]
    .name
}

pub fn fg(c: Tone) -> Style {
    Style::new().fg(c.color())
}

pub fn bold(c: Tone) -> Style {
    fg(c).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_hits_its_stops() {
        let p = t().power;
        assert_eq!(THEMES[0].power.at(0.0), POWER_JUICE[0]);
        assert_eq!(THEMES[0].power.at(1.0), *POWER_JUICE.last().unwrap());
        assert_eq!(p.at(f64::NAN), p.at(0.0));
        const ONE: &[Tone] = &[hex(0x123456)];
        assert_eq!(Gradient(ONE).at(0.7), hex(0x123456));
        const TWO: &[Tone] = &[Tone::Ansi(Color::Green), Tone::Ansi(Color::Red)];
        let ansi = Gradient(TWO);
        assert_eq!(ansi.at(0.2), Tone::Ansi(Color::Green));
        assert_eq!(ansi.at(0.9), Tone::Ansi(Color::Red));
    }

    #[test]
    fn maps_to_xterm256() {
        assert_eq!(xterm256(0, 0, 0), 16);
        assert_eq!(xterm256(255, 255, 255), 231);
        assert_eq!(xterm256(255, 0, 0), 196);
        assert_eq!(xterm256(128, 128, 128), 244);
    }

    #[test]
    fn themes_are_complete_and_unique() {
        let mut names: Vec<&str> = THEMES.iter().map(|th| th.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), THEMES.len());
        for th in THEMES {
            assert!(
                !th.logo.0.is_empty() && th.power.0.len() >= 2 && th.price.0.len() >= 2,
                "{}",
                th.name
            );
        }
        assert_eq!(cycle("juicetop", false), "terminal");
        assert_eq!(label_of("nord"), "Nord");
    }
}
