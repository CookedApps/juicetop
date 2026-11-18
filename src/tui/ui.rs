//! Drawing only. All numbers come from `dashboard::{Fast, Slow}`.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::symbols::{self, Marker};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Bar, BarChart, Block, BorderType, Borders, Cell, Chart, Clear, Dataset, GraphType,
    LegendPosition, Paragraph, Row, Table, Wrap,
};

use super::App;
use super::settings::Field;
use crate::dashboard::{ComponentRow, Range};
use crate::report::{fmt_co2, fmt_kwh, fmt_money, fmt_watts, local};

const ACCENT: Color = Color::Yellow;
const DIM: Color = Color::DarkGray;
/// Index 0 is the wall total; then one color per measured source.
const PALETTE: [Color; 7] = [
    Color::Yellow,
    Color::Cyan,
    Color::Magenta,
    Color::Green,
    Color::Blue,
    Color::LightRed,
    Color::LightCyan,
];

fn color(i: usize) -> Color {
    if i == usize::MAX {
        DIM
    } else {
        PALETTE[i % PALETTE.len()]
    }
}

fn block(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(title.into())
}

fn title(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(text.to_string(), Style::new().fg(ACCENT).bold()),
        Span::raw(" "),
    ])
}

pub fn draw(f: &mut Frame, app: &App) {
    // The tables take what their rows need (up to 16 lines); the graph gets the rest.
    let components = app.fast.components.len().max(app.slow.components.len()) + 1;
    let rows = components.max(app.slow.procs.len()).clamp(4, 13) as u16 + 3;
    let [header, chart, middle, bottom, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(rows),
        Constraint::Length(9),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    draw_chart(f, app, chart);

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(middle);
    draw_components(f, app, left);
    draw_procs(f, app, right);

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(bottom);
    draw_periods(f, app, left);
    draw_bars(f, app, right);

    draw_footer(f, app, footer);

    if app.settings.is_some() {
        draw_settings(f, app);
    } else if app.show_help {
        draw_help(f);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let fast = &app.fast;
    let sep = Span::styled(" │ ", Style::new().fg(DIM));
    let mut spans = vec![
        Span::styled(" juicetop", Style::new().fg(ACCENT).bold()),
        sep.clone(),
    ];
    match fast.now_wall_w {
        Some(w) => {
            spans.push(Span::styled(
                format!("⚡ {}", fmt_watts(w)),
                Style::new().bold(),
            ));
            spans.push(sep.clone());
            spans.push(Span::raw(format!(
                "{}/h",
                fmt_money(fast.cost_per_hour.unwrap_or(0.0), &app.cfg.currency)
            )));
        }
        None => spans.push(Span::styled(
            "collector offline",
            Style::new().fg(Color::Red),
        )),
    }
    spans.push(sep.clone());
    spans.push(Span::raw(fast.price_desc.clone()));
    spans.push(sep.clone());
    spans.push(Span::raw(format!("{:.0} g CO2/kWh", app.cfg.co2.g_per_kwh)));
    let clock = local(fast.now).format("%H:%M:%S ").to_string();
    let left = Line::from(spans);
    f.render_widget(Paragraph::new(left), area);
    f.render_widget(
        Paragraph::new(clock).alignment(Alignment::Right).fg(DIM),
        area,
    );
}

fn range_tabs(active: Range) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, r) in Range::ALL.iter().enumerate() {
        let style = if *r == active {
            Style::new().fg(Color::Black).bg(ACCENT).bold()
        } else {
            Style::new().fg(DIM)
        };
        spans.push(Span::styled(format!("{}", i + 1), Style::new().fg(DIM)));
        spans.push(Span::styled(format!(" {} ", r.label()), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans).alignment(Alignment::Right)
}

fn fmt_ago(secs: i64) -> String {
    match secs {
        s if s >= 86_400 && s % 86_400 == 0 => format!("-{}d", s / 86_400),
        s if s >= 86_400 => format!("-{:.1}d", s as f64 / 86_400.0),
        s if s >= 3600 && s % 3600 == 0 => format!("-{}h", s / 3600),
        s if s >= 3600 => format!("-{:.1}h", s as f64 / 3600.0),
        s if s >= 60 => format!("-{}m", s / 60),
        s => format!("-{s}s"),
    }
}

/// Rounds an axis maximum up to a readable number.
fn nice_max(v: f64) -> f64 {
    if v <= 0.0 {
        return 100.0;
    }
    let mag = 10f64.powf(v.log10().floor());
    let n = v / mag;
    let step = if n <= 1.0 {
        1.0
    } else if n <= 2.0 {
        2.0
    } else if n <= 2.5 {
        2.5
    } else if n <= 5.0 {
        5.0
    } else {
        10.0
    };
    step * mag
}

fn draw_chart(f: &mut Frame, app: &App, area: Rect) {
    let range = app.range;
    let blk = block(title(&format!("Power, last {}", range.label()))).title(range_tabs(range));
    let fast = &app.fast;
    if fast.chart.iter().all(|s| s.points.is_empty()) {
        let msg = Paragraph::new("no data in this range yet")
            .alignment(Alignment::Center)
            .fg(DIM)
            .block(blk);
        f.render_widget(msg, area);
        return;
    }
    let y_max = nice_max(fast.y_max * 1.05);
    let span = range.secs() as f64;
    let datasets: Vec<Dataset> = fast
        .chart
        .iter()
        .rev() // draw the total last, on top
        .map(|s| {
            let mut d = Dataset::default()
                .name(s.name.clone())
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(color(s.color)))
                .data(&s.points);
            if s.color == 0 {
                d = d.style(Style::new().fg(color(0)).bold());
            }
            d
        })
        .collect();
    let x_labels = vec![
        Span::styled(fmt_ago(range.secs()), Style::new().fg(DIM)),
        Span::styled(fmt_ago(range.secs() / 2), Style::new().fg(DIM)),
        Span::styled("now", Style::new().fg(DIM)),
    ];
    let y_labels = vec![
        Span::styled("0", Style::new().fg(DIM)),
        Span::styled(fmt_watts(y_max / 2.0), Style::new().fg(DIM)),
        Span::styled(fmt_watts(y_max), Style::new().fg(DIM)),
    ];
    let chart = Chart::new(datasets)
        .block(blk)
        .x_axis(
            Axis::default()
                .bounds([-span, 0.0])
                .labels(x_labels)
                .style(Style::new().fg(DIM)),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, y_max])
                .labels(y_labels)
                .style(Style::new().fg(DIM)),
        )
        .legend_position(Some(LegendPosition::TopLeft))
        .hidden_legend_constraints((Constraint::Percentage(60), Constraint::Percentage(60)));
    f.render_widget(chart, area);
}

fn draw_components(f: &mut Frame, app: &App, area: Rect) {
    let rows: &[ComponentRow] = if app.fast.components.is_empty() {
        &app.slow.components
    } else {
        &app.fast.components
    };
    let totals = app.fast.range_totals.or(app.slow.range_totals);
    let cur = &app.cfg.currency;
    let header = Row::new(["", "Component", "Now", "Avg", "Energy", "Cost", "CO2"])
        .style(Style::new().fg(DIM).add_modifier(Modifier::BOLD));
    let mut table_rows: Vec<Row> = rows
        .iter()
        .map(|r| {
            let style = if r.estimated {
                Style::new().fg(DIM)
            } else {
                Style::new()
            };
            Row::new(vec![
                Cell::from("■").style(Style::new().fg(color(r.color))),
                Cell::from(r.name.clone()),
                Cell::from(r.now_w.map(fmt_watts).unwrap_or_else(|| "·".into())),
                Cell::from(fmt_watts(r.avg_w)),
                Cell::from(fmt_kwh(r.kwh)),
                Cell::from(fmt_money(r.cost, cur)),
                Cell::from(fmt_co2(r.co2_g)),
            ])
            .style(style)
        })
        .collect();
    if let Some(t) = totals {
        table_rows.push(
            Row::new(vec![
                Cell::from("■").style(Style::new().fg(color(0))),
                Cell::from("Total at the wall"),
                Cell::from(
                    app.fast
                        .now_wall_w
                        .map(fmt_watts)
                        .unwrap_or_else(|| "·".into()),
                ),
                Cell::from(fmt_watts(t.avg_watts())),
                Cell::from(fmt_kwh(t.kwh())),
                Cell::from(fmt_money(t.cost, cur)),
                Cell::from(fmt_co2(t.co2_g)),
            ])
            .style(Style::new().fg(ACCENT).bold()),
        );
    }
    let widths = [
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(8),
    ];
    let t = Table::new(table_rows, widths)
        .header(header)
        .column_spacing(1)
        .block(block(title(&format!(
            "Components, last {}",
            app.range.label()
        ))));
    f.render_widget(t, area);
}

fn draw_procs(f: &mut Frame, app: &App, area: Rect) {
    let cur = &app.cfg.currency;
    let header = Row::new(["Process", "Now", "Avg", "Energy", "Cost", "%"])
        .style(Style::new().fg(DIM).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .slow
        .procs
        .iter()
        .skip(app.proc_scroll)
        .map(|p| {
            let dim = p.name.starts_with('(') || p.name.starts_with('[');
            Row::new(vec![
                Cell::from(p.name.clone()),
                Cell::from(p.now_w.map(fmt_watts).unwrap_or_else(|| "·".into())),
                Cell::from(fmt_watts(p.avg_w)),
                Cell::from(fmt_kwh(p.kwh)),
                Cell::from(fmt_money(p.cost, cur)),
                Cell::from(format!("{:>3.0}%", p.share * 100.0)),
            ])
            .style(if dim {
                Style::new().fg(DIM)
            } else {
                Style::new()
            })
        })
        .collect();
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(4),
    ];
    let scroll = if app.slow.procs.len() > 1 {
        format!(" ↑↓ {}/{} ", app.proc_scroll + 1, app.slow.procs.len())
    } else {
        String::new()
    };
    let blk = block(title(&format!(
        "Processes (CPU+GPU), last {}",
        app.range.label()
    )))
    .title(Line::from(scroll).alignment(Alignment::Right).fg(DIM));
    if rows.is_empty() {
        let msg = Paragraph::new("process data appears after the first full minute")
            .fg(DIM)
            .block(blk);
        f.render_widget(msg, area);
        return;
    }
    f.render_widget(
        Table::new(rows, widths)
            .header(header)
            .column_spacing(1)
            .block(blk),
        area,
    );
}

fn draw_periods(f: &mut Frame, app: &App, area: Rect) {
    let cur = &app.cfg.currency;
    let header = Row::new(["Period", "Energy", "Cost", "CO2", "Avg"])
        .style(Style::new().fg(DIM).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .slow
        .periods
        .iter()
        .map(|(name, t)| {
            let mark = if t.price_fallback { "*" } else { "" };
            let style = if name.starts_with("Month at") {
                Style::new().fg(DIM).italic()
            } else {
                Style::new()
            };
            Row::new(vec![
                Cell::from(*name),
                Cell::from(fmt_kwh(t.kwh())),
                Cell::from(format!("{}{mark}", fmt_money(t.cost, cur))),
                Cell::from(fmt_co2(t.co2_g)),
                Cell::from(fmt_watts(t.avg_watts())),
            ])
            .style(style)
        })
        .collect();
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(10),
        Constraint::Length(11),
        Constraint::Length(9),
        Constraint::Length(7),
    ];
    let since = app
        .slow
        .first_data
        .map(|t| format!(" since {} ", local(t).format("%Y-%m-%d")))
        .unwrap_or_default();
    let blk = block(title("Totals")).title(Line::from(since).alignment(Alignment::Right).fg(DIM));
    f.render_widget(
        Table::new(rows, widths)
            .header(header)
            .column_spacing(1)
            .block(blk),
        area,
    );
}

/// Green to red by share of the maximum, like btop's meters.
fn heat(frac: f64) -> Color {
    match frac {
        f if f < 0.25 => Color::Green,
        f if f < 0.5 => Color::LightGreen,
        f if f < 0.75 => Color::Yellow,
        f if f < 0.9 => Color::LightRed,
        _ => Color::Red,
    }
}

fn draw_bars(f: &mut Frame, app: &App, area: Rect) {
    let bars = &app.slow.bars;
    let max = bars.iter().map(|b| b.kwh).fold(0.0, f64::max);
    let total: f64 = bars.iter().map(|b| b.kwh).sum();
    let cost: f64 = bars.iter().map(|b| b.cost).sum();
    let blk = block(title(&format!("Energy {}", app.slow.bars_title))).title(
        Line::from(format!(
            " Σ {} · {} ",
            fmt_kwh(total),
            fmt_money(cost, &app.cfg.currency)
        ))
        .alignment(Alignment::Right)
        .fg(DIM),
    );
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    if bars.is_empty() || inner.width == 0 {
        return;
    }
    let n = bars.len() as u16;
    let gap = if inner.width / n >= 3 { 1 } else { 0 };
    let width = ((inner.width + gap) / n).saturating_sub(gap).max(1);
    // Center the bars in the leftover space.
    let used = (n * (width + gap)).saturating_sub(gap).min(inner.width);
    let inner = Rect {
        x: inner.x + (inner.width - used) / 2,
        width: used,
        ..inner
    };
    // Label every k-th bar so two-digit labels never touch.
    let every = 3u16.div_ceil(width + gap).max(1) as usize;
    let items: Vec<Bar> = bars
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let frac = if max > 0.0 { b.kwh / max } else { 0.0 };
            let mut bar = Bar::default()
                .value((b.kwh * 1000.0).round() as u64)
                .text_value(if width >= 4 {
                    format!("{:.1}", b.kwh)
                } else {
                    String::new()
                })
                .style(Style::new().fg(heat(frac)));
            if width >= 2 && i % every == 0 {
                bar = bar.label(Line::from(b.label.clone()).fg(DIM));
            }
            bar
        })
        .collect();
    let chart = BarChart::vertical(items)
        .bar_width(width)
        .bar_gap(gap)
        .bar_set(symbols::bar::NINE_LEVELS)
        .value_style(Style::new().fg(Color::Black));
    f.render_widget(chart, inner);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let key = |k: &'static str, what: &'static str| {
        vec![
            Span::styled(k, Style::new().fg(Color::Black).bg(DIM)),
            Span::styled(format!(" {what}  "), Style::new().fg(DIM)),
        ]
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, w) in [
        ("1-6", "range"),
        ("←→", "zoom"),
        ("↑↓", "processes"),
        ("s", "settings"),
        ("?", "help"),
        ("q", "quit"),
    ] {
        spans.extend(key(k, w));
    }
    let message = app
        .status
        .as_ref()
        .filter(|(_, at)| at.elapsed().as_secs() < 5)
        .map(|(m, _)| m.clone())
        .or_else(|| app.market_status.clone())
        .or_else(|| app.fast.notes.first().cloned());
    if let Some(m) = message {
        spans.push(Span::styled(m, Style::new().fg(Color::LightRed)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn popup(f: &mut Frame, width: u16, height: u16) -> Rect {
    let area = f
        .area()
        .centered(Constraint::Length(width), Constraint::Length(height));
    f.render_widget(Clear, area);
    area
}

fn draw_settings(f: &mut Frame, app: &App) {
    let Some(form) = &app.settings else { return };
    let area = popup(f, 78, Field::ALL.len() as u16 + 16);
    let blk = block(title("Settings")).title(
        Line::from(" ↑↓ select · Enter edit · ←→ change · Esc save and close ")
            .alignment(Alignment::Right)
            .fg(DIM),
    );
    let inner = blk.inner(area);
    f.render_widget(blk, area);

    let mut lines: Vec<Line> = Vec::new();
    let mut last_section = "";
    for (i, field) in Field::ALL.iter().enumerate() {
        if field.section() != last_section {
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(
                field.section(),
                Style::new().fg(ACCENT).bold(),
            ));
            last_section = field.section();
        }
        let selected = i == form.selected;
        let active = field.active(&app.cfg);
        let value = match (&form.editing, selected) {
            (Some(buf), true) => format!("{buf}▏"),
            _ if field.is_choice() => format!("‹ {} ›", field.get(&app.cfg)),
            _ => field.get(&app.cfg),
        };
        let base = if !active {
            Style::new().fg(DIM)
        } else {
            Style::new()
        };
        let style = if selected {
            base.bg(Color::DarkGray).fg(Color::White).bold()
        } else {
            base
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { " ▶ " } else { "   " }, style),
            Span::styled(format!("{:<30}", field.label()), style),
            Span::styled(format!("{value:<24}"), style),
        ]));
    }
    lines.push(Line::raw(""));
    let field = form.field();
    match &form.error {
        Some(e) => lines.push(Line::styled(format!("  {e}"), Style::new().fg(Color::Red))),
        None => lines.push(Line::styled(
            format!("  {}", field.help()),
            Style::new().fg(DIM),
        )),
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_help(f: &mut Frame) {
    let area = popup(f, 76, 22);
    let text = vec![
        Line::styled("What you see", Style::new().fg(ACCENT).bold()),
        Line::raw("CPU and GPU are measured (RAPL or socket PPT, NVML, amdgpu)."),
        Line::raw("Base load (mainboard, RAM, drives, fans) and PSU loss are estimates"),
        Line::raw("you can tune in the settings. Total at the wall = measured + estimates."),
        Line::raw(""),
        Line::raw("Processes share CPU energy by CPU time and GPU energy by GPU"),
        Line::raw("utilization (or held GPU memory when idle). Energy is shown at the wall."),
        Line::raw(""),
        Line::raw("Costs use your tariff per 15 minutes, so HT/NT and dynamic prices"),
        Line::raw("are exact. * marks totals partly priced with the fallback price."),
        Line::raw(""),
        Line::styled("Keys", Style::new().fg(ACCENT).bold()),
        Line::raw("1 to 6    5 min, 1 hour, 24 hours, 7 days, 30 days, 1 year"),
        Line::raw("← →       zoom out and in"),
        Line::raw("↑ ↓       scroll processes"),
        Line::raw("s         settings: price, tariff, CO2, estimates"),
        Line::raw("r         refresh now"),
        Line::raw("q         quit"),
        Line::raw(""),
        Line::styled("any key closes this", Style::new().fg(DIM)),
    ];
    f.render_widget(Paragraph::new(text).block(block(title("Help"))), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_axis_max() {
        assert_eq!(nice_max(0.0), 100.0);
        assert_eq!(nice_max(412.0), 500.0);
        assert_eq!(nice_max(180.0), 200.0);
        assert_eq!(nice_max(1000.0), 1000.0);
    }

    #[test]
    fn ago_labels() {
        assert_eq!(fmt_ago(300), "-5m");
        assert_eq!(fmt_ago(1800), "-30m");
        assert_eq!(fmt_ago(43_200), "-12h");
        assert_eq!(fmt_ago(7 * 86_400), "-7d");
        assert_eq!(fmt_ago(302_400), "-3.5d");
    }
}
