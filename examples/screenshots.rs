//! Renders the README screenshots from demo data.
//!
//!     cargo run --release --example screenshots -- <out-dir>
//!
//! Writes one HTML page per scene. `scripts/screenshots.sh` turns them into PNGs with a
//! headless Chromium.

use std::fmt::Write as _;
use std::path::PathBuf;

use juicetop::config::PriceMode;
use juicetop::pricing::{Interval, PriceTable};
use juicetop::tui::{self, App};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// Wednesday, 2026-11-18, 16:42:10 CET: a working afternoon with the LLM server busy.
const NOW: i64 = 1_795_016_530;
const COLS: u16 = 140;
const ROWS: u16 = 44;

fn key(app: &mut App, c: KeyCode) {
    app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
}

/// Plausible day-ahead prices: cheap at night and midday, expensive in the evening peak.
fn market() -> PriceTable {
    let mut t = PriceTable::default();
    let start = NOW - NOW.rem_euclid(3600) - 125 * 86_400;
    t.merge((0..130 * 24).map(|h| {
        let s = start + h * 3600;
        let hour = ((s + 3600) / 3600) % 24;
        let eur = match hour {
            0..=5 => 70.0,
            6..=8 => 125.0,
            9..=15 => 85.0,
            16..=20 => 165.0,
            _ => 105.0,
        } + ((h * 37) % 23) as f64;
        Interval {
            start: s,
            end: s + 3600,
            eur_per_mwh: eur,
        }
    }));
    t
}

fn main() -> anyhow::Result<()> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/screenshots".into()),
    );
    std::fs::create_dir_all(&out)?;
    let data = tempfile::tempdir()?;
    let cfg = juicetop::demo::seed(data.path(), 120, NOW)?;

    let new_app = || {
        let mut app = App::new(cfg.clone(), data.path().join("config.toml"));
        app.set_clock(NOW);
        app
    };

    let mut app = new_app();
    key(&mut app, KeyCode::Char('3'));
    write(&out, "dashboard", &tui::render(&app, COLS, ROWS))?;

    let mut app = new_app();
    key(&mut app, KeyCode::Char('5'));
    write(&out, "month", &tui::render(&app, COLS, ROWS))?;

    let mut app = new_app();
    app.cfg.price.mode = PriceMode::Dynamic;
    app.set_market(market());
    key(&mut app, KeyCode::Char('2'));
    key(&mut app, KeyCode::Char('s'));
    write(&out, "settings", &tui::render(&app, COLS, ROWS))?;

    println!("wrote {}", out.display());
    Ok(())
}

/// Tokyo Night inspired palette for the terminal colors the TUI uses.
fn rgb(c: Color, fg: bool) -> &'static str {
    match c {
        Color::Reset => {
            if fg {
                "#c0caf5"
            } else {
                "transparent"
            }
        }
        Color::Black => "#15161e",
        Color::Red => "#f7768e",
        Color::Green => "#9ece6a",
        Color::Yellow => "#e0af68",
        Color::Blue => "#7aa2f7",
        Color::Magenta => "#bb9af7",
        Color::Cyan => "#7dcfff",
        Color::Gray => "#a9b1d6",
        Color::DarkGray => "#565f89",
        Color::LightRed => "#ff9e64",
        Color::LightGreen => "#b9f27c",
        Color::LightYellow => "#ffdb69",
        Color::LightBlue => "#8db0ff",
        Color::LightMagenta => "#c7a9ff",
        Color::LightCyan => "#b4f9f8",
        Color::White => "#c0caf5",
        _ => "#c0caf5",
    }
}

/// Braille cells drawn as dots, so the graphs look as dense as in a real terminal.
fn braille(sym: &str, color: &str) -> Option<String> {
    let c = sym.chars().next()? as u32;
    if !(0x2801..=0x28FF).contains(&c) {
        return None;
    }
    let bits = c - 0x2800;
    // Unicode braille bit order: dots 1,2,3 left column, 4,5,6 right, 7 and 8 the bottom row.
    let dots = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    let mut svg = String::from("<svg width='8.4' height='17' style='display:block'>");
    for (i, (col, row)) in dots.iter().enumerate() {
        if bits & (1 << i) != 0 {
            let _ = write!(
                svg,
                "<circle cx='{}' cy='{}' r='1.35' fill='{color}'/>",
                2.3 + *col as f64 * 3.8,
                2.3 + *row as f64 * 4.15
            );
        }
    }
    svg.push_str("</svg>");
    Some(svg)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn write(dir: &std::path::Path, name: &str, buf: &Buffer) -> anyhow::Result<()> {
    let area = buf.area;
    let mut rows = String::new();
    for y in 0..area.height {
        rows.push_str("<div class=r>");
        let mut x = 0;
        while x < area.width {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            let w = UnicodeWidthStr::width(sym).max(1) as u16;
            let mut style = format!("color:{};", rgb(cell.fg, true));
            if cell.bg != Color::Reset {
                let _ = write!(style, "background:{};", rgb(cell.bg, false));
            }
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str("font-weight:700;");
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                style.push_str("font-style:italic;");
            }
            if w == 2 {
                style.push_str("width:1.2em;");
            }
            let text = match braille(sym, rgb(cell.fg, true)) {
                Some(svg) => svg,
                None if sym.trim().is_empty() => "&nbsp;".to_string(),
                None => escape(sym),
            };
            let _ = write!(rows, "<span style=\"{style}\">{text}</span>");
            x += w;
        }
        rows.push_str("</div>\n");
    }
    let html = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><style>
html, body {{ margin: 0; }}
body {{
  width: {page_w}px; height: {page_h}px; display: flex; align-items: center; justify-content: center;
  background: linear-gradient(135deg, #2b1f4a 0%, #1b2a4a 55%, #10303a 100%);
}}
.win {{
  background: #1a1b26; border-radius: 12px; overflow: hidden;
  box-shadow: 0 24px 60px rgba(0,0,0,.55), 0 0 0 1px rgba(255,255,255,.06);
}}
.bar {{ height: 34px; display: flex; align-items: center; padding: 0 14px; gap: 8px; background: #16161e; }}
.dot {{ width: 12px; height: 12px; border-radius: 50%; }}
.title {{ flex: 1; text-align: center; margin-right: 52px; color: #737aa2;
  font: 500 13px 'JetBrains Mono', monospace; }}
.term {{ padding: 14px 16px 16px; font: 400 14px/17px 'JetBrains Mono', 'DejaVu Sans Mono', monospace; }}
.r {{ height: 17px; white-space: pre; }}
.r span {{ display: inline-block; width: 0.6em; height: 17px; text-align: center; vertical-align: top; overflow: visible; }}
</style></head><body><div class="win">
<div class="bar"><span class="dot" style="background:#ff5f57"></span><span class="dot" style="background:#febc2e"></span><span class="dot" style="background:#28c840"></span><span class="title">juicetop</span></div>
<div class="term">
{rows}</div></div></body></html>
"#,
        page_w = (area.width as f64 * 8.4) as u32 + 32 + 96,
        page_h = area.height as u32 * 17 + 34 + 30 + 96,
    );
    std::fs::write(dir.join(format!("{name}.html")), html)?;
    Ok(())
}
