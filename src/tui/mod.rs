//! The dashboard: event loop and app state. Drawing lives in `ui` and `onboarding`.

mod onboarding;
mod settings;
mod theme;
mod ui;
mod widgets;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};

use crate::awattar;
use crate::config::{Config, PriceMode};
use crate::dashboard::{self, AllTimeCache, BarMetric, Fast, ProcRow, Range, Slow};
use crate::live::{DataPaths, DeviceStat};
use crate::pricing::PriceTable;
use crate::report::Reader;
use onboarding::Onboarding;
use settings::SettingsForm;

const SLOW_EVERY: Duration = Duration::from_secs(5);
const MARKET_EVERY: Duration = Duration::from_secs(30 * 60);
/// Seconds of per-process watts kept for the sparklines.
const PROC_HISTORY: usize = 120;

/// Something the mouse can click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Range(usize),
    Proc(usize),
    Bar(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcSort {
    #[default]
    Energy,
    Now,
    Cost,
}

impl ProcSort {
    pub fn label(self) -> &'static str {
        match self {
            ProcSort::Energy => "energy",
            ProcSort::Now => "now",
            ProcSort::Cost => "cost",
        }
    }
}

/// The history popup of one process.
pub struct Detail {
    pub name: String,
    pub row: Option<ProcRow>,
    /// (unix seconds, watts)
    pub points: Vec<(i64, f64)>,
}

pub struct App {
    pub cfg: Config,
    config_path: PathBuf,
    reader: Reader,
    pub range: Range,
    pub fast: Fast,
    pub slow: Slow,
    cache: AllTimeCache,
    market: Option<PriceTable>,
    market_rx: Option<Receiver<(String, Result<PriceTable, String>)>>,
    market_fetched: Option<(String, Instant)>,
    pub market_status: Option<String>,
    pub settings: Option<SettingsForm>,
    pub onboarding: Option<Onboarding>,
    pub show_help: bool,
    pub stacked: bool,
    pub bar_metric: BarMetric,
    pub bar_sel: Option<usize>,
    pub proc_sel: usize,
    pub proc_sort: ProcSort,
    pub proc_history: HashMap<String, VecDeque<f64>>,
    pub detail: Option<Detail>,
    pub status: Option<(String, Instant)>,
    /// Clickable areas of the last frame.
    pub hits: RefCell<Vec<(Rect, Hit)>>,
    last_slow: Option<Instant>,
    /// Fixed time for reproducible screenshots and tests.
    clock: Option<i64>,
    quit: bool,
}

/// Opens the dashboard. `config_path` is where settings are saved; `setup` starts with the
/// first-run setup.
pub fn run(cfg: Config, config_path: PathBuf, setup: bool) -> Result<()> {
    let mut app = App::new(cfg, config_path);
    if setup {
        app.start_setup();
    }
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = (|| -> Result<()> {
        let mut next_tick = Instant::now();
        while !app.quit {
            if Instant::now() >= next_tick {
                app.tick();
                next_tick = Instant::now() + Duration::from_secs(1);
            }
            terminal.draw(|f| ui::draw(f, &app))?;
            let timeout = next_tick.saturating_duration_since(Instant::now());
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => app.on_key(key),
                    Event::Mouse(m) => app.on_mouse(m),
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Draws one frame into an off-screen buffer (screenshots, tests).
pub fn render(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
        .expect("test backend never fails");
    term.draw(|f| ui::draw(f, app))
        .expect("test backend never fails");
    term.backend().buffer().clone()
}

impl App {
    pub fn new(cfg: Config, config_path: PathBuf) -> Self {
        let reader = Reader::new(DataPaths::for_reader(&cfg));
        let market = (cfg.price.mode == PriceMode::Dynamic)
            .then(|| awattar::load_cache(&cfg.price.dynamic.region));
        App {
            cfg,
            config_path,
            reader,
            range: Range::H24,
            fast: Fast::default(),
            slow: Slow::default(),
            cache: AllTimeCache::default(),
            market,
            market_rx: None,
            market_fetched: None,
            market_status: None,
            settings: None,
            onboarding: None,
            show_help: false,
            stacked: false,
            bar_metric: BarMetric::Energy,
            bar_sel: None,
            proc_sel: 0,
            proc_sort: ProcSort::Energy,
            proc_history: HashMap::new(),
            detail: None,
            status: None,
            hits: RefCell::new(Vec::new()),
            last_slow: None,
            clock: None,
            quit: false,
        }
    }

    /// Freezes the clock at `ts` (unix seconds).
    pub fn set_clock(&mut self, ts: i64) {
        self.clock = Some(ts);
    }

    /// Uses these market prices and skips fetching for a while.
    pub fn set_market(&mut self, table: PriceTable) {
        self.market = Some(table);
        self.market_fetched = Some((self.cfg.price.dynamic.region.clone(), Instant::now()));
        self.last_slow = None;
    }

    /// Fills the process sparklines, e.g. from demo data.
    pub fn prefill_proc_history(&mut self, history: HashMap<String, Vec<f64>>) {
        for (name, v) in history {
            let start = v.len().saturating_sub(PROC_HISTORY);
            self.proc_history
                .insert(name, v[start..].iter().copied().collect());
        }
    }

    pub fn start_setup(&mut self) {
        self.onboarding = Some(Onboarding::new(
            &self.cfg,
            &self.fast,
            self.reader.daemon_running(self.now()),
        ));
    }

    pub fn now(&self) -> i64 {
        self.clock.unwrap_or_else(unix_now)
    }

    /// Live device details of a source key.
    pub fn device(&self, key: &str) -> Option<&DeviceStat> {
        self.reader
            .live
            .as_ref()
            .filter(|_| self.fast.live.is_some())
            .and_then(|l| l.device(key))
    }

    /// Processes in the chosen order.
    pub fn sorted_procs(&self) -> Vec<&ProcRow> {
        let mut v: Vec<&ProcRow> = self.slow.procs.iter().collect();
        match self.proc_sort {
            ProcSort::Energy => v.sort_by(|a, b| b.kwh.total_cmp(&a.kwh)),
            ProcSort::Now => {
                v.sort_by(|a, b| b.now_w.unwrap_or(0.0).total_cmp(&a.now_w.unwrap_or(0.0)))
            }
            ProcSort::Cost => v.sort_by(|a, b| b.cost.total_cmp(&a.cost)),
        }
        v
    }

    /// Re-reads data and recomputes what is shown.
    pub fn tick(&mut self) {
        let now = self.now();
        self.reader.refresh();
        self.poll_market(now);
        match dashboard::build_fast(
            &self.reader,
            &self.cfg,
            self.market.as_ref(),
            self.range,
            now,
        ) {
            Ok(f) => self.fast = f,
            Err(e) => self.flash(format!("read error: {e:#}")),
        }
        if self.last_slow.is_none_or(|t| t.elapsed() >= SLOW_EVERY) {
            self.refresh_slow(now);
        }
        self.record_proc_history();
    }

    fn record_proc_history(&mut self) {
        let Some(live) = self
            .reader
            .live
            .as_ref()
            .filter(|_| self.fast.live.is_some())
        else {
            return;
        };
        let eff = self.cfg.estimate.psu_efficiency_percent / 100.0;
        let current: HashMap<&str, f64> = live
            .proc_watts()
            .into_iter()
            .map(|(n, w, _)| (n, w / eff))
            .collect();
        for p in &self.slow.procs {
            let h = self.proc_history.entry(p.name.clone()).or_default();
            h.push_back(current.get(p.name.as_str()).copied().unwrap_or(0.0));
            while h.len() > PROC_HISTORY {
                h.pop_front();
            }
        }
    }

    fn refresh_slow(&mut self, now: i64) {
        match dashboard::build_slow(
            &self.reader,
            &self.cfg,
            self.market.as_ref(),
            self.range,
            now,
            &mut self.cache,
        ) {
            Ok(s) => self.slow = s,
            Err(e) => self.flash(format!("read error: {e:#}")),
        }
        self.last_slow = Some(Instant::now());
    }

    /// Recomputes everything right away, e.g. after a range or settings change.
    fn invalidate(&mut self) {
        self.last_slow = None;
        self.tick();
    }

    /// Starts a background fetch of market prices when the dynamic tariff needs them.
    fn poll_market(&mut self, now: i64) {
        if let Some(rx) = &self.market_rx
            && let Ok((region, result)) = rx.try_recv()
        {
            self.market_rx = None;
            match result {
                Ok(table) => {
                    self.market_status = None;
                    if region == self.cfg.price.dynamic.region {
                        self.market = Some(table);
                        self.last_slow = None;
                    }
                }
                Err(e) => self.market_status = Some(format!("market prices: {e}")),
            }
        }
        if self.cfg.price.mode != PriceMode::Dynamic || self.market_rx.is_some() {
            return;
        }
        let region = self.cfg.price.dynamic.region.clone();
        let due = match &self.market_fetched {
            Some((r, at)) => *r != region || at.elapsed() >= MARKET_EVERY,
            None => true,
        };
        if !due {
            return;
        }
        if self.market.is_none() {
            self.market = Some(awattar::load_cache(&region));
        }
        let need_from = self.reader.first_minute().unwrap_or(now);
        let (tx, rx) = mpsc::channel();
        let r = region.clone();
        std::thread::spawn(move || {
            let result = awattar::refresh(&r, need_from, now).map_err(|e| format!("{e:#}"));
            let _ = tx.send((r, result));
        });
        self.market_rx = Some(rx);
        self.market_fetched = Some((region, Instant::now()));
        self.market_status = Some("fetching market prices…".into());
    }

    fn flash(&mut self, msg: String) {
        self.status = Some((msg, Instant::now()));
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.onboarding.is_some() || self.settings.is_some() || self.detail.is_some() {
            return;
        }
        let pos = Position::new(m.column, m.row);
        let target = self
            .hits
            .borrow()
            .iter()
            .rev()
            .find(|(r, _)| r.contains(pos))
            .map(|(_, h)| *h);
        match (m.kind, target) {
            (MouseEventKind::Down(MouseButton::Left), Some(Hit::Range(i))) => {
                self.set_range(Range::ALL[i])
            }
            (MouseEventKind::Down(MouseButton::Left), Some(Hit::Proc(i))) => {
                if self.proc_sel == i {
                    self.open_detail();
                } else {
                    self.proc_sel = i;
                }
            }
            (MouseEventKind::Down(MouseButton::Left), Some(Hit::Bar(i))) => self.bar_sel = Some(i),
            (MouseEventKind::ScrollDown, Some(Hit::Proc(_))) => self.move_proc(1),
            (MouseEventKind::ScrollUp, Some(Hit::Proc(_))) => self.move_proc(-1),
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.onboarding.is_some() {
            self.on_onboarding_key(key);
            return;
        }
        if self.settings.is_some() {
            self.on_settings_key(key);
            return;
        }
        if self.detail.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                self.detail = None;
            }
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('?') | KeyCode::Char('h') => self.show_help = true,
            KeyCode::Char('s') => self.settings = Some(SettingsForm::new(&self.cfg)),
            KeyCode::Char('S') => self.start_setup(),
            KeyCode::Char(c @ '1'..='6') => self.set_range(Range::ALL[(c as u8 - b'1') as usize]),
            KeyCode::Left | KeyCode::Char('-') => self.step_range(-1),
            KeyCode::Right | KeyCode::Char('+') => self.step_range(1),
            KeyCode::Down | KeyCode::Char('j') => self.move_proc(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_proc(-1),
            KeyCode::Enter => self.open_detail(),
            KeyCode::Char('e') => self.sort_by(ProcSort::Energy),
            KeyCode::Char('n') => self.sort_by(ProcSort::Now),
            KeyCode::Char('c') => self.sort_by(ProcSort::Cost),
            KeyCode::Char('g') => self.stacked = !self.stacked,
            KeyCode::Char('u') => self.bar_metric = self.bar_metric.next(),
            KeyCode::Char('[') => self.move_bar(-1),
            KeyCode::Char(']') => self.move_bar(1),
            KeyCode::Char('r') => self.invalidate(),
            _ => {}
        }
    }

    fn sort_by(&mut self, s: ProcSort) {
        self.proc_sort = s;
        self.proc_sel = 0;
    }

    fn move_proc(&mut self, delta: isize) {
        let n = self.slow.procs.len();
        if n > 0 {
            self.proc_sel = (self.proc_sel as isize + delta).clamp(0, n as isize - 1) as usize;
        }
    }

    fn move_bar(&mut self, delta: isize) {
        let n = self.slow.bars.len();
        if n == 0 {
            return;
        }
        let cur = self
            .bar_sel
            .or_else(|| self.slow.bars.iter().position(|b| b.current))
            .unwrap_or(n - 1);
        self.bar_sel = Some((cur as isize + delta).clamp(0, n as isize - 1) as usize);
    }

    fn open_detail(&mut self) {
        let Some(row) = self.sorted_procs().get(self.proc_sel).map(|r| (*r).clone()) else {
            return;
        };
        let now = self.now();
        let bucket = (self.range.secs() / 240).max(60);
        let points = self
            .reader
            .proc_series(
                &row.name,
                now - self.range.secs(),
                now + 1,
                bucket,
                &self.cfg,
            )
            .unwrap_or_default();
        self.detail = Some(Detail {
            name: row.name.clone(),
            row: Some(row),
            points,
        });
    }

    fn set_range(&mut self, r: Range) {
        self.range = r;
        self.proc_sel = 0;
        self.bar_sel = None;
        self.invalidate();
    }

    fn step_range(&mut self, delta: isize) {
        let i = Range::ALL
            .iter()
            .position(|r| *r == self.range)
            .unwrap_or(0) as isize;
        let n = Range::ALL.len() as isize;
        self.set_range(Range::ALL[(i + delta).clamp(0, n - 1) as usize]);
    }

    fn on_onboarding_key(&mut self, key: KeyEvent) {
        let Some(ob) = self.onboarding.as_mut() else {
            return;
        };
        match ob.on_key(key) {
            onboarding::Outcome::Continue => {}
            onboarding::Outcome::Done(cfg) => {
                self.onboarding = None;
                self.cfg = Config {
                    data: self.cfg.data.clone(),
                    ..cfg.sanitized()
                };
                self.market = None;
                self.market_fetched = None;
                match self.cfg.save(&self.config_path) {
                    Ok(()) => self.flash(format!("saved {}", self.config_path.display())),
                    Err(e) => self.flash(format!("could not save: {e:#}")),
                }
                self.cache = AllTimeCache::default();
                self.invalidate();
            }
        }
    }

    fn on_settings_key(&mut self, key: KeyEvent) {
        let Some(form) = self.settings.as_mut() else {
            return;
        };
        if let Some(buf) = form.editing.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    form.editing = None;
                    form.error = None;
                }
                KeyCode::Enter => {
                    let input = buf.clone();
                    match form.field().set(&mut self.cfg, &input) {
                        Ok(()) => {
                            form.editing = None;
                            form.error = None;
                            self.invalidate();
                        }
                        Err(e) => form.error = Some(e),
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s') => self.close_settings(),
            KeyCode::Char('S') => {
                self.close_settings();
                self.start_setup();
            }
            KeyCode::Up | KeyCode::Char('k') => form.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => form.move_by(1),
            KeyCode::Left | KeyCode::Right if form.field().is_choice() => {
                form.field()
                    .cycle(&mut self.cfg, key.code == KeyCode::Right);
                self.after_choice();
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let field = form.field();
                if field.is_choice() {
                    field.cycle(&mut self.cfg, true);
                    self.after_choice();
                } else {
                    form.editing = Some(field.get(&self.cfg));
                    form.error = None;
                }
            }
            _ => {}
        }
    }

    fn after_choice(&mut self) {
        if self.cfg.price.mode == PriceMode::Dynamic {
            let region = &self.cfg.price.dynamic.region;
            if self
                .market_fetched
                .as_ref()
                .is_some_and(|(r, _)| r != region)
            {
                self.market = Some(awattar::load_cache(region));
            }
        }
        self.invalidate();
    }

    fn close_settings(&mut self) {
        let Some(form) = self.settings.take() else {
            return;
        };
        self.cfg = std::mem::take(&mut self.cfg).sanitized();
        if self.cfg != form.original {
            match self.cfg.save(&self.config_path) {
                Ok(()) => self.flash(format!("saved {}", self.config_path.display())),
                Err(e) => self.flash(format!("could not save: {e:#}")),
            }
        }
        self.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three days of demo history, frozen at a weekday afternoon.
    fn demo_app(dir: &std::path::Path) -> App {
        let now = 1_795_016_530; // Wednesday 2026-11-18, mid afternoon in Europe
        let cfg = crate::demo::seed(dir, 3, now).unwrap();
        let mut app = App::new(cfg, dir.join("config.toml"));
        app.set_clock(now);
        app
    }

    pub fn screen(app: &App, w: u16, h: u16) -> String {
        let buf = render(app, w, h);
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn type_text(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn renders_every_range_and_size() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = demo_app(tmp.path());
        app.tick();
        assert!(app.fast.running);
        assert!(app.fast.now_wall_w().is_some_and(|w| w > 100.0));
        for (i, _) in Range::ALL.iter().enumerate() {
            app.on_key(key(KeyCode::Char((b'1' + i as u8) as char)));
            for (w, h) in [(180, 50), (140, 44), (120, 40), (80, 24), (40, 12), (10, 4)] {
                let s = screen(&app, w, h);
                if w >= 120 {
                    assert!(s.contains("processes"), "{s}");
                    assert!(s.contains("vllm"), "{s}");
                    assert!(s.contains("Today"), "{s}");
                }
            }
            if std::env::var_os("JUICETOP_DUMP").is_some() {
                println!("{}", screen(&app, 140, 44));
            }
        }
        let today = app.slow.card("Today").unwrap().totals;
        assert!(today.kwh() > 0.0 && today.cost > 0.0 && today.co2_g > 0.0);
        assert!(!app.slow.models.is_empty());
        assert!(app.slow.models[0].wh_per_1k.is_some());
    }

    #[test]
    fn keys_select_sort_and_open_details() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = demo_app(tmp.path());
        app.tick();
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.proc_sel, 1);
        app.on_key(key(KeyCode::Char('c')));
        assert_eq!((app.proc_sort, app.proc_sel), (ProcSort::Cost, 0));
        app.on_key(key(KeyCode::Enter));
        let d = app.detail.as_ref().unwrap();
        assert!(!d.points.is_empty(), "history of {}", d.name);
        assert!(screen(&app, 140, 44).contains("Esc to close"));
        app.on_key(key(KeyCode::Esc));
        assert!(app.detail.is_none());
        app.on_key(key(KeyCode::Char('u')));
        assert_eq!(app.bar_metric, BarMetric::Cost);
        app.on_key(key(KeyCode::Char('[')));
        assert!(app.bar_sel.is_some());
        app.on_key(key(KeyCode::Char('g')));
        assert!(app.stacked);
        let _ = screen(&app, 140, 44);
    }

    #[test]
    fn mouse_clicks_range_tabs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = demo_app(tmp.path());
        app.tick();
        let _ = screen(&app, 140, 44);
        let tab = app
            .hits
            .borrow()
            .iter()
            .find(|(_, h)| *h == Hit::Range(4))
            .map(|(r, _)| *r)
            .unwrap();
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: tab.x + 1,
            row: tab.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.range, Range::D30);
    }

    #[test]
    fn settings_edit_saves_and_reprices() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = demo_app(tmp.path());
        app.on_key(key(KeyCode::Char('2')));
        let before = app.fast.range_totals.unwrap().cost;
        let old_price = app.cfg.price.fixed_ct_per_kwh;
        app.on_key(key(KeyCode::Char('s')));
        assert!(screen(&app, 120, 40).contains("Tariff model"));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down)); // fixed price
        app.on_key(key(KeyCode::Enter));
        for _ in 0..5 {
            app.on_key(key(KeyCode::Backspace));
        }
        type_text(&mut app, "70");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.cfg.price.fixed_ct_per_kwh, 70.0);
        let after = app.fast.range_totals.unwrap().cost;
        assert!(
            (after / before - 70.0 / old_price).abs() < 0.01,
            "{before} -> {after}"
        );
        app.on_key(key(KeyCode::Esc));
        assert!(app.settings.is_none());
        let saved = Config::load(&tmp.path().join("config.toml")).unwrap();
        assert_eq!(saved.price.fixed_ct_per_kwh, 70.0);
    }

    #[test]
    fn onboarding_with_enter_only_saves_country_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = demo_app(tmp.path());
        app.tick();
        app.start_setup();
        for step in 0..4 {
            let s = screen(&app, 120, 40);
            assert!(s.contains(&format!("{} of 4", step + 1)), "{s}");
            if std::env::var_os("JUICETOP_DUMP").is_some() {
                println!("{s}");
            }
            app.on_key(key(KeyCode::Enter));
        }
        assert!(app.onboarding.is_none());
        let saved = Config::load(&tmp.path().join("config.toml")).unwrap();
        assert!(!saved.country.is_empty());
        assert_eq!(saved.price.mode, PriceMode::Fixed);
    }

    #[test]
    fn onboarding_choices_end_up_in_the_config() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = demo_app(tmp.path());
        app.start_setup();
        // Country: pick Austria.
        let ob = app.onboarding.as_mut().unwrap();
        ob.select_country("AT");
        app.on_key(key(KeyCode::Enter));
        // Tariff: day/night, then edit the day price.
        app.on_key(key(KeyCode::Right));
        app.on_key(key(KeyCode::Down));
        type_text(&mut app, "40");
        app.on_key(key(KeyCode::Enter)); // confirm the value
        app.on_key(key(KeyCode::Enter)); // next step
        // Mix: green tariff.
        app.on_key(key(KeyCode::Right));
        app.on_key(key(KeyCode::Enter));
        // Computer: workstation, the second card.
        for _ in 0..3 {
            app.on_key(key(KeyCode::Left));
        }
        app.on_key(key(KeyCode::Right));
        app.on_key(key(KeyCode::Enter));
        let cfg = Config::load(&tmp.path().join("config.toml")).unwrap();
        assert_eq!(cfg.country, "AT");
        assert_eq!(cfg.price.mode, PriceMode::TimeOfUse);
        assert_eq!(cfg.price.time_of_use.high_ct_per_kwh, 40.0);
        assert!(cfg.co2.g_per_kwh < 100.0);
        assert_eq!(cfg.estimate.base_watts, 85.0);
    }
}
