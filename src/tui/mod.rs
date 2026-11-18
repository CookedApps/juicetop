//! The dashboard: event loop and app state. Drawing lives in `ui`.

mod settings;
mod ui;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::awattar;
use crate::config::{Config, PriceMode};
use crate::dashboard::{self, AllTimeCache, Fast, Range, Slow};
use crate::live::DataPaths;
use crate::pricing::PriceTable;
use crate::report::Reader;
use settings::SettingsForm;

const SLOW_EVERY: Duration = Duration::from_secs(5);
const MARKET_EVERY: Duration = Duration::from_secs(30 * 60);

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
    pub show_help: bool,
    pub proc_scroll: usize,
    pub status: Option<(String, Instant)>,
    last_slow: Option<Instant>,
    /// Fixed time for reproducible screenshots and tests.
    clock: Option<i64>,
    quit: bool,
}

/// Opens the dashboard. `config_path` is where settings changes are saved.
pub fn run(cfg: Config, config_path: PathBuf) -> Result<()> {
    let mut app = App::new(cfg, config_path);

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        let mut next_tick = Instant::now();
        while !app.quit {
            if Instant::now() >= next_tick {
                app.tick();
                next_tick = Instant::now() + Duration::from_secs(1);
            }
            terminal.draw(|f| ui::draw(f, &app))?;
            let timeout = next_tick.saturating_duration_since(Instant::now());
            if event::poll(timeout)?
                && let Event::Key(key) = event::read()?
                && key.kind != KeyEventKind::Release
            {
                app.on_key(key);
            }
        }
        Ok(())
    })();
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
            range: Range::M5,
            fast: Fast::default(),
            slow: Slow::default(),
            cache: AllTimeCache::default(),
            market,
            market_rx: None,
            market_fetched: None,
            market_status: None,
            settings: None,
            show_help: false,
            proc_scroll: 0,
            status: None,
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

    fn now(&self) -> i64 {
        self.clock.unwrap_or_else(unix_now)
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

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.settings.is_some() {
            self.on_settings_key(key);
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
            KeyCode::Char(c @ '1'..='6') => {
                self.range = Range::ALL[(c as u8 - b'1') as usize];
                self.proc_scroll = 0;
                self.invalidate();
            }
            KeyCode::Left | KeyCode::Char('-') => self.step_range(-1),
            KeyCode::Right | KeyCode::Char('+') => self.step_range(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.proc_scroll =
                    (self.proc_scroll + 1).min(self.slow.procs.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.proc_scroll = self.proc_scroll.saturating_sub(1)
            }
            KeyCode::Char('r') => self.invalidate(),
            _ => {}
        }
    }

    fn step_range(&mut self, delta: isize) {
        let i = Range::ALL
            .iter()
            .position(|r| *r == self.range)
            .unwrap_or(0) as isize;
        let n = Range::ALL.len() as isize;
        self.range = Range::ALL[(i + delta).clamp(0, n - 1) as usize];
        self.proc_scroll = 0;
        self.invalidate();
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

    /// Three days of demo history, frozen at a weekday afternoon with the LLM server busy.
    fn demo_app(dir: &std::path::Path) -> App {
        let now = 1_795_016_530; // 2026-11-18, mid afternoon in Europe
        let cfg = crate::demo::seed(dir, 3, now).unwrap();
        let mut app = App::new(cfg, dir.join("config.toml"));
        app.set_clock(now);
        app
    }

    fn screen(app: &App, w: u16, h: u16) -> String {
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

    #[test]
    fn renders_every_range_and_size() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = demo_app(tmp.path());
        app.tick();
        assert!(app.fast.running);
        assert!(app.fast.now_wall_w.is_some_and(|w| w > 50.0));
        for (i, _) in Range::ALL.iter().enumerate() {
            app.on_key(key(KeyCode::Char((b'1' + i as u8) as char)));
            for (w, h) in [(160, 48), (120, 40), (80, 24), (40, 12), (10, 4)] {
                let s = screen(&app, w, h);
                if w >= 120 {
                    assert!(s.contains("Total at the wall"), "{s}");
                    assert!(s.contains("llama-server"), "{s}");
                }
            }
            if std::env::var_os("JUICETOP_DUMP").is_some() {
                println!("{}", screen(&app, 130, 42));
            }
        }
        let today = app.slow.periods.iter().find(|p| p.0 == "Today").unwrap().1;
        assert!(today.kwh() > 0.0 && today.cost > 0.0 && today.co2_g > 0.0);
    }

    #[test]
    fn settings_edit_saves_and_reprices() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = demo_app(tmp.path());
        app.on_key(key(KeyCode::Char('2')));
        let before = app.fast.range_totals.unwrap().cost;
        app.on_key(key(KeyCode::Char('s')));
        assert!(screen(&app, 120, 40).contains("Tariff model"));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down)); // fixed price
        app.on_key(key(KeyCode::Enter));
        for _ in 0..5 {
            app.on_key(key(KeyCode::Backspace));
        }
        for c in "70".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.cfg.price.fixed_ct_per_kwh, 70.0);
        let after = app.fast.range_totals.unwrap().cost;
        assert!((after / before - 2.0).abs() < 0.01, "{before} -> {after}");
        app.on_key(key(KeyCode::Esc));
        assert!(app.settings.is_none());
        let saved = Config::load(&tmp.path().join("config.toml")).unwrap();
        assert_eq!(saved.price.fixed_ct_per_kwh, 70.0);
        if std::env::var_os("JUICETOP_DUMP").is_some() {
            app.on_key(key(KeyCode::Char('s')));
            println!("{}", screen(&app, 130, 42));
        }
    }
}
