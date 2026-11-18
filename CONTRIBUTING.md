# Contributing to juicetop

Thanks for helping! The most valuable contributions right now are **reports from hardware we have
not seen**: if juicetop misreads or misses a sensor on your machine, open an issue with the output
of `juicetop sensors`.

## Setup

You need Linux and Rust 1.89 or newer (https://rustup.rs).

```bash
git clone https://github.com/CookedApps/juicetop
cd juicetop
cargo test
cargo run -- demo      # try the dashboard on generated data
cargo run -- sensors   # see what your machine exposes
```

To collect real data while developing, run the collector against a scratch database:

```bash
cargo run -- daemon --db /tmp/juicetop.db --live /tmp/juicetop-live.json
```

and point the dashboard at it with a config file:

```toml
# /tmp/juicetop.toml
[data]
db_path = "/tmp/juicetop.db"
live_path = "/tmp/juicetop-live.json"
```

```bash
cargo run -- --config /tmp/juicetop.toml
```

RAPL counters need root. Without it, AMD APUs fall back to socket power; other CPUs report no CPU
data. `sudo -E cargo run -- sensors` shows what root sees.

## Before you open a pull request

- `cargo fmt`, `cargo clippy --all-targets` and `cargo test` pass (CI checks all three).
- New logic has a test. Hardware access is tested with fake sysfs trees in a temp dir; see the
  tests in `src/sensors.rs`.
- Layout changes: run `JUICETOP_DUMP=1 cargo test renders -- --nocapture` and check the output at
  the sizes it prints. If the README screenshots change, regenerate them with
  `./scripts/screenshots.sh`.
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

`AGENTS.md` has a map of the source and the rules that keep the numbers right, for example that
the collector stores only measured joules and every estimate is applied when reading. It is
written for coding agents but is the quickest tour for humans too.

## Releases

Maintainers tag `vX.Y.Z` on `main`. The release workflow builds Linux binaries for x86_64 and
aarch64 and attaches them to a GitHub release, which `install.sh` downloads.
