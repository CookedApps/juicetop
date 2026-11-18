# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org).

## [Unreleased]

## [0.1.0]

First release.

### Added

- Collector daemon: CPU energy via RAPL (AMD APU socket power without root), NVIDIA GPUs via NVML,
  AMD GPUs via amdgpu; per-process attribution; per-second history for 48 hours and per-minute
  history forever in SQLite.
- Dashboard with live and historical power graphs (5 minutes to 1 year), component and process
  tables, totals per hour, day, week, month and all time, and energy bars per hour, day or month.
- Prices: fixed, time of use (HT/NT) and dynamic via aWATTar day-ahead prices (DE, AT).
- CO2 estimate from your electricity mix; base load and PSU loss estimates; all editable in the
  dashboard and applied to the whole history.
- `juicetop service install` for a sandboxed system service or a user service, `juicetop summary`,
  `juicetop sensors`, `juicetop demo`.
- Install script with prebuilt binaries for x86_64 and aarch64.
