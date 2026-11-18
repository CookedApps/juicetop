//! First-run setup in four steps: country, tariff, electricity mix, and the computer.
//! Every step starts on a sensible default, so pressing Enter four times is a valid setup.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::App;
use super::settings::Field;
use super::theme::{self, Rgb, bold, fg};
use super::widgets;
use crate::config::{Config, PriceMode};
use crate::dashboard::Fast;
use crate::presets::{self, COUNTRIES, Country};
use crate::report::{fmt_money, fmt_watts};

pub const STEPS: usize = 4;
const GREEN_G: f64 = 30.0;
const PCS: [(&str, &str, f64); 4] = [
    ("Desktop", "a typical tower", 40.0),
    ("Workstation", "many drives, big board", 85.0),
    ("Mini PC / laptop", "small and frugal", 15.0),
    ("Custom", "set your own", 0.0),
];
const PSUS: [(&str, f64); 5] = [
    ("80 PLUS", 82.0),
    ("Bronze", 85.0),
    ("Gold", 90.0),
    ("Platinum", 92.0),
    ("Titanium", 94.0),
];

pub enum Outcome {
    Continue,
    Done(Box<Config>),
}

pub struct Onboarding {
    pub step: usize,
    row: usize,
    editing: Option<String>,
    error: Option<String>,
    pub draft: Config,
    country: usize,
    detected: usize,
    mix: usize,
    pc: usize,
    psu: usize,
    hardware: Vec<String>,
    collector: bool,
    live_w: Option<f64>,
}

impl Onboarding {
    pub fn new(cfg: &Config, fast: &Fast, collector: bool) -> Self {
        let detected = presets::by_code(&cfg.country).unwrap_or_else(presets::detect);
        let detected = COUNTRIES
            .iter()
            .position(|c| c.code == detected.code)
            .unwrap_or(COUNTRIES.len() - 1);
        let mut draft = cfg.clone();
        if cfg.country.is_empty() {
            presets::apply(&mut draft, &COUNTRIES[detected]);
        }
        let hardware = if fast.sources.is_empty() {
            let nvml = nvml_wrapper::Nvml::init().ok();
            crate::sensors::probe(nvml.as_ref())
                .0
                .iter()
                .map(|s| s.info().label.clone())
                .collect()
        } else {
            fast.sources.iter().map(|s| s.name.clone()).collect()
        };
        let pc = PCS
            .iter()
            .position(|p| p.2 == cfg.estimate.base_watts)
            .unwrap_or(0);
        let psu = PSUS
            .iter()
            .position(|p| p.1 == cfg.estimate.psu_efficiency_percent)
            .unwrap_or(2);
        let mix = if draft.co2.g_per_kwh == presets::COUNTRIES[detected].co2_g {
            0
        } else if draft.co2.g_per_kwh == GREEN_G {
            1
        } else if cfg.country.is_empty() {
            0
        } else {
            2
        };
        Self {
            step: 0,
            row: detected,
            editing: None,
            error: None,
            draft,
            country: detected,
            detected,
            mix,
            pc,
            psu,
            hardware,
            collector,
            live_w: fast.now_wall_w(),
        }
    }

    fn country(&self) -> &'static Country {
        &COUNTRIES[self.country]
    }

    /// Selects a country by code (also used by tests).
    pub fn select_country(&mut self, code: &str) {
        if let Some(i) = COUNTRIES.iter().position(|c| c.code == code) {
            self.country = i;
            self.row = i;
            self.mix = 0;
            presets::apply(&mut self.draft, &COUNTRIES[i]);
        }
    }

    fn modes(&self) -> Vec<PriceMode> {
        let mut m = vec![PriceMode::Fixed, PriceMode::TimeOfUse];
        if self.country().market.is_some() {
            m.push(PriceMode::Dynamic);
        }
        m
    }

    /// Editable fields below the choice on the current step.
    fn fields(&self) -> Vec<Field> {
        match self.step {
            1 => match self.draft.price.mode {
                PriceMode::Fixed => vec![Field::Fixed],
                PriceMode::TimeOfUse => vec![
                    Field::TouHigh,
                    Field::TouLow,
                    Field::TouStart,
                    Field::TouEnd,
                ],
                PriceMode::Dynamic => vec![Field::DynMarkup, Field::DynVat],
            },
            2 if self.mix == 2 => vec![Field::Co2],
            3 if self.pc == 3 => vec![Field::BaseWatts],
            _ => Vec::new(),
        }
    }

    /// Rows on steps 1 to 3: the choice row, (on step 3) the PSU row, then fields.
    fn rows(&self) -> usize {
        match self.step {
            0 => COUNTRIES.len(),
            3 => 2 + self.fields().len(),
            _ => 1 + self.fields().len(),
        }
    }

    fn field_at(&self, row: usize) -> Option<Field> {
        let first = if self.step == 3 { 2 } else { 1 };
        row.checked_sub(first)
            .and_then(|i| self.fields().get(i).copied())
    }

    fn choose(&mut self, forward: bool) {
        let step = |i: usize, n: usize| {
            if forward {
                (i + 1).min(n - 1)
            } else {
                i.saturating_sub(1)
            }
        };
        match (self.step, self.row) {
            (1, 0) => {
                let modes = self.modes();
                let i = modes
                    .iter()
                    .position(|m| *m == self.draft.price.mode)
                    .unwrap_or(0);
                self.draft.price.mode = modes[step(i, modes.len())];
            }
            (2, 0) => {
                self.mix = step(self.mix, 3);
                self.draft.co2.g_per_kwh = match self.mix {
                    0 => self.country().co2_g,
                    1 => GREEN_G,
                    _ => self.draft.co2.g_per_kwh,
                };
            }
            (3, 0) => {
                self.pc = step(self.pc, PCS.len());
                if self.pc < 3 {
                    self.draft.estimate.base_watts = PCS[self.pc].2;
                }
            }
            (3, 1) => {
                self.psu = step(self.psu, PSUS.len());
                self.draft.estimate.psu_efficiency_percent = PSUS[self.psu].1;
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        if let Some(buf) = self.editing.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.editing = None;
                    self.error = None;
                }
                KeyCode::Enter => {
                    let input = buf.clone();
                    if let Some(f) = self.field_at(self.row) {
                        match f.set(&mut self.draft, &input) {
                            Ok(()) => {
                                self.editing = None;
                                self.error = None;
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return Outcome::Continue;
        }
        match key.code {
            KeyCode::Enter => {
                if self.step + 1 == STEPS {
                    return Outcome::Done(Box::new(self.draft.clone()));
                }
                if self.step == 0
                    && self.country().market.is_none()
                    && self.draft.price.mode == PriceMode::Dynamic
                {
                    self.draft.price.mode = PriceMode::Fixed;
                }
                self.step += 1;
                self.row = 0;
            }
            KeyCode::Esc => {
                if self.step == 0 {
                    return Outcome::Done(Box::new(self.draft.clone()));
                }
                self.step -= 1;
                self.row = if self.step == 0 { self.country } else { 0 };
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.row = self.row.saturating_sub(1);
                if self.step == 0 {
                    self.select_country(COUNTRIES[self.row].code);
                }
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.row = (self.row + 1).min(self.rows() - 1);
                if self.step == 0 {
                    self.select_country(COUNTRIES[self.row].code);
                }
            }
            KeyCode::Left => self.choose(false),
            KeyCode::Right => self.choose(true),
            KeyCode::Backspace => {
                if let Some(f) = self.field_at(self.row) {
                    let mut v = f.get(&self.draft);
                    v.pop();
                    self.editing = Some(v);
                }
            }
            KeyCode::Char(c)
                if (c.is_ascii_digit() || matches!(c, '.' | ',' | ':' | '-'))
                    && self.field_at(self.row).is_some() =>
            {
                self.editing = Some(c.to_string());
            }
            _ => {}
        }
        Outcome::Continue
    }
}

// ------------------------------------------------------------------ drawing

/// A two-row price curve; each level is 0 to 16 eighths of the two rows.
fn curve(levels: &[u8]) -> Vec<Line<'static>> {
    const PARTS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let row = |offset: u8| -> Line<'static> {
        Line::from(
            levels
                .iter()
                .map(|l| {
                    let part = l.saturating_sub(offset).min(8) as usize;
                    Span::styled(
                        PARTS[part].to_string(),
                        fg(theme::PRICE.at(*l as f64 / 16.0)),
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    vec![row(8), row(0)]
}

struct Card {
    title: String,
    lines: Vec<Line<'static>>,
    enabled: bool,
}

fn draw_cards(f: &mut Frame, area: Rect, cards: &[Card], selected: usize, focused: bool) {
    let n = cards.len() as u32;
    let cols = Layout::horizontal(vec![Constraint::Ratio(1, n); n as usize])
        .spacing(1)
        .split(area);
    for (i, (c, rect)) in cards.iter().zip(cols.iter()).enumerate() {
        let sel = i == selected;
        let color = if !c.enabled {
            theme::FAINT
        } else if sel {
            theme::BRAND
        } else {
            theme::DIM
        };
        let border = if sel && focused {
            BorderType::Thick
        } else {
            BorderType::Rounded
        };
        let title_style = if sel {
            bold(color)
        } else {
            fg(if c.enabled { theme::MUTED } else { theme::DIM })
        };
        let mark = if sel { "● " } else { "○ " };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(border)
            .border_style(fg(color))
            .title(Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{mark}{}", c.title), title_style),
                Span::raw(" "),
            ]));
        let inner = block.inner(*rect);
        f.render_widget(block, *rect);
        f.render_widget(
            Paragraph::new(c.lines.clone()),
            Rect {
                x: inner.x + 1,
                width: inner.width.saturating_sub(1),
                ..inner
            },
        );
    }
}

fn field_row(ob: &Onboarding, row: usize, label: &str, unit: &str, field: Field) -> Line<'static> {
    let selected = ob.row == row;
    let value = match (&ob.editing, selected) {
        (Some(buf), true) => format!("{buf}▏"),
        _ => field.get(&ob.draft),
    };
    let st = if selected {
        Style::new().bg(theme::SELECT_BG.color())
    } else {
        Style::new()
    };
    Line::from(vec![
        Span::styled(
            if selected { " ▶ " } else { "   " },
            st.fg(theme::BRAND.color()),
        ),
        Span::styled(format!("{label:<28}"), st.fg(theme::TEXT.color())),
        Span::styled(
            format!(" {value:>8} "),
            st.fg(theme::MONEY.color()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {unit}"), st.fg(theme::DIM.color())),
    ])
}

pub fn draw(f: &mut Frame, ob: &Onboarding, _app: &App) {
    let full = f.area();
    let area = full.centered(
        Constraint::Length(full.width.min(100)),
        Constraint::Length(full.height.min(28)),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(theme::BRAND.lerp(theme::FAINT, 0.3)))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("setup", bold(theme::BRAND)),
            Span::raw(" "),
        ]))
        .title(
            Line::styled(format!(" step {} of {STEPS} ", ob.step + 1), fg(theme::DIM))
                .alignment(Alignment::Right),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let inner = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(4),
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
    };

    // Progress: ●━━●━━◉━━○
    let names = ["Country", "Tariff", "Mix", "Computer"];
    let mut prog = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let (dot, c) = match i.cmp(&ob.step) {
            std::cmp::Ordering::Less => ("●", theme::GOOD),
            std::cmp::Ordering::Equal => ("◉", theme::BRAND),
            std::cmp::Ordering::Greater => ("○", theme::DIM),
        };
        prog.push(Span::styled(
            format!("{dot} {name}"),
            if i == ob.step { bold(c) } else { fg(c) },
        ));
        if i + 1 < names.len() {
            prog.push(Span::styled(
                "  ━━━  ",
                fg(if i < ob.step {
                    theme::GOOD
                } else {
                    theme::FAINT
                }),
            ));
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(prog)).alignment(Alignment::Center),
        Rect { height: 1, ..inner },
    );
    let body = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(4),
        ..inner
    };
    match ob.step {
        0 => draw_welcome(f, ob, body),
        1 => draw_tariff(f, ob, body),
        2 => draw_mix(f, ob, body),
        _ => draw_computer(f, ob, body),
    }

    let hint = match (ob.editing.is_some(), ob.step) {
        (true, _) => "type a value · ⏎ confirm · esc cancel",
        (false, 0) => "↑↓ choose · ⏎ continue · esc skip setup and use the defaults",
        (false, 3) => "←→ choose · ↑↓ move · type to edit · ⏎ save and start · esc back",
        _ => "←→ choose · ↑↓ move · type to edit · ⏎ continue · esc back",
    };
    let mut footer = vec![Span::styled(hint, fg(theme::DIM))];
    if let Some(e) = &ob.error {
        footer = vec![Span::styled(e.clone(), fg(theme::BAD))];
    }
    f.render_widget(
        Paragraph::new(Line::from(footer)).alignment(Alignment::Center),
        Rect {
            y: inner.bottom().saturating_sub(1),
            height: 1,
            ..inner
        },
    );
}

fn heading(title: &str, sub: &str) -> Vec<Line<'static>> {
    vec![
        Line::styled(title.to_string(), bold(theme::TEXT)),
        Line::styled(sub.to_string(), fg(theme::DIM)),
        Line::raw(""),
    ]
}

fn draw_welcome(f: &mut Frame, ob: &Onboarding, area: Rect) {
    let mut lines = vec![];
    let mut logo = vec![Span::styled("Welcome to ", bold(theme::TEXT))];
    logo.extend(widgets::gradient_text(
        "⚡juicetop",
        theme::BRAND_GRADIENT,
        Style::new().add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::from(logo));
    lines.push(Line::styled(
        "Four quick questions so the costs are yours, not an average. Every answer has a default.",
        fg(theme::DIM),
    ));
    lines.push(Line::raw(""));
    lines.push(Line::styled("Found on this machine", bold(theme::MUTED)));
    if ob.hardware.is_empty() {
        lines.push(Line::styled(
            "  ! no power sensors found; run `juicetop sensors` for details",
            fg(theme::WARN),
        ));
    }
    for h in &ob.hardware {
        lines.push(Line::from(vec![
            Span::styled("  ✓ ", fg(theme::GOOD)),
            Span::styled(h.clone(), fg(theme::TEXT)),
        ]));
    }
    if ob.collector {
        lines.push(Line::from(vec![
            Span::styled("  ✓ ", fg(theme::GOOD)),
            Span::styled("collector is running", fg(theme::TEXT)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  ! ", fg(theme::WARN)),
            Span::styled(
                "collector is not running. Start it later with ",
                fg(theme::TEXT),
            ),
            Span::styled("sudo juicetop service install", bold(theme::BRAND)),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Where do you live?", bold(theme::TEXT)));
    lines.push(Line::styled(
        "Sets currency, a typical price and the grid's CO2 intensity. You can adjust them next.",
        fg(theme::DIM),
    ));
    let top = lines.len() as u16;
    f.render_widget(Paragraph::new(lines), area);

    let list = Rect {
        y: area.y + top + 1,
        height: area.height.saturating_sub(top + 1),
        ..area
    };
    let visible = list.height as usize;
    let offset = ob
        .row
        .saturating_sub(visible / 2)
        .min(COUNTRIES.len().saturating_sub(visible));
    let rows: Vec<Line> = COUNTRIES
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .map(|(i, c)| {
            let sel = i == ob.row;
            let st = if sel {
                Style::new().bg(theme::SELECT_BG.color())
            } else {
                Style::new()
            };
            let mut spans = vec![
                Span::styled(if sel { " ▶ " } else { "   " }, st.fg(theme::BRAND.color())),
                Span::styled(
                    format!("{:<18}", c.name),
                    if sel {
                        st.fg(theme::TEXT.color()).add_modifier(Modifier::BOLD)
                    } else {
                        st.fg(theme::MUTED.color())
                    },
                ),
            ];
            if c.code != "XX" {
                spans.push(Span::styled(
                    format!("{:>4}  ", c.currency),
                    st.fg(theme::DIM.color()),
                ));
                spans.push(Span::styled(
                    format!("{:>5.0} {}/kWh  ", c.price, c.minor),
                    st.fg(theme::MONEY.color()),
                ));
                spans.push(Span::styled(
                    format!("{:>4.0} g CO2/kWh", c.co2_g),
                    st.fg(theme::CO2.color()),
                ));
                if c.market.is_some() {
                    spans.push(Span::styled("  dynamic tariffs", st.fg(theme::AI.color())));
                }
            }
            if i == ob.detected {
                spans.push(Span::styled("  detected", st.fg(theme::GOOD.color())));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(rows), list);
}

fn draw_tariff(f: &mut Frame, ob: &Onboarding, area: Rect) {
    let c = ob.country();
    let lines = heading(
        "How do you pay for electricity?",
        "Most households pay one fixed price. Check your bill if you're unsure.",
    );
    f.render_widget(Paragraph::new(lines), area);
    let cards_area = Rect {
        y: area.y + 3,
        height: 5,
        ..area
    };
    let dynamic = c.market.is_some();
    let cards = vec![
        Card {
            title: "Fixed price".into(),
            lines: [
                curve(&[7; 24]),
                vec![Line::styled("same price all day", fg(theme::MUTED))],
            ]
            .concat(),
            enabled: true,
        },
        Card {
            title: "Day / night".into(),
            lines: [
                curve(&[
                    4, 4, 4, 4, 4, 4, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11,
                    11, 4, 4,
                ]),
                vec![Line::styled("cheaper at night (HT/NT)", fg(theme::MUTED))],
            ]
            .concat(),
            enabled: true,
        },
        Card {
            title: "Dynamic".into(),
            lines: [
                curve(&[
                    5, 3, 3, 2, 3, 5, 9, 11, 9, 7, 6, 5, 5, 6, 8, 11, 14, 16, 15, 13, 10, 8, 7, 5,
                ]),
                vec![Line::styled(
                    if dynamic {
                        "hourly exchange price".to_string()
                    } else {
                        format!("not available in {}", c.name)
                    },
                    fg(theme::MUTED),
                )],
            ]
            .concat(),
            enabled: dynamic,
        },
    ];
    let sel = match ob.draft.price.mode {
        PriceMode::Fixed => 0,
        PriceMode::TimeOfUse => 1,
        PriceMode::Dynamic => 2,
    };
    draw_cards(f, cards_area, &cards, sel, ob.row == 0);

    let unit = format!("{}/kWh", c.minor);
    let mut rows = Vec::new();
    for (i, field) in ob.fields().iter().enumerate() {
        let (label, u) = match field {
            Field::Fixed => ("Price per kWh, incl. VAT", unit.as_str()),
            Field::TouHigh => ("Day price (high tariff)", unit.as_str()),
            Field::TouLow => ("Night price (low tariff)", unit.as_str()),
            Field::TouStart => ("Night starts at", "HH:MM"),
            Field::TouEnd => ("Night ends at", "HH:MM"),
            Field::DynMarkup => ("Fees and margin, before VAT", unit.as_str()),
            Field::DynVat => ("VAT", "%"),
            _ => ("", ""),
        };
        rows.push(field_row(ob, i + 1, label, u, *field));
    }
    rows.push(Line::raw(""));
    let hint = match ob.draft.price.mode {
        PriceMode::Fixed => format!("Prefilled with the {} average. Your bill calls it the energy or unit rate.", c.name),
        PriceMode::TimeOfUse => "Two prices and the hours of the cheaper one, as on a dual-rate meter.".to_string(),
        PriceMode::Dynamic => "The exchange price comes from aWATTar every hour; add what your supplier charges on top.".to_string(),
    };
    rows.push(Line::styled(hint, fg(theme::DIM)));
    f.render_widget(
        Paragraph::new(rows),
        Rect {
            y: cards_area.bottom() + 1,
            height: area.bottom().saturating_sub(cards_area.bottom() + 1),
            ..area
        },
    );
}

fn co2_gauge(g: f64, width: usize) -> Vec<Span<'static>> {
    let mut v = widgets::meter(g / 700.0, width, theme::PRICE);
    v.push(Span::styled(
        format!("  {g:.0} g CO2 per kWh"),
        bold(theme::CO2),
    ));
    v
}

fn draw_mix(f: &mut Frame, ob: &Onboarding, area: Rect) {
    let c = ob.country();
    let lines = heading(
        "What is in your electricity?",
        "Used for the CO2 numbers. Your supplier lists its mix on the bill or website.",
    );
    f.render_widget(Paragraph::new(lines), area);
    let cards_area = Rect {
        y: area.y + 3,
        height: 6,
        ..area
    };
    let cards = vec![
        Card {
            title: "Grid average".into(),
            lines: vec![
                Line::styled(format!("{:.0} g CO2/kWh", c.co2_g), bold(theme::CO2)),
                Line::styled(format!("typical for {}", c.name), fg(theme::MUTED)),
            ],
            enabled: true,
        },
        Card {
            title: "Green tariff".into(),
            lines: vec![
                Line::styled(format!("≈ {GREEN_G:.0} g CO2/kWh"), bold(theme::CO2)),
                Line::styled("renewable electricity", fg(theme::MUTED)),
            ],
            enabled: true,
        },
        Card {
            title: "Custom".into(),
            lines: vec![
                Line::styled("your supplier's value", bold(theme::CO2)),
                Line::styled("from the fuel mix label", fg(theme::MUTED)),
            ],
            enabled: true,
        },
    ];
    draw_cards(f, cards_area, &cards, ob.mix, ob.row == 0);
    let mut rows = vec![
        Line::from(co2_gauge(ob.draft.co2.g_per_kwh, 30)),
        Line::raw(""),
    ];
    if ob.mix == 2 {
        rows.push(field_row(ob, 1, "Grams of CO2 per kWh", "g", Field::Co2));
    }
    f.render_widget(
        Paragraph::new(rows),
        Rect {
            y: cards_area.bottom() + 1,
            height: area.bottom().saturating_sub(cards_area.bottom() + 1),
            ..area
        },
    );
}

fn draw_computer(f: &mut Frame, ob: &Onboarding, area: Rect) {
    let lines = heading(
        "What kind of computer is this?",
        "Mainboard, RAM, drives and fans have no power sensor. This sets their estimated draw.",
    );
    f.render_widget(Paragraph::new(lines), area);
    let cards_area = Rect {
        y: area.y + 3,
        height: 5,
        ..area
    };
    let cards: Vec<Card> = PCS
        .iter()
        .map(|(name, desc, w)| Card {
            title: (*name).into(),
            lines: vec![
                Line::styled(
                    if *w > 0.0 {
                        format!("+{w:.0} W")
                    } else {
                        format!("{:.0} W", ob.draft.estimate.base_watts)
                    },
                    bold(theme::ESTIMATE),
                ),
                Line::styled((*desc).to_string(), fg(theme::MUTED)),
            ],
            enabled: true,
        })
        .collect();
    draw_cards(f, cards_area, &cards, ob.pc, ob.row == 0);

    let psu_sel = ob.row == 1;
    let st = if psu_sel {
        Style::new().bg(theme::SELECT_BG.color())
    } else {
        Style::new()
    };
    let mut psu = vec![
        Span::styled(
            if psu_sel { " ▶ " } else { "   " },
            st.fg(theme::BRAND.color()),
        ),
        Span::styled(
            format!("{:<28}", "Power supply rating"),
            st.fg(theme::TEXT.color()),
        ),
    ];
    for (i, (name, eff)) in PSUS.iter().enumerate() {
        let s = if i == ob.psu {
            st.fg(theme::MONEY.color()).add_modifier(Modifier::BOLD)
        } else {
            st.fg(theme::DIM.color())
        };
        psu.push(Span::styled(
            if i == ob.psu {
                format!(" ‹ {name} {eff:.0}% › ")
            } else {
                format!(" {name} ")
            },
            s,
        ));
    }
    let mut rows = vec![Line::from(psu)];
    if ob.pc == 3 {
        rows.push(field_row(
            ob,
            2,
            "Base load without a sensor",
            "W",
            Field::BaseWatts,
        ));
    }
    rows.push(Line::raw(""));

    // Summary.
    let d = &ob.draft;
    let c = ob.country();
    let price = match d.price.mode {
        PriceMode::Fixed => format!("{:.1} {}/kWh fixed", d.price.fixed_ct_per_kwh, c.minor),
        PriceMode::TimeOfUse => format!(
            "{:.1} / {:.1} {}/kWh, night {} to {}",
            d.price.time_of_use.high_ct_per_kwh,
            d.price.time_of_use.low_ct_per_kwh,
            c.minor,
            d.price.time_of_use.low_start,
            d.price.time_of_use.low_end
        ),
        PriceMode::Dynamic => format!(
            "dynamic ({}), +{:.1} {} and {:.0}% VAT",
            d.price.dynamic.region.to_uppercase(),
            d.price.dynamic.markup_ct_per_kwh,
            c.minor,
            d.price.dynamic.vat_percent
        ),
    };
    rows.push(Line::styled("Your setup", bold(theme::MUTED)));
    let item = |k: &str, v: String, col: Rgb| {
        Line::from(vec![
            Span::styled(format!("  {k:<14}"), fg(theme::DIM)),
            Span::styled(v, fg(col)),
        ])
    };
    rows.push(item(
        "Country",
        format!("{} ({})", c.name, d.currency),
        theme::TEXT,
    ));
    rows.push(item("Price", price, theme::MONEY));
    rows.push(item(
        "CO2",
        format!("{:.0} g per kWh", d.co2.g_per_kwh),
        theme::CO2,
    ));
    rows.push(item(
        "Estimates",
        format!(
            "{:.0} W base load, {:.0}% efficient power supply",
            d.estimate.base_watts, d.estimate.psu_efficiency_percent
        ),
        theme::ESTIMATE,
    ));
    if let Some(w) = ob.live_w {
        let ct = match d.price.mode {
            PriceMode::Fixed | PriceMode::Dynamic => d.price.fixed_ct_per_kwh,
            PriceMode::TimeOfUse => {
                (d.price.time_of_use.high_ct_per_kwh * 2.0 + d.price.time_of_use.low_ct_per_kwh)
                    / 3.0
            }
        };
        let day = w * 24.0 / 1000.0 * ct / 100.0;
        rows.push(Line::raw(""));
        rows.push(Line::from(vec![
            Span::styled("  At the current draw of ", fg(theme::DIM)),
            Span::styled(fmt_watts(w), bold(theme::TEXT)),
            Span::styled(" this machine costs about ", fg(theme::DIM)),
            Span::styled(fmt_money(day, &d.currency), bold(theme::MONEY)),
            Span::styled(" a day, ", fg(theme::DIM)),
            Span::styled(fmt_money(day * 30.44, &d.currency), bold(theme::MONEY)),
            Span::styled(" a month.", fg(theme::DIM)),
        ]));
    }
    f.render_widget(
        Paragraph::new(rows),
        Rect {
            y: cards_area.bottom() + 1,
            height: area.bottom().saturating_sub(cards_area.bottom() + 1),
            ..area
        },
    );
}
