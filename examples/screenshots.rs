//! Renders the README screenshots from demo data.
//!
//!     cargo run --release --example screenshots -- <out-dir>
//!
//! Writes one HTML page per scene. `scripts/screenshots.sh` turns them into PNGs with a
//! headless Chromium.

use std::fmt::Write as _;
use std::path::PathBuf;

use juicetop::config::PriceMode;
use juicetop::tui::{self, App, theme};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// Wednesday, 2026-11-18, 16:42:10 CET: a working afternoon.
const NOW: i64 = 1_795_016_530;
const COLS: u16 = 156;
const ROWS: u16 = 48;

fn key(app: &mut App, c: KeyCode) {
    app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
}

fn main() -> anyhow::Result<()> {
    // SAFETY: set before any other thread exists.
    unsafe { std::env::set_var("JUICETOP_TRUECOLOR", "1") };
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
        app.cfg.price.mode = PriceMode::Dynamic;
        app.set_market(juicetop::demo::market(NOW));
        app.set_clock(NOW);
        app.tick();
        app.prefill_proc_history(juicetop::demo::proc_history(NOW, 120));
        app
    };

    let app = new_app();
    write(&out, "dashboard", &tui::render(&app, COLS, ROWS))?;

    let mut app = new_app();
    key(&mut app, KeyCode::Char('4'));
    key(&mut app, KeyCode::Char('g'));
    key(&mut app, KeyCode::Char('u'));
    key(&mut app, KeyCode::Char('c'));
    write(&out, "week", &tui::render(&app, COLS, ROWS))?;

    let mut app = new_app();
    key(&mut app, KeyCode::Enter);
    write(&out, "detail", &tui::render(&app, COLS, ROWS))?;

    let mut app = new_app();
    app.start_setup();
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Right);
    write(&out, "setup", &tui::render(&app, COLS, ROWS))?;

    let app = new_app();
    write_themes(
        &out,
        &app,
        &[
            "nord",
            "gruvbox-dark",
            "catppuccin-mocha",
            "solarized-light",
        ],
    )?;

    println!("wrote {}", out.display());
    Ok(())
}

/// Tokyo Night inspired palette for the terminal colors the TUI uses.
fn rgb(c: Color, fg: bool) -> String {
    let named = match c {
        Color::Rgb(r, g, b) => return format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Reset if fg => "#c0caf5",
        Color::Reset => "transparent",
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
        _ => "#c0caf5",
    };
    named.to_string()
}

/// Braille cells drawn as dots, so the graphs look as dense as in a real terminal.
fn braille(sym: &str, color: &str) -> Option<String> {
    let c = sym.chars().next()? as u32;
    if !(0x2801..=0x28FF).contains(&c) {
        return None;
    }
    let bits = c - 0x2800;
    // Unicode braille bit order: dots 1, 2, 3 left column, 4, 5, 6 right, 7 and 8 the bottom row.
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
            let (x, y) = (2.3 + *col as f64 * 3.8, 2.3 + *row as f64 * 4.15);
            let _ = write!(svg, "<circle cx='{x}' cy='{y}' r='1.35' fill='{color}'/>");
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

fn rows_html(buf: &Buffer) -> String {
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
            let text = match braille(sym, &rgb(cell.fg, true)) {
                Some(svg) => svg,
                None if sym.trim().is_empty() => "&nbsp;".to_string(),
                None => escape(sym),
            };
            let _ = write!(rows, "<span style=\"{style}\">{text}</span>");
            x += w;
        }
        rows.push_str("</div>\n");
    }
    rows
}

/// A terminal window around a rendered buffer.
fn window(buf: &Buffer, title: &str, bg: &str, fg: &str) -> String {
    format!(
        r#"<div class="win" style="background:{bg}">
<div class="bar"><span class="dot" style="background:#ff5f57"></span><span class="dot" style="background:#febc2e"></span><span class="dot" style="background:#28c840"></span><span class="title" style="color:{fg}">{title}</span></div>
<div class="term">
{rows}</div></div>"#,
        rows = rows_html(buf)
    )
}

fn page(
    dir: &std::path::Path,
    name: &str,
    body: &str,
    width: u32,
    height: u32,
    columns: u32,
) -> anyhow::Result<()> {
    let html = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><style>
html, body {{ margin: 0; }}
body {{
  width: {width}px; height: {height}px; display: grid; grid-template-columns: repeat({columns}, auto);
  gap: 40px; align-content: center; justify-content: center;
  background: linear-gradient(135deg, #2b1f4a 0%, #1b2a4a 55%, #10303a 100%);
}}
.win {{
  border-radius: 12px; overflow: hidden;
  box-shadow: 0 24px 60px rgba(0,0,0,.55), 0 0 0 1px rgba(255,255,255,.06);
}}
.bar {{ height: 34px; display: flex; align-items: center; padding: 0 14px; gap: 8px; background: rgba(0,0,0,.18); }}
.dot {{ width: 12px; height: 12px; border-radius: 50%; }}
.title {{ flex: 1; text-align: center; margin-right: 52px; font: 500 13px 'JetBrains Mono', monospace; }}
.term {{ padding: 14px 16px 16px; font: 400 14px/17px 'JetBrains Mono', 'DejaVu Sans Mono', monospace; }}
.r {{ height: 17px; white-space: pre; }}
.r span {{ display: inline-block; width: 0.6em; height: 17px; text-align: center; vertical-align: top; overflow: visible; }}
</style></head><body>
{body}
</body></html>
"#
    );
    std::fs::write(dir.join(format!("{name}.html")), html)?;
    Ok(())
}

fn win_size(buf: &Buffer) -> (u32, u32) {
    (
        (buf.area.width as f64 * 8.4) as u32 + 32,
        buf.area.height as u32 * 17 + 34 + 30,
    )
}

fn write(dir: &std::path::Path, name: &str, buf: &Buffer) -> anyhow::Result<()> {
    let (w, h) = win_size(buf);
    page(
        dir,
        name,
        &window(buf, "juicetop", "#1a1b26", "#737aa2"),
        w + 96,
        h + 96,
        1,
    )
}

/// Four themes side by side.
fn write_themes(dir: &std::path::Path, app: &App, themes: &[&str]) -> anyhow::Result<()> {
    let mut body = String::new();
    let mut size = (0, 0);
    for name in themes {
        theme::set(name);
        let buf = tui::render(app, 104, 32);
        size = win_size(&buf);
        let t = theme::t();
        let bg = tone_hex(t.popup_bg, "#1a1b26");
        let fg = tone_hex(t.dim, "#737aa2");
        body.push_str(&window(&buf, t.label, &bg, &fg));
    }
    theme::set("juicetop");
    let cols = 2;
    let rows = themes.len().div_ceil(2) as u32;
    page(
        dir,
        "themes",
        &body,
        size.0 * cols + 40 + 96,
        size.1 * rows + 40 * (rows - 1) + 96,
        cols,
    )
}

fn tone_hex(t: theme::Tone, fallback: &str) -> String {
    match t {
        theme::Tone::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        theme::Tone::Ansi(_) => fallback.to_string(),
    }
}
