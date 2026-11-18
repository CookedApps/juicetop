# juicetop: notes for coding agents

A power monitor in Rust for Linux, with untested ports to macOS and Windows. A collector daemon records CPU and GPU energy into SQLite; a ratatui
dashboard turns it into watts, kWh, money and CO2. `README.md` has the user view.

## Map

| Path | What |
|---|---|
| `src/main.rs` | CLI (clap). Wiring and printing, no logic. |
| `src/sensors.rs` | Power probes: RAPL, amdgpu hwmon (APU socket power or discrete GPU), NVML, plus `PlatformSensor` for OS-specific ones. Each returns joules since the last read. |
| `src/macos.rs`, `src/powermetrics.rs` | macOS: CPU, GPU and ANE energy from a `powermetrics` child process; its plist parser (portable, tested everywhere). |
| `src/windows.rs` | Windows: CPU package power from the PDH `Energy Meter` counters, the elevation check, console detach. |
| `src/sysproc.rs` | macOS and Windows: processes, CPU load and memory through `sysinfo`, where `/proc` is missing. |
| `src/devices.rs` | Live device details: utilization, temperature, clock, memory, power limit. Never stored. |
| `src/procs.rs` | Per-process attribution: CPU ticks from `/proc`, readable process names, proportional split. |
| `src/models.rs` | Local model servers: discovery via listening sockets, Prometheus token counters, per-minute deltas. |
| `src/daemon.rs` | The collector loop: one tick per wall-clock second, live snapshot every second, one DB transaction per minute. |
| `src/store.rs` | SQLite schema, migrations and queries (`sources`, `samples`, `minutes`, `proc_minutes`, `token_minutes`). |
| `src/live.rs` | Live snapshot JSON (tmpfs) and the system vs user path rules. |
| `src/pricing.rs`, `src/awattar.rs` | Price per kWh at a timestamp (fixed, time of use, dynamic); market prices with an on-disk cache. |
| `src/presets.rs` | Country defaults for the setup (currency, price, VAT, grid CO2) with sources. |
| `src/report.rs` | Joules to wall energy (base load and PSU loss estimates), totals, calendar slots, number formatting. |
| `src/dashboard.rs` | Everything the screen shows, as plain data (`Fast` every second, `Slow` every 5 s). No terminal code. |
| `src/tui/mod.rs` | App state, event loop, keys and mouse. |
| `src/tui/ui.rs` | Drawing of the dashboard and its popups. |
| `src/tui/onboarding.rs` | The four-step setup. |
| `src/tui/theme.rs` | Themes. Every color comes from `theme::t()`; add a theme with the `ide_theme!` macro in `THEMES`. |
| `src/tui/widgets.rs` | Drawing primitives: braille area graphs, bars, meters, sparklines. |
| `src/tui/settings.rs` | The settings form; each field parses and validates its own input. |
| `src/demo.rs` | Deterministic example data (a two-GPU workstation). Used by `juicetop demo`, render tests and screenshots. |
| `src/service/` | `juicetop service install/uninstall/status`: systemd (`packaging/systemd`), launchd (`packaging/launchd`), Task Scheduler (`packaging/windows`). |
| `examples/screenshots.rs`, `scripts/screenshots.sh` | README screenshots from demo data. |

## Rules that keep it correct

- **Store joules, apply opinions at read time.** The daemon writes measured joules. Price, CO2,
  base load and PSU efficiency get applied in `report.rs` and `dashboard.rs`, so a changed setting
  re-prices history. Keep estimates out of the database.
- **`ui.rs` draws.** New numbers go into `dashboard.rs` with a test, then `ui.rs` formats them.
- **Color goes to data.** Frames, titles and labels use the theme's neutral roles (`border`,
  `title`, `dim`). Graphs, meters, money, device identity and status may use color. Never hardcode
  a color; add a role to `Theme` if one is missing.
- **Readers open the database read-only.** The database uses a rollback journal (not WAL) so an
  unprivileged reader can open a root-owned file. Readers must cope with older schemas: check
  `Store::detect` before relying on a new column or table.
- **One write per minute.** Fresher data travels through the live snapshot; its `flushed_until`
  marks the boundary. New snapshot fields need `#[serde(default)]` so a new dashboard can read an
  old collector's file.
- **Sources keep their keys.** A key (`cpu`, `nvidia:0`, `amdgpu:0`) must stay stable across
  restarts and between the RAPL and socket-power CPU paths.
- **Network.** The collector talks to model servers on 127.0.0.1 and nothing else; the systemd unit
  enforces this with `IPAddressAllow=localhost`. The dashboard and `summary` call aWATTar in
  dynamic mode.

## Platforms

Linux is the tested platform. macOS and Windows code sits behind `#[cfg]` and is untested on real
machines; CI builds it and runs the tests on both. Keep Linux-only readers (sysfs, `/proc`)
compiling everywhere when they only read files, so their tests run on every OS. To check the other
targets locally without their C toolchains:

```bash
rustup target add x86_64-pc-windows-gnu aarch64-apple-darwin
CC_x86_64_pc_windows_gnu=/bin/true AR_x86_64_pc_windows_gnu=/bin/true \
  cargo clippy --all-targets --target x86_64-pc-windows-gnu
CC_aarch64_apple_darwin=/bin/true AR_aarch64_apple_darwin=/bin/true \
  cargo clippy --all-targets --target aarch64-apple-darwin
```

## Commands

```bash
cargo test                       # unit tests, render tests on demo data
cargo clippy --all-targets       # must be clean; CI uses -D warnings
cargo fmt
cargo run -- sensors             # what this machine exposes
cargo run -- demo                # the TUI on generated data, no collector needed
cargo run -- daemon --db /tmp/j.db --live /tmp/live.json
JUICETOP_DUMP=1 cargo test renders -- --nocapture   # print rendered screens as text
./scripts/screenshots.sh         # regenerate docs/images/*.png (needs Chromium)
```

## Testing approach

Tests fake hardware with sysfs and `/proc` trees in temp dirs (`sensors.rs`, `devices.rs`,
`procs.rs`, `models.rs`). TUI tests seed demo data, freeze the clock (`App::set_clock`) and render
into a `TestBackend` at several sizes, including 40×12 and 10×4. After a layout change, run the
render tests and read the dump.

## Adding things

- **A sensor:** add a variant to `Sensor` in `sensors.rs`, probe it in `probe()`, return joules
  since the last read, give it a stable key. Add a fake-sysfs test and a row in the README table.
- **A model server:** add its (prompt, generated) counter names to `COUNTERS` and a name to
  `SERVER_NAMES` in `models.rs`, with a parsing test.
- **A tariff:** extend `PriceMode` and `Pricer::at`, add config fields with defaults, add rows to
  `tui/settings.rs` and a card to `tui/onboarding.rs`. Add a pricing test.
- **A setting:** give it a serde default so old config files keep loading.

## Conventions

- Conventional Commits, one line, no body: `feat(sensors): read Intel GPU energy`.
- English for code, docs and UI text.
- Few dependencies; prefer the standard library.
