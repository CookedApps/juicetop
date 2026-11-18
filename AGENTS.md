# juicetop: notes for coding agents

A Linux power monitor in Rust: a collector daemon records CPU and GPU energy into SQLite, a
ratatui dashboard turns it into watts, kWh, money and CO2. Read `README.md` for the user view.

## Map

| Path | What |
|---|---|
| `src/main.rs` | CLI only (clap). No logic beyond wiring and printing. |
| `src/sensors.rs` | Hardware probes: RAPL, amdgpu hwmon (APU socket PPT or discrete GPU), NVML. Each returns joules since the last read. |
| `src/procs.rs` | Per-process attribution: CPU ticks from `/proc`, readable process names, proportional split. |
| `src/daemon.rs` | The collector loop: one tick per wall-clock second, live snapshot every second, one DB transaction per minute. |
| `src/store.rs` | SQLite schema and queries (`samples`, `minutes`, `proc_minutes`, `sources`). |
| `src/live.rs` | Live snapshot JSON (tmpfs) and the system vs user path rules. |
| `src/pricing.rs` | Price per kWh at a timestamp: fixed, time of use (HT/NT), dynamic. |
| `src/awattar.rs` | Day-ahead market prices from aWATTar with an on-disk cache. |
| `src/report.rs` | Joules to wall energy (base load and PSU loss estimates), totals, calendar slots, number formatting. |
| `src/dashboard.rs` | Everything the screen shows, as plain data (`Fast` every second, `Slow` every 5 s). No terminal code. |
| `src/tui/` | Event loop (`mod.rs`), drawing only (`ui.rs`), settings form (`settings.rs`). |
| `src/demo.rs` | Deterministic example data. Used by `juicetop demo`, render tests and screenshots. |
| `src/service.rs` | `juicetop service install/uninstall/status`; embeds `packaging/systemd/*.service`. |
| `examples/screenshots.rs`, `scripts/screenshots.sh` | README screenshots from demo data. |

## Rules that keep it correct

- **Store joules, apply opinions at read time.** The daemon writes measured joules only. Price,
  CO2, base load and PSU efficiency are applied in `report.rs`/`dashboard.rs`, so changing a
  setting re-prices history. Do not bake estimates into the database.
- **`ui.rs` draws, it does not compute.** New numbers go into `dashboard.rs` (and get a test there),
  then `ui.rs` formats them.
- **Readers are read-only.** The TUI opens SQLite with `SQLITE_OPEN_READ_ONLY`; the database uses a
  rollback journal (not WAL) so an unprivileged reader can open a root-owned file. Keep it that way.
- **One write per minute.** The daemon must not commit more often; fresher data travels through
  the live snapshot. `flushed_until` in the snapshot marks the boundary between the two.
- **Sources are keyed, not numbered.** A source key (`cpu`, `nvidia:0`, `amdgpu:0`) must stay stable
  across restarts and between the RAPL and PPT CPU paths.
- The collector needs no network. Only the TUI and `summary` talk to aWATTar, only in dynamic mode.

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

Hardware is mocked with fake sysfs trees in temp dirs (`sensors.rs`, `procs.rs` tests). The TUI is
tested by seeding demo data, freezing the clock (`App::set_clock`) and rendering into a
`TestBackend` at several sizes. When you change layout, run the render tests and look at the dump.

## Adding things

- **A sensor:** add a variant to `Sensor` in `sensors.rs`, probe it in `probe()`, return joules
  since the last read, give it a stable key. Add a fake-sysfs test. Update the README table.
- **A tariff:** extend `PriceMode` and `Pricer::at`, add fields with defaults to `config.rs`, add
  rows to `tui/settings.rs`. Add a pricing test.
- **A setting:** it must have a serde default so old config files keep loading.

## Conventions

- Conventional Commits, one line, no body: `feat(sensors): read Intel GPU energy`.
- English for code, docs and UI text.
- Keep dependencies few; prefer the standard library.
