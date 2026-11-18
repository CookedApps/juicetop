# Changelog

This file lists notable changes. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org).

## [Unreleased]

### Added

- Setup on first start in four steps: country, tariff, electricity mix, computer. Country presets
  cover 18 countries. Run it again with `juicetop setup` or `S`.
- Device panels with power against the limit, utilization, clock, temperature, memory and a
  sparkline for each CPU and GPU.
- Energy and cost per token for llama.cpp, vLLM, SGLang, Text Generation Inference and Strata
  servers, found on the machine or passed with `--model-endpoint`. A server's energy includes its
  child processes.
- Price outlook for day/night and dynamic tariffs with the cheapest upcoming window.
- Processes: CPU or GPU tag, sparklines, sorting (`e`, `n`, `c`) and a history view (`Enter`).
- Energy bars with an axis, a unit, a selected bar (`[` `]`) and kWh, money or CO2 (`u`);
  colored by price on variable tariffs.
- Totals with a trend against the previous day, week and month.
- Stacked power graph by device (`g`), mouse support, true color with a 256-color fallback.
- Themes: juicetop, juicetop vivid, juicetop muted, Greyscale, Tokyo Night, Catppuccin Mocha, Dracula, Nord, Gruvbox Dark,
  One Dark, Monokai, Solarized Dark and Light, GitHub Light and the terminal's own colors.
  Choose one in the settings.

### Changed

- New look: filled braille graphs with a power gradient, a status header, and a neutral frame
  that leaves color to the data.
- Process names drop the interpreter: `train.py`, `vllm`, `ComfyUI/main.py`.
- The system service may reach localhost (model servers) and nothing else.
- Money shows the symbol before the amount for $, £ and ¥.

## [0.1.0]

First release.

### Added

- Collector daemon: CPU energy via RAPL (AMD APU socket power without root), NVIDIA GPUs via NVML,
  AMD GPUs via amdgpu; per-process attribution; per-second history for 48 hours and per-minute
  history for good in SQLite.
- Dashboard with power graphs from 5 minutes to 1 year, component and process tables, totals and
  energy bars.
- Prices: fixed, time of use and dynamic via aWATTar day-ahead prices (DE, AT).
- CO2 estimate, base load and PSU loss estimates, editable in the dashboard.
- `juicetop service install`, `juicetop summary`, `juicetop sensors`, `juicetop demo`.
- Install script with prebuilt binaries for x86_64 and aarch64.
