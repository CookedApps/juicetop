# Contributing to juicetop

Reports from hardware we have not seen help the project most. If juicetop misreads or misses a
sensor on your machine, open an issue and paste the output of `juicetop sensors`.

## Setup

You need Linux and Rust 1.89 or newer (https://rustup.rs).

```bash
git clone https://github.com/CookedApps/juicetop
cd juicetop
cargo test
cargo run -- demo      # the dashboard on generated data
cargo run -- sensors   # what your machine exposes
```

For real data while you work, run the collector against a scratch database:

```bash
cargo run -- daemon --db /tmp/juicetop.db --live /tmp/juicetop-live.json
```

Point the dashboard at it with a config file:

```toml
# /tmp/juicetop.toml
[data]
db_path = "/tmp/juicetop.db"
live_path = "/tmp/juicetop-live.json"
```

```bash
cargo run -- --config /tmp/juicetop.toml
```

RAPL counters need root. Without root, AMD APUs fall back to socket power and other CPUs report
no CPU data. `sudo -E cargo run -- sensors` shows what root sees.

## Before you open a pull request

- Run `cargo fmt`, `cargo clippy --all-targets` and `cargo test`. CI runs all three.
- Add a test for new logic. Tests fake hardware with sysfs and `/proc` trees in a temp dir; the
  tests in `src/sensors.rs` show how.
- For layout changes, run `JUICETOP_DUMP=1 cargo test renders -- --nocapture` and read the screens
  it prints. If the README screenshots change, regenerate them with `./scripts/screenshots.sh`.
- Update `README.md` when behavior, settings or supported hardware change, and add a line to
  `CHANGELOG.md` under "Unreleased".

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org), one line:

```
feat(sensors): read Intel GPU energy from the xe driver
fix(tui): keep the process list scrolled after a refresh
docs: explain the PSU efficiency setting
```

## Code layout

`AGENTS.md` maps the source and lists the rules that keep the numbers right. Example: the
collector stores measured joules, and the dashboard applies every estimate when it reads them.
Coding agents read that file first; it makes a fast tour for people too.

## Releases

A maintainer tags `vX.Y.Z` on `main`. The release workflow builds Linux binaries for x86_64 and
aarch64 and attaches them to a GitHub release, where `install.sh` finds them.
