//! Drawing only. All numbers come from `dashboard::{Fast, Slow}`.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Row, Table, Wrap};

use super::settings::Field;
use super::theme::{self, Tone, bold, fg};
use super::widgets::{self, Bars, Column};
use super::{App, Hit};
use crate::config::PriceMode;
use crate::dashboard::{BarMetric, GraphPoint, ProcRow, Range, SourceView};
use crate::report::{fmt_co2, fmt_count, fmt_kwh, fmt_money, fmt_watts, local};
use crate::sensors::SourceKind;

const SUPERSCRIPT: [&str; 6] = ["¹", "²", "³", "⁴", "⁵", "⁶"];

pub fn source_color(s: &SourceView) -> Tone {
    match s.kind {
        SourceKind::Cpu => theme::t().cpu,
        SourceKind::Gpu => theme::t().gpu(s.slot),
    }
}

/// A panel with the theme's frame. `color` is used only by themes with colored panels.
pub fn panel(title: &str, color: Tone) -> Block<'static> {
    let th = theme::t();
    let (border, title_color) = if th.colored_panels {
        (color.lerp(th.faint, 0.35), color)
    } else {
        (th.border, th.title)
    };
    framed(title, border, title_color)
}

/// A panel whose title carries an identity color (a device).
pub fn tinted_panel(title: &str, color: Tone) -> Block<'static> {
    let th = theme::t();
    let border = if th.colored_panels {
        color.lerp(th.faint, 0.35)
    } else {
        th.border
    };
    framed(title, border, color)
}

fn framed(title: &str, border: Tone, title_color: Tone) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(border))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(title.to_string(), bold(title_color)),
            Span::raw(" "),
        ]))
}

fn minor(app: &App) -> &'static str {
    crate::presets::minor_unit(&app.cfg)
}

fn hit(app: &App, area: Rect, h: Hit) {
    app.hits.borrow_mut().push((area, h));
}

pub fn draw(f: &mut Frame, app: &App) {
    app.hits.borrow_mut().clear();
    let area = f.area();
    if let Some(ob) = &app.onboarding {
        super::onboarding::draw(f, ob, app);
        return;
    }
    let show_devices = area.height >= 30 && !app.fast.sources.is_empty();
    let [header, power, devices, middle, totals, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(2),
        Constraint::Length(if show_devices { 5 } else { 0 }),
        Constraint::Fill(3),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(f, app, header);
    draw_power(f, app, power);
    if show_devices {
        draw_devices(f, app, devices);
    }
    draw_middle(f, app, middle);
    draw_totals(f, app, totals);
    draw_footer(f, app, footer);

    if app.detail.is_some() {
        draw_detail(f, app);
    } else if app.settings.is_some() {
        draw_settings(f, app);
    } else if app.show_help {
        draw_help(f);
    }
}

// ------------------------------------------------------------------ header

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let fast = &app.fast;
    let sep = || Span::styled("  ", Style::new());
    let mut spans = vec![Span::raw(" ")];
    spans.extend(widgets::gradient_text(
        "⚡juicetop",
        theme::t().logo,
        Style::new().bold(),
    ));
    spans.push(sep());
    match &fast.live {
        Some(now) => {
            spans.push(Span::styled("● live", fg(theme::t().good)));
            spans.push(sep());
            let t = now.wall / fast.peak_w.max(now.wall).max(1.0);
            spans.push(Span::styled(
                fmt_watts(now.wall),
                bold(theme::t().power.at(t)),
            ));
            spans.push(sep());
            spans.push(Span::styled(
                format!("{}/h", fmt_money(now.cost_per_hour, &app.cfg.currency)),
                fg(theme::t().money),
            ));
        }
        None => spans.push(Span::styled("○ collector offline", bold(theme::t().bad))),
    }
    if let Some(today) = app.slow.card("Today") {
        spans.push(sep());
        spans.push(Span::styled("today ", fg(theme::t().dim)));
        spans.push(Span::styled(
            fmt_money(today.totals.cost, &app.cfg.currency),
            fg(theme::t().money),
        ));
    }
    if let Some(p) = fast.price {
        spans.push(sep());
        let mode = match app.cfg.price.mode {
            PriceMode::Fixed => "fixed",
            PriceMode::TimeOfUse => "time of use",
            PriceMode::Dynamic => "dynamic",
        };
        spans.push(Span::styled(format!("{mode} "), fg(theme::t().dim)));
        spans.push(Span::styled(
            format!("{:.1} {}", p.ct_per_kwh, minor(app)),
            fg(theme::t().text),
        ));
        if let Some(next) = fast.price_next {
            let (arrow, c) = if next < p.ct_per_kwh - 0.05 {
                (" ▼", theme::t().good)
            } else if next > p.ct_per_kwh + 0.05 {
                (" ▲", theme::t().bad)
            } else {
                (" ▶", theme::t().dim)
            };
            spans.push(Span::styled(arrow, fg(c)));
        }
    }
    spans.push(sep());
    spans.push(Span::styled(
        format!("{:.0} g CO2/kWh", app.cfg.co2.g_per_kwh),
        fg(theme::t().co2),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let clock = local(fast.now).format("%H:%M:%S ").to_string();
    f.render_widget(
        Paragraph::new(clock)
            .alignment(Alignment::Right)
            .style(fg(theme::t().dim)),
        area,
    );
}

// ------------------------------------------------------------------ power graph

fn range_tabs(app: &App, area: Rect) -> Line<'static> {
    let mut spans = Vec::new();
    // Tabs are right aligned in the title; remember their positions for mouse clicks.
    let labels: Vec<String> = Range::ALL
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}{}", SUPERSCRIPT[i], r.label()))
        .collect();
    let total: u16 = labels
        .iter()
        .map(|l| l.chars().count() as u16 + 2)
        .sum::<u16>()
        + 1;
    let mut x = area.right().saturating_sub(total + 1);
    for (i, (r, l)) in Range::ALL.iter().zip(labels).enumerate() {
        let w = l.chars().count() as u16 + 2;
        hit(app, Rect::new(x, area.y, w, 1), Hit::Range(i));
        x += w;
        let style = if *r == app.range {
            Style::new()
                .fg(theme::t().on_accent.color())
                .bg(theme::t().accent.color())
                .bold()
        } else {
            fg(theme::t().muted)
        };
        spans.push(Span::styled(format!(" {l} "), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans).alignment(Alignment::Right)
}

fn fmt_axis_w(w: f64) -> String {
    if w >= 1000.0 {
        format!("{:.1}k", w / 1000.0)
    } else {
        format!("{w:.0}")
    }
}

/// Rounds an axis maximum up to a readable number.
pub fn nice_max(v: f64) -> f64 {
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

fn time_label(range: Range, ts: i64, now: i64) -> String {
    let t = local(ts);
    match range {
        Range::M5 => format!("-{}m", (now - ts + 30) / 60),
        Range::H1 | Range::H24 => t.format("%H:%M").to_string(),
        Range::D7 => t.format("%a %H:%M").to_string(),
        Range::D30 => t.format("%d %b").to_string(),
        Range::Y1 => t.format("%b").to_string(),
    }
}

fn draw_power(f: &mut Frame, app: &App, area: Rect) {
    let mode = if app.stacked { "stacked" } else { "total" };
    let block =
        panel(&format!("power · {mode}"), theme::t().panel_power).title(range_tabs(app, area));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 20 {
        return;
    }
    let info_w = if inner.width >= 90 { 30 } else { 0 };
    let [graph_area, info_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(info_w)]).areas(inner);
    draw_graph(f.buffer_mut(), app, graph_area);
    if info_w > 0 {
        draw_power_info(f, app, info_area);
    }
}

fn draw_graph(buf: &mut Buffer, app: &App, area: Rect) {
    let fast = &app.fast;
    let axis_w = 6u16;
    if area.width <= axis_w + 4 || area.height < 3 {
        return;
    }
    let plot = Rect::new(
        area.x + axis_w,
        area.y,
        area.width - axis_w - 1,
        area.height - 1,
    );
    let max = nice_max(fast.peak_w.max(fast.live.as_ref().map_or(0.0, |n| n.wall)) * 1.08);

    for (frac, row) in [
        (1.0, plot.y),
        (0.5, plot.y + plot.height / 2),
        (0.0, plot.bottom() - 1),
    ] {
        let label = if frac == 0.0 {
            "0".to_string()
        } else {
            fmt_axis_w(max * frac)
        };
        buf.set_string(area.x, row, format!("{label:>5}"), fg(theme::t().dim));
        buf.set_string(area.x + 5, row, "┤", fg(theme::t().faint));
    }
    let span = app.range.secs() as f64;
    let ticks = (plot.width / 18).clamp(2, 8);
    for i in 0..=ticks {
        let frac = i as f64 / ticks as f64;
        let x = plot.x + ((plot.width - 1) as f64 * frac) as u16;
        let ts = fast.now - (span * (1.0 - frac)) as i64;
        let label = if i == ticks {
            "now".to_string()
        } else {
            time_label(app.range, ts, fast.now)
        };
        let len = label.chars().count() as u16;
        let lx = x
            .saturating_sub(len / 2)
            .max(plot.x)
            .min(plot.right().saturating_sub(len));
        buf.set_string(lx, plot.bottom(), label, fg(theme::t().dim));
    }

    if fast.graph.is_empty() {
        let msg = "no data in this range yet";
        let x = plot.x + plot.width.saturating_sub(msg.len() as u16) / 2;
        buf.set_string(x, plot.y + plot.height / 2, msg, fg(theme::t().dim));
        return;
    }

    // Resample every series to two dot columns per cell.
    let n = plot.width as usize * 2;
    let pts = |get: &dyn Fn(&GraphPoint) -> f64| -> Vec<Option<f64>> {
        let p: Vec<(f64, f64)> = fast.graph.iter().map(|g| (g.x, get(g))).collect();
        widgets::resample(&p, -span, 0.0, n)
    };
    let total = pts(&|g| g.total);
    let columns: Vec<Option<Column>> = if app.stacked {
        // Bottom to top: CPU, GPUs, then base load and PSU loss.
        let mut layers: Vec<(Vec<Option<f64>>, Tone)> = fast
            .sources
            .iter()
            .map(|s| {
                let id = s.id;
                (
                    pts(&move |g: &GraphPoint| g.sources.get(&id).copied().unwrap_or(0.0)),
                    source_color(s),
                )
            })
            .collect();
        layers.push((pts(&|g| g.base + g.psu), theme::t().estimate));
        (0..n)
            .map(|i| {
                total[i]?;
                let mut acc = 0.0;
                let segments = layers
                    .iter()
                    .map(|(v, c)| {
                        acc += v[i].unwrap_or(0.0);
                        (acc, Some(*c))
                    })
                    .collect();
                Some(Column { segments })
            })
            .collect()
    } else {
        total.iter().map(|v| v.map(Column::single)).collect()
    };
    widgets::area(buf, plot, &columns, max, theme::t().power);
    widgets::hline(buf, plot, fast.avg_w, max, theme::t().muted);
}

fn kv(label: &str, value: String, color: Tone) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<9}"), fg(theme::t().dim)),
        Span::styled(value, fg(color)),
    ])
}

fn draw_power_info(f: &mut Frame, app: &App, area: Rect) {
    let fast = &app.fast;
    let cur = &app.cfg.currency;
    let totals = fast.range_totals.or(app.slow.range_totals);
    let rule = || {
        Line::styled(
            "─".repeat(area.width.saturating_sub(2) as usize),
            fg(theme::t().faint),
        )
    };
    let mut lines = Vec::new();
    match &fast.live {
        Some(n) => {
            let t = (n.wall / fast.peak_w.max(n.wall).max(1.0)).clamp(0.0, 1.0);
            lines.push(Line::from(vec![
                Span::styled(fmt_watts(n.wall), bold(theme::t().power.at(t))),
                Span::styled("  at the wall", fg(theme::t().dim)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(fmt_money(n.cost_per_hour, cur), bold(theme::t().money)),
                Span::styled(" per hour", fg(theme::t().dim)),
            ]));
        }
        None => lines.push(Line::styled("no live data", fg(theme::t().bad))),
    }
    lines.push(rule());
    lines.push(kv("peak", fmt_watts(fast.peak_w), theme::t().text));
    lines.push(kv("average", fmt_watts(fast.avg_w), theme::t().muted));
    if let Some(t) = totals {
        lines.push(kv("energy", fmt_kwh(t.kwh()), theme::t().text));
        lines.push(kv("cost", fmt_money(t.cost, cur), theme::t().money));
        lines.push(kv("CO2", fmt_co2(t.co2_g), theme::t().co2));
    }
    if let Some(n) = &fast.live {
        lines.push(rule());
        let mut rows: Vec<(String, f64, Tone)> = fast
            .sources
            .iter()
            .map(|s| {
                let label = match s.kind {
                    SourceKind::Cpu => "CPU".to_string(),
                    SourceKind::Gpu => format!("GPU {}", s.slot),
                };
                (
                    label,
                    n.sources.get(&s.id).copied().unwrap_or(0.0),
                    source_color(s),
                )
            })
            .collect();
        rows.push(("base + PSU".into(), n.base + n.psu, theme::t().estimate));
        for (name, w, c) in rows {
            let share = if n.wall > 0.0 { w / n.wall } else { 0.0 };
            let short: String = name.chars().take(11).collect();
            let mut line = vec![
                Span::styled("■ ", fg(c)),
                Span::styled(format!("{short:<11}"), fg(theme::t().muted)),
                Span::styled(format!("{:>7} ", fmt_watts(w)), fg(theme::t().text)),
            ];
            line.extend(widgets::meter_tone(share, 6, c));
            lines.push(Line::from(line));
        }
    }
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(1),
        ..area
    };
    f.render_widget(Paragraph::new(lines), inner);
}

// ------------------------------------------------------------------ devices

fn draw_devices(f: &mut Frame, app: &App, area: Rect) {
    let fast = &app.fast;
    let n = fast.sources.len().max(1) as u32;
    let cols = Layout::horizontal(vec![Constraint::Ratio(1, n); n as usize]).split(area);
    for (s, rect) in fast.sources.iter().zip(cols.iter()) {
        draw_device(f, app, s, *rect);
    }
}

fn draw_device(f: &mut Frame, app: &App, s: &SourceView, area: Rect) {
    let color = source_color(s);
    let kind = match s.kind {
        SourceKind::Cpu => "cpu".to_string(),
        SourceKind::Gpu => format!("gpu{}", s.slot),
    };
    let block = tinted_panel(&s.name, color)
        .title(Line::styled(format!(" {kind} "), fg(theme::t().dim)).alignment(Alignment::Right));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width < 16 {
        return;
    }
    let stat = app.device(&s.key);
    let now_w = app
        .fast
        .live
        .as_ref()
        .and_then(|n| n.sources.get(&s.id).copied());
    let history = app.fast.history.get(&s.id).cloned().unwrap_or_default();
    let peak = history.iter().copied().fold(0.0, f64::max);
    let limit = stat
        .and_then(|d| d.power_limit_w)
        .unwrap_or_else(|| nice_max(peak.max(1.0)));
    let w = inner.width as usize;

    // Line 1: watts and a meter against the power limit.
    let mut l1 = vec![Span::styled(
        format!("{:>8} ", now_w.map(fmt_watts).unwrap_or_else(|| "·".into())),
        bold(color),
    )];
    let label = format!(" of {}", fmt_watts(limit));
    let meter_w = w.saturating_sub(9 + label.chars().count()).max(4);
    l1.extend(widgets::meter(
        now_w.unwrap_or(0.0) / limit,
        meter_w,
        theme::t().power,
    ));
    l1.push(Span::styled(label, fg(theme::t().dim)));

    // Line 2: utilization, clock, temperature.
    let mut l2 = Vec::new();
    if let Some(u) = stat.and_then(|d| d.util_pct) {
        l2.push(Span::styled("    util ", fg(theme::t().dim)));
        l2.extend(widgets::meter(
            u / 100.0,
            (w / 5).clamp(4, 12),
            theme::t().power,
        ));
        l2.push(Span::styled(format!(" {u:>3.0}%"), fg(theme::t().text)));
    }
    if let Some(c) = stat.and_then(|d| d.clock_mhz) {
        let s = if c >= 1000.0 {
            format!("  {:.1} GHz", c / 1000.0)
        } else {
            format!("  {c:.0} MHz")
        };
        l2.push(Span::styled(s, fg(theme::t().muted)));
    }
    if let Some(t) = stat.and_then(|d| d.temp_c) {
        l2.push(Span::styled(
            format!("  {t:.0}°C"),
            fg(theme::t().power.at(((t - 35.0) / 55.0).clamp(0.0, 1.0))),
        ));
    }

    // Line 3: the last minutes as a sparkline, and memory.
    let mem = stat
        .and_then(|d| d.mem_used_mb.zip(d.mem_total_mb))
        .map(|(u, t)| {
            let label = if s.kind == SourceKind::Gpu {
                "VRAM"
            } else {
                "RAM"
            };
            format!("  {label} {:.0}/{:.0} GB", u / 1024.0, t / 1024.0)
        });
    let spark_w = w
        .saturating_sub(mem.as_ref().map_or(0, |m| m.chars().count()) + 1)
        .max(4);
    let mut l3 = widgets::dots(&history, spark_w, limit, color.lerp(theme::t().faint, 0.15));
    if let Some(m) = mem {
        l3.push(Span::styled(m, fg(theme::t().muted)));
    }
    f.render_widget(
        Paragraph::new(vec![Line::from(l1), Line::from(l2), Line::from(l3)]),
        inner,
    );
}

// ------------------------------------------------------------------ processes, bars, models, prices

fn draw_middle(f: &mut Frame, app: &App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
    draw_procs(f, app, left);
    let models_h = if app.slow.models.is_empty() {
        0
    } else {
        app.slow.models.len().min(4) as u16 + 3
    };
    let mut prices_h = if app.slow.prices.is_some() { 7 } else { 0 };
    let mut models_h = models_h;
    // Hide panels that do not fit rather than squash them; the bars need 8 rows.
    if right.height < 8 + models_h + prices_h {
        prices_h = 0;
    }
    if right.height < 8 + models_h {
        models_h = 0;
    }
    let [bars, models, prices] = Layout::vertical([
        Constraint::Min(6),
        Constraint::Length(models_h),
        Constraint::Length(prices_h),
    ])
    .areas(right);
    draw_bars(f, app, bars);
    if models_h > 0 {
        draw_models(f, app, models);
    }
    if prices_h > 0 {
        draw_prices(f, app, prices);
    }
}

/// Sparkline color of a process: its device color, toned down.
fn spark_color(p: &ProcRow) -> Tone {
    let th = theme::t();
    let base = if p.gpu_share.is_some_and(|g| g >= 0.5) {
        th.gpu(0)
    } else {
        th.cpu
    };
    base.lerp(th.faint, 0.35)
}

/// The color of a process's CPU/GPU tag.
fn tag_tone(p: &ProcRow) -> Option<Tone> {
    let th = theme::t();
    match p.gpu_share {
        Some(g) if g >= 0.6 => Some(th.gpu(0)),
        Some(g) if g >= 0.15 => Some(th.muted),
        Some(_) => Some(th.cpu),
        None => None,
    }
}

pub fn tag(p: &ProcRow) -> Span<'static> {
    match p.gpu_share {
        Some(g) if g >= 0.6 => Span::styled("GPU", fg(theme::t().gpu(0))),
        Some(g) if g >= 0.15 => Span::styled("C+G", fg(theme::t().muted)),
        Some(_) => Span::styled("CPU", fg(theme::t().cpu)),
        None => Span::raw(""),
    }
}

fn draw_procs(f: &mut Frame, app: &App, area: Rect) {
    let cur = &app.cfg.currency;
    let procs = app.sorted_procs();
    let sort = Line::from(vec![
        Span::styled(" sort ", fg(theme::t().dim)),
        Span::styled(format!("{} ▾ ", app.proc_sort.label()), fg(theme::t().text)),
    ])
    .alignment(Alignment::Right);
    let block = panel(
        &format!("processes · {}", app.range.label()),
        theme::t().panel_procs,
    )
    .title(sort);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if procs.is_empty() {
        let msg = "Process data appears after the collector's first full minute.";
        f.render_widget(Paragraph::new(msg).style(fg(theme::t().dim)), inner);
        return;
    }
    let visible = inner.height.saturating_sub(1) as usize;
    let sel = app.proc_sel.min(procs.len() - 1);
    let offset = sel.saturating_sub(visible.saturating_sub(1));
    let header = Row::new(["Process", "", "Now", "", "Avg", "Energy", "Cost", "%"])
        .style(fg(theme::t().dim).add_modifier(Modifier::BOLD));
    // Narrow terminals drop the sparkline, then the average, to keep names readable.
    let spark_w = if inner.width >= 84 { 8usize } else { 0 };
    let avg_w: u16 = if inner.width >= 70 { 7 } else { 0 };
    // One scale for all sparklines, so a 5 W process does not look as busy as a 500 W one.
    let spark_max = app
        .proc_history
        .values()
        .flat_map(|h| h.iter().copied())
        .fold(1.0, f64::max);
    let rows: Vec<Row> = procs
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .map(|(i, p)| {
            let dim = p.name.starts_with('(') || p.name.starts_with('[');
            let hist: Vec<f64> = app
                .proc_history
                .get(&p.name)
                .map(|h| h.iter().copied().collect())
                .unwrap_or_default();
            // Rows fade towards the bottom of the list, like btop's process box.
            let th = theme::t();
            let shown = visible.min(procs.len() - offset).max(2);
            let depth = (i - offset) as f64 / (shown - 1) as f64;
            let fade = |c: Tone| {
                if i == sel {
                    c
                } else {
                    c.lerp(th.faint, depth * 0.6)
                }
            };
            let base = fg(fade(if dim { th.dim } else { th.text }));
            let mut tag_span = tag(p);
            if let Some(c) = tag_span.style.fg {
                tag_span.style = tag_span
                    .style
                    .fg(tag_tone(p).map(fade).map_or(c, |t| t.color()));
            }
            let row = Row::new(vec![
                Line::styled(p.name.clone(), base),
                Line::from(tag_span),
                Line::styled(p.now_w.map(fmt_watts).unwrap_or_default(), base)
                    .alignment(Alignment::Right),
                Line::from(widgets::dots(
                    &hist,
                    spark_w,
                    spark_max,
                    fade(spark_color(p)),
                )),
                Line::styled(fmt_watts(p.avg_w), fg(fade(th.muted))).alignment(Alignment::Right),
                Line::styled(fmt_kwh(p.kwh), base).alignment(Alignment::Right),
                Line::styled(fmt_money(p.cost, cur), fg(fade(th.money)))
                    .alignment(Alignment::Right),
                Line::styled(format!("{:.0}%", p.share * 100.0), fg(fade(th.muted)))
                    .alignment(Alignment::Right),
            ]);
            let y = inner.y + 1 + (i - offset) as u16;
            hit(app, Rect::new(inner.x, y, inner.width, 1), Hit::Proc(i));
            if i == sel {
                row.style(Style::new().bg(theme::t().select_bg.color()))
            } else {
                row
            }
        })
        .collect();
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(3),
        Constraint::Length(8),
        Constraint::Length(spark_w as u16),
        Constraint::Length(avg_w),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(4),
    ];
    f.render_widget(
        Table::new(rows, widths).header(header).column_spacing(1),
        inner,
    );
}

fn draw_bars(f: &mut Frame, app: &App, area: Rect) {
    let slow = &app.slow;
    let cur = &app.cfg.currency;
    let m = app.bar_metric;
    let unit = match m {
        BarMetric::Energy => "kWh",
        BarMetric::Cost => cur.as_str(),
        BarMetric::Co2 => "g CO2",
    };
    let mut tabs = vec![Span::styled(" u ", fg(theme::t().dim))];
    for (mm, label) in [
        (BarMetric::Energy, "kWh"),
        (BarMetric::Cost, cur.as_str()),
        (BarMetric::Co2, "CO2"),
    ] {
        let st = if mm == m {
            bold(theme::t().accent)
        } else {
            fg(theme::t().dim)
        };
        tabs.push(Span::styled(format!("{label} "), st));
    }
    let block = panel(
        &format!("energy {} · {unit}", slow.bars_title),
        theme::t().panel_energy,
    )
    .title(Line::from(tabs).alignment(Alignment::Right));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let bars = &slow.bars;
    if bars.is_empty() || inner.height < 4 || inner.width < 20 {
        return;
    }
    let sel = app
        .bar_sel
        .filter(|i| *i < bars.len())
        .or_else(|| bars.iter().position(|b| b.current))
        .unwrap_or(bars.len() - 1);
    let b = &bars[sel];

    // The selected bar, spelled out.
    let when = match slow.bars_title {
        "per hour" => format!(
            "{} to {}",
            local(b.start).format("%H:%M"),
            local(b.end).format("%H:%M")
        ),
        "per day" => local(b.start).format("%a %d %b").to_string(),
        _ => local(b.start).format("%B %Y").to_string(),
    };
    let mut info = vec![
        Span::styled(format!("{when}  "), bold(theme::t().text)),
        Span::styled(fmt_kwh(b.kwh), fg(theme::t().text)),
        Span::styled("  ", Style::new()),
        Span::styled(fmt_money(b.cost, cur), fg(theme::t().money)),
        Span::styled("  ", Style::new()),
        Span::styled(fmt_co2(b.co2_g), fg(theme::t().co2)),
    ];
    if let Some(ct) = b
        .ct_per_kwh
        .filter(|_| app.cfg.price.mode != PriceMode::Fixed)
    {
        info.push(Span::styled(
            format!("  {ct:.1} {}/kWh", minor(app)),
            fg(theme::t().muted),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(info)),
        Rect { height: 1, ..inner },
    );

    let values: Vec<f64> = bars.iter().map(|b| b.value(m)).collect();
    let max = nice_max(values.iter().copied().fold(0.0, f64::max) * 1.05);
    let axis_w = 7u16;
    let plot = Rect::new(
        inner.x + axis_w,
        inner.y + 1,
        inner.width - axis_w,
        inner.height - 2,
    );
    let buf = f.buffer_mut();
    for (frac, row) in [
        (1.0, plot.y),
        (0.5, plot.y + plot.height / 2),
        (0.0, plot.bottom() - 1),
    ] {
        let v = max * frac;
        let label = match m {
            BarMetric::Energy => format!("{v:.1}"),
            BarMetric::Cost => format!("{v:.2}"),
            BarMetric::Co2 if v >= 1000.0 => format!("{:.1}k", v / 1000.0),
            BarMetric::Co2 => format!("{v:.0}"),
        };
        buf.set_string(inner.x, row, format!("{label:>6}"), fg(theme::t().dim));
        buf.set_string(inner.x + 6, row, "┤", fg(theme::t().faint));
    }
    let n = bars.len() as u16;
    let gap = if plot.width / n >= 3 { 1 } else { 0 };
    let width = ((plot.width + gap) / n).saturating_sub(gap).max(1);
    // Color by price for variable tariffs, by size otherwise.
    let cts: Vec<f64> = bars.iter().filter_map(|b| b.ct_per_kwh).collect();
    let (lo, hi) = cts
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), c| (lo.min(*c), hi.max(*c)));
    let colors: Vec<Tone> = bars
        .iter()
        .zip(&values)
        .map(|(b, v)| match b.ct_per_kwh {
            Some(ct) if app.cfg.price.mode != PriceMode::Fixed && hi > lo => {
                theme::t().price.at((ct - lo) / (hi - lo))
            }
            _ => theme::t().power.at(if max > 0.0 { v / max } else { 0.0 }),
        })
        .collect();
    Bars {
        values: &values,
        colors: &colors,
        max,
        width,
        gap,
        highlight: Some(sel),
        gradient: theme::t().power,
    }
    .render(buf, plot);
    let every = 5u16.div_ceil(width + gap).max(1) as usize;
    for (i, bar) in bars.iter().enumerate() {
        let x = plot.x + i as u16 * (width + gap);
        hit(
            app,
            Rect::new(x, plot.y, width + gap, plot.height + 1),
            Hit::Bar(i),
        );
        if (i % every == 0 || i == sel) && x + 2 <= plot.right() {
            let st = if i == sel {
                bold(theme::t().accent)
            } else {
                fg(theme::t().dim)
            };
            buf.set_string(x, plot.bottom(), &bar.label, st);
        }
    }
}

fn draw_models(f: &mut Frame, app: &App, area: Rect) {
    let cur = &app.cfg.currency;
    let block = panel(
        &format!("local models · energy per token · {}", app.range.label()),
        theme::t().panel_models,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let header = Row::new(["Model", "tok/s", "tokens", "Wh/1k", "per 1M"])
        .style(fg(theme::t().dim).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .slow
        .models
        .iter()
        .map(|m| {
            let name = if m.model.is_empty() {
                format!("{} ({})", m.process, m.endpoint)
            } else {
                m.model.clone()
            };
            let dot = "·".to_string();
            Row::new(vec![
                Line::styled(name, fg(theme::t().text)),
                Line::styled(
                    m.now_tps.map(fmt_count).unwrap_or_default(),
                    fg(theme::t().text),
                )
                .alignment(Alignment::Right),
                Line::styled(fmt_count(m.gen_tokens as f64), fg(theme::t().muted))
                    .alignment(Alignment::Right),
                Line::styled(
                    m.wh_per_1k
                        .map(|v| format!("{v:.2}"))
                        .unwrap_or(dot.clone()),
                    fg(theme::t().text),
                )
                .alignment(Alignment::Right),
                Line::styled(
                    m.cost_per_1m.map(|v| fmt_money(v, cur)).unwrap_or(dot),
                    fg(theme::t().money),
                )
                .alignment(Alignment::Right),
            ])
        })
        .collect();
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Length(9),
    ];
    f.render_widget(
        Table::new(rows, widths).header(header).column_spacing(1),
        inner,
    );
}

fn draw_prices(f: &mut Frame, app: &App, area: Rect) {
    let Some(o) = &app.slow.prices else { return };
    let title = match app.cfg.price.mode {
        PriceMode::Dynamic => format!("price · {}/kWh", minor(app)),
        _ => format!("tariff · {}/kWh", minor(app)),
    };
    let mut right = Vec::new();
    if let Some((s, e, avg)) = o.cheapest {
        right.push(Span::styled(
            format!(" cheapest {} h ", o.window_hours),
            fg(theme::t().dim),
        ));
        right.push(Span::styled(
            format!(
                "{} → {} ",
                local(s).format("%a %H:%M"),
                local(e).format("%H:%M")
            ),
            bold(theme::t().good),
        ));
        right.push(Span::styled(
            format!("{avg:.1} {} ", minor(app)),
            fg(theme::t().good),
        ));
    }
    let block =
        panel(&title, theme::t().panel_prices).title(Line::from(right).alignment(Alignment::Right));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 20 || o.slots.is_empty() {
        return;
    }
    let values: Vec<f64> = o.slots.iter().map(|s| s.1).collect();
    let (lo, hi) = values
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    let axis_w = 5u16;
    let plot = Rect::new(
        inner.x + axis_w,
        inner.y,
        inner.width - axis_w,
        inner.height - 1,
    );
    let buf = f.buffer_mut();
    buf.set_string(inner.x, plot.y, format!("{hi:>4.0}"), fg(theme::t().dim));
    buf.set_string(
        inner.x,
        plot.bottom() - 1,
        format!("{:>4.0}", lo.min(hi)),
        fg(theme::t().dim),
    );
    let width = (plot.width / values.len() as u16).max(1);
    let shown = (plot.width / width) as usize;
    let skip = values.len().saturating_sub(shown);
    let floor = (lo - (hi - lo) * 0.25).max(0.0);
    let vals: Vec<f64> = values.iter().skip(skip).map(|v| v - floor).collect();
    let now_hour = o.now - o.now.rem_euclid(3600);
    let colors: Vec<Tone> = o
        .slots
        .iter()
        .skip(skip)
        .map(|(t, ct)| {
            let in_cheapest = o.cheapest.is_some_and(|(s, e, _)| *t >= s && *t < e);
            let c = theme::t()
                .price
                .at(if hi > lo { (ct - lo) / (hi - lo) } else { 0.0 });
            if *t < now_hour {
                c.lerp(theme::t().faint, 0.6)
            } else if in_cheapest {
                theme::t().good
            } else {
                c
            }
        })
        .collect();
    Bars {
        values: &vals,
        colors: &colors,
        max: (hi - floor).max(0.01),
        width,
        gap: 0,
        highlight: None,
        gradient: theme::t().price,
    }
    .render(buf, plot);
    let now_idx = o.slots.iter().skip(skip).position(|s| s.0 == now_hour);
    for (i, (t, _)) in o.slots.iter().skip(skip).enumerate() {
        let x = plot.x + i as u16 * width;
        if Some(i) == now_idx {
            buf.set_string(x, plot.bottom(), "now", bold(theme::t().text));
        } else if local(*t).format("%H").to_string() == "00"
            || local(*t).format("%H").to_string() == "12"
        {
            let near_now =
                now_idx.is_some_and(|n| (n as i64 - i as i64).abs() * (width as i64) < 6);
            if !near_now && x + 5 < plot.right() {
                buf.set_string(
                    x,
                    plot.bottom(),
                    local(*t).format("%H:%M").to_string(),
                    fg(theme::t().dim),
                );
            }
        }
    }
}

// ------------------------------------------------------------------ totals and footer

fn draw_totals(f: &mut Frame, app: &App, area: Rect) {
    let since = app
        .slow
        .first_data
        .map(|t| format!(" since {} ", local(t).format("%Y-%m-%d")))
        .unwrap_or_default();
    let block = panel("totals", theme::t().panel_totals)
        .title(Line::styled(since, fg(theme::t().dim)).alignment(Alignment::Right));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let cards = &app.slow.cards;
    if cards.is_empty() {
        return;
    }
    let cur = &app.cfg.currency;
    let n = cards.len() as u32;
    let cols = Layout::horizontal(vec![Constraint::Ratio(1, n); n as usize]).split(inner);
    let faint = theme::t().faint.lerp(theme::t().dim, 0.6);
    for (i, (c, rect)) in cards.iter().zip(cols.iter()).enumerate() {
        let projected = c.label.starts_with("Projected");
        let mut l1 = vec![Span::styled(c.label.to_string(), fg(theme::t().dim))];
        if let Some(t) = c.trend() {
            let (arrow, color) = if t > 0.02 {
                ("▲", theme::t().bad)
            } else if t < -0.02 {
                ("▼", theme::t().good)
            } else {
                ("▶", theme::t().dim)
            };
            l1.push(Span::styled(
                format!("  {arrow} {:.0}%", t.abs() * 100.0),
                fg(color),
            ));
        }
        let money = if projected {
            fg(theme::t().money).add_modifier(Modifier::ITALIC)
        } else {
            bold(theme::t().money)
        };
        let mark = if c.totals.price_fallback { "*" } else { "" };
        let l2 = Line::from(vec![
            Span::styled(format!("{}{mark}", fmt_money(c.totals.cost, cur)), money),
            Span::styled(
                format!("  {}", fmt_kwh(c.totals.kwh())),
                fg(theme::t().text),
            ),
        ]);
        let mut l3 = vec![Span::styled(fmt_co2(c.totals.co2_g), fg(theme::t().co2))];
        if c.previous.is_some() {
            l3.push(Span::styled(
                format!("  vs {}", c.previous_label),
                fg(faint),
            ));
        } else if projected {
            l3.push(Span::styled("  at the 7-day pace", fg(faint)));
        }
        let r = Rect {
            x: rect.x + 1,
            width: rect.width.saturating_sub(2),
            ..*rect
        };
        f.render_widget(Paragraph::new(vec![Line::from(l1), l2, Line::from(l3)]), r);
        if i + 1 < cards.len() {
            for y in rect.y..rect.bottom() {
                f.buffer_mut()
                    .set_string(rect.right() - 1, y, "│", fg(theme::t().faint));
            }
        }
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let key = |k: &'static str, what: &'static str| {
        vec![
            Span::styled(
                format!(" {k} "),
                Style::new()
                    .fg(theme::t().text.color())
                    .bg(theme::t().faint.color()),
            ),
            Span::styled(format!(" {what} "), fg(theme::t().dim)),
        ]
    };
    let mut spans = vec![];
    for (k, w) in [
        ("1-6", "range"),
        ("↑↓", "select"),
        ("⏎", "details"),
        ("g", "graph"),
        ("u", "unit"),
        ("s", "settings"),
        ("?", "help"),
        ("q", "quit"),
    ] {
        spans.extend(key(k, w));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let message = app
        .status
        .as_ref()
        .filter(|(_, at)| at.elapsed().as_secs() < 5)
        .map(|(m, _)| m.clone())
        .or_else(|| app.market_status.clone())
        .or_else(|| app.fast.notes.first().cloned());
    if let Some(m) = message {
        let p = Paragraph::new(Span::styled(format!("{m} "), fg(theme::t().warn)))
            .alignment(Alignment::Right);
        f.render_widget(p, area);
    }
}

// ------------------------------------------------------------------ popups

pub fn popup(f: &mut Frame, width: u16, height: u16, title: &str, color: Tone) -> Rect {
    let full = f.area();
    let area = full.centered(
        Constraint::Length(width.min(full.width)),
        Constraint::Length(height.min(full.height)),
    );
    f.render_widget(Clear, area);
    let block = panel(title, color).style(Style::new().bg(theme::t().popup_bg.color()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

fn draw_detail(f: &mut Frame, app: &App) {
    let Some(d) = &app.detail else { return };
    let inner = popup(
        f,
        100,
        22,
        &format!("{} · last {}", d.name, app.range.label()),
        theme::t().panel_procs,
    );
    let cur = &app.cfg.currency;
    let mut lines = Vec::new();
    if let Some(p) = &d.row {
        let mut l = vec![tag(p), Span::raw("  ")];
        if let Some(n) = p.now_w {
            l.push(Span::styled(
                format!("now {}  ", fmt_watts(n)),
                bold(theme::t().text),
            ));
        }
        l.extend([
            Span::styled(
                format!("avg {}  ", fmt_watts(p.avg_w)),
                fg(theme::t().muted),
            ),
            Span::styled(format!("{}  ", fmt_kwh(p.kwh)), fg(theme::t().text)),
            Span::styled(
                format!("{}  ", fmt_money(p.cost, cur)),
                bold(theme::t().money),
            ),
            Span::styled(
                format!("{:.0}% of all process energy", p.share * 100.0),
                fg(theme::t().dim),
            ),
        ]);
        lines.push(Line::from(l));
        if let Some(g) = p.gpu_share {
            let txt = format!(
                "{:.0}% on GPUs, {:.0}% on the CPU",
                g * 100.0,
                (1.0 - g) * 100.0
            );
            lines.push(Line::styled(txt, fg(theme::t().dim)));
        }
    }
    f.render_widget(Paragraph::new(lines), Rect { height: 2, ..inner });
    let graph = Rect {
        y: inner.y + 3,
        height: inner.height.saturating_sub(4),
        ..inner
    };
    if d.points.is_empty() || graph.height < 3 {
        let msg = "No minute data for this process in this range yet.";
        f.render_widget(Paragraph::new(msg).style(fg(theme::t().dim)), graph);
        return;
    }
    let max = nice_max(d.points.iter().map(|p| p.1).fold(0.0, f64::max) * 1.08);
    let axis_w = 6;
    let plot = Rect::new(
        graph.x + axis_w,
        graph.y,
        graph.width - axis_w,
        graph.height - 1,
    );
    let span = app.range.secs() as f64;
    let pts: Vec<(f64, f64)> = d
        .points
        .iter()
        .map(|(t, w)| ((*t - app.fast.now) as f64, *w))
        .collect();
    let cols: Vec<Option<Column>> = widgets::resample(&pts, -span, 0.0, plot.width as usize * 2)
        .into_iter()
        .map(|v| v.map(Column::single))
        .collect();
    let buf = f.buffer_mut();
    buf.set_string(
        graph.x,
        plot.y,
        format!("{:>5}", fmt_axis_w(max)),
        fg(theme::t().dim),
    );
    buf.set_string(
        graph.x,
        plot.bottom() - 1,
        format!("{:>5}", "0"),
        fg(theme::t().dim),
    );
    widgets::area(buf, plot, &cols, max, theme::t().power);
    let start = time_label(app.range, app.fast.now - app.range.secs(), app.fast.now);
    buf.set_string(plot.x, plot.bottom(), start, fg(theme::t().dim));
    buf.set_string(
        plot.right().saturating_sub(4),
        plot.bottom(),
        "now",
        fg(theme::t().dim),
    );
    buf.set_string(
        inner.right().saturating_sub(13),
        inner.y,
        "Esc to close",
        fg(theme::t().dim),
    );
}

fn draw_settings(f: &mut Frame, app: &App) {
    let Some(form) = &app.settings else { return };
    let inner = popup(
        f,
        82,
        Field::ALL.len() as u16 + 16,
        "settings",
        theme::t().accent,
    );
    let mut lines: Vec<Line> = Vec::new();
    let mut last_section = "";
    for (i, field) in Field::ALL.iter().enumerate() {
        if field.section() != last_section {
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(field.section(), bold(theme::t().accent)));
            last_section = field.section();
        }
        let selected = i == form.selected;
        let active = field.active(&app.cfg);
        let value = match (&form.editing, selected) {
            (Some(buf), true) => format!("{buf}▏"),
            _ if field.is_choice() => format!("‹ {} ›", field.get(&app.cfg)),
            _ => field.get(&app.cfg),
        };
        let base = if active {
            fg(theme::t().text)
        } else {
            fg(theme::t().dim)
        };
        let style = if selected {
            base.bg(theme::t().select_bg.color()).bold()
        } else {
            base
        };
        let value_style = if active {
            style.fg(theme::t().money.color())
        } else {
            style
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { " ▶ " } else { "   " },
                style.fg(theme::t().accent.color()),
            ),
            Span::styled(format!("{:<32}", field.label()), style),
            Span::styled(format!("{value:<26}"), value_style),
        ]));
    }
    lines.push(Line::raw(""));
    match &form.error {
        Some(e) => lines.push(Line::styled(format!("  {e}"), fg(theme::t().bad))),
        None => lines.push(Line::styled(
            format!("  {}", form.field().help()),
            fg(theme::t().dim),
        )),
    }
    let keys = "  ↑↓ select · ⏎ edit · ←→ change · S run setup again · Esc save and close";
    lines.push(Line::styled(
        keys,
        fg(theme::t().faint.lerp(theme::t().dim, 0.7)),
    ));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_help(f: &mut Frame) {
    let inner = popup(f, 84, 24, "help", theme::t().accent);
    let h = |s: &str| Line::styled(s.to_string(), bold(theme::t().accent));
    let t = |s: &str| Line::styled(s.to_string(), fg(theme::t().text));
    let text = vec![
        h("What you see"),
        t("CPU and GPUs are measured (RAPL or socket power, NVML, amdgpu). Base load and"),
        t("power supply loss are estimates from the settings. Total = measured + estimates."),
        t("Processes share CPU energy by CPU time and GPU energy by GPU utilization."),
        t("Local models: energy of the serving process divided by the tokens it generated."),
        t("Costs use your tariff per 15 minutes; * marks totals priced with the fallback."),
        Line::raw(""),
        h("Keys"),
        t("1 to 6     5 min, 1 hour, 24 hours, 7 days, 30 days, 1 year (or click)"),
        t("← →        zoom out and in"),
        t("↑ ↓        select a process        ⏎   history of the selected process"),
        t("e n c      sort processes by energy, now, cost"),
        t("[ ]        select a bar            u   kWh, money or CO2 in the bars"),
        t("g          total or stacked graph"),
        t("s          settings                S   run the setup again"),
        t("q          quit"),
        Line::raw(""),
        Line::styled("any key closes this", fg(theme::t().dim)),
    ];
    f.render_widget(Paragraph::new(text), inner);
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
}
