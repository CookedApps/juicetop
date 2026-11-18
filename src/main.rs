use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use juicetop::config::{Config, PriceMode};
use juicetop::dashboard::{self, AllTimeCache, Range};
use juicetop::live::DataPaths;
use juicetop::report::{Reader, fmt_co2, fmt_kwh, fmt_money, fmt_watts};
use juicetop::{awattar, daemon, demo, sensors, service, tui};

#[derive(Parser)]
#[command(
    version,
    about = "Your PC's power draw, electricity cost and CO2, live and over time"
)]
struct Cli {
    /// Settings file (price, CO2, estimates). Default: ~/.config/juicetop/config.toml
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the dashboard (default). Starts the setup on first use.
    Tui,
    /// Run the setup again: country, tariff, electricity mix and computer
    Setup,
    /// Open the dashboard with generated example data, to try juicetop without a collector
    Demo {
        /// Days of example history
        #[arg(long, default_value_t = 60)]
        days: i64,
    },
    /// Print totals for this hour, today, this week, this month and all time
    Summary,
    /// Show which sensors work on this machine, with a 1 second reading
    Sensors,
    /// Install, remove or inspect the background collector (systemd, launchd or Task Scheduler)
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Run the collector in the foreground (what the service runs)
    Daemon {
        /// Database file. Default: the system data folder as root or administrator, else your own
        #[arg(long)]
        db: Option<PathBuf>,
        /// Live snapshot file. Default: next to the system data as root or administrator, else your own
        #[arg(long)]
        live: Option<PathBuf>,
        /// Hours of per-second data to keep (minute data is kept forever)
        #[arg(long, default_value_t = 48)]
        raw_retention_hours: u64,
        /// Do not attribute energy to processes
        #[arg(long)]
        no_procs: bool,
        /// Do not look for local model servers (llama.cpp, vLLM, SGLang, TGI)
        #[arg(long)]
        no_models: bool,
        /// Prometheus metrics URL of a model server to watch, e.g. http://127.0.0.1:8000/metrics
        #[arg(long = "model-endpoint", value_name = "URL")]
        model_endpoints: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Install and start the collector. As root: system service (recommended); with --user: for your account
    Install {
        #[arg(long)]
        user: bool,
    },
    /// Stop and remove the collector
    Uninstall {
        #[arg(long)]
        user: bool,
        /// Also delete the recorded data
        #[arg(long)]
        purge: bool,
    },
    /// Show the collector's service status
    Status {
        #[arg(long)]
        user: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli
        .config
        .clone()
        .or_else(Config::default_path)
        .context("no config directory")?;
    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => {
            let first_run = !config_path.exists();
            tui::run(Config::load(&config_path)?, config_path, first_run)
        }
        Command::Setup => tui::run(Config::load(&config_path)?, config_path, true),
        Command::Demo { days } => run_demo(&config_path, days),
        Command::Summary => summary(&config_path),
        Command::Sensors => sensors_cmd(),
        Command::Service { action } => match action {
            ServiceAction::Install { user } => service::install(user),
            ServiceAction::Uninstall { user, purge } => service::uninstall(user, purge),
            ServiceAction::Status { user } => service::status(user),
        },
        Command::Daemon {
            db,
            live,
            raw_retention_hours,
            no_procs,
            no_models,
            model_endpoints,
        } => {
            let defaults = DataPaths::for_daemon();
            daemon::run(daemon::DaemonOpts {
                db: db.unwrap_or(defaults.db),
                live: live.unwrap_or(defaults.live),
                raw_retention_hours,
                procs: !no_procs,
                models: !no_models,
                model_endpoints,
            })
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Prints without panicking when the reader went away (`juicetop summary | head`).
fn emit(text: &str) {
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

fn run_demo(config_path: &Path, days: i64) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("juicetop-demo-{}", std::process::id()));
    eprintln!("generating {days} days of example data…");
    let demo_cfg = demo::seed(&dir, days.clamp(1, 730), unix_now())?;
    // Your own price and CO2 settings, but the demo's data. Settings changes stay in the demo.
    let mut cfg = Config::load(config_path).unwrap_or_default();
    cfg.data = demo_cfg.data.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let sim = {
        let stop = Arc::clone(&stop);
        let cfg = cfg.clone();
        std::thread::spawn(move || demo::simulate(&cfg, stop))
    };
    let result = tui::run(cfg, dir.join("config.toml"), false);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = sim.join();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn summary(config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    let paths = DataPaths::for_reader(&cfg);
    let reader = Reader::new(paths.clone());
    let now = unix_now();
    let market = (cfg.price.mode == PriceMode::Dynamic).then(|| {
        let region = &cfg.price.dynamic.region;
        awattar::refresh(region, reader.first_minute().unwrap_or(now), now).unwrap_or_else(|e| {
            eprintln!("juicetop: market prices unavailable: {e:#}");
            awattar::load_cache(region)
        })
    });
    let fast = dashboard::build_fast(&reader, &cfg, market.as_ref(), Range::H24, now)?;
    let slow = dashboard::build_slow(
        &reader,
        &cfg,
        market.as_ref(),
        Range::H24,
        now,
        &mut AllTimeCache::default(),
    )?;
    let cur = &cfg.currency;

    let mut out = String::new();
    writeln!(
        out,
        "juicetop  {}  ({})",
        paths.db.display(),
        fast.price_desc
    )?;
    match &fast.live {
        Some(n) => writeln!(
            out,
            "now       {} at the wall, {} per hour",
            fmt_watts(n.wall),
            fmt_money(n.cost_per_hour, cur)
        )?,
        None => writeln!(out, "now       collector not running")?,
    }
    writeln!(
        out,
        "\n{:<22}{:>12}{:>12}{:>12}{:>10}",
        "", "energy", "cost", "CO2", "avg"
    )?;
    for card in &slow.cards {
        let (name, t) = (card.label, &card.totals);
        let mark = if t.price_fallback { "*" } else { "" };
        writeln!(
            out,
            "{:<22}{:>12}{:>12}{:>12}{:>10}",
            name,
            fmt_kwh(t.kwh()),
            format!("{}{mark}", fmt_money(t.cost, cur)),
            fmt_co2(t.co2_g),
            fmt_watts(t.avg_watts())
        )?;
    }
    if slow.cards.iter().any(|c| c.totals.price_fallback) {
        writeln!(
            out,
            "* partly priced with the fixed price because market prices were missing"
        )?;
    }
    if !slow.procs.is_empty() {
        writeln!(out, "\nTop processes, last 24 h")?;
        for p in slow.procs.iter().take(10) {
            writeln!(
                out,
                "  {:<28}{:>10}{:>12}{:>6.0}%",
                p.name,
                fmt_kwh(p.kwh),
                fmt_money(p.cost, cur),
                p.share * 100.0
            )?;
        }
    }
    for n in &fast.notes {
        writeln!(out, "note: {n}")?;
    }
    emit(&out);
    Ok(())
}

fn sensors_cmd() -> Result<()> {
    let nvml = nvml_wrapper::Nvml::init().ok();
    let (mut sensors, notes) = sensors::probe(nvml.as_ref());
    let mut out = String::new();
    for n in &notes {
        writeln!(out, "note: {n}")?;
    }
    if sensors.is_empty() {
        writeln!(out, "no power sensors found")?;
        emit(&out);
        return Ok(());
    }
    let start = Instant::now();
    std::thread::sleep(Duration::from_secs(1));
    let dt = start.elapsed().as_secs_f64();
    for s in &mut sensors {
        let w = s.read(nvml.as_ref(), dt).map(|r| r.joules / dt);
        let info = s.info();
        let reading = w.map(fmt_watts).unwrap_or_else(|| "no reading".into());
        writeln!(out, "{:<12} {:<52} {reading}", info.key, info.label)?;
    }
    emit(&out);
    Ok(())
}
