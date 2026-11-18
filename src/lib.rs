//! juicetop: measure, store and price the power draw of a computer (Linux; macOS and Windows
//! untested).
//!
//! The binary is a thin CLI over these modules. See `AGENTS.md` for a map of the code.

pub mod awattar;
pub mod config;
pub mod daemon;
pub mod dashboard;
pub mod demo;
pub mod devices;
pub mod live;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod models;
pub mod powermetrics;
pub mod presets;
pub mod pricing;
pub mod procs;
pub mod report;
pub mod sensors;
pub mod service;
pub mod store;
#[cfg(not(target_os = "linux"))]
pub mod sysproc;
pub mod tui;
#[cfg(windows)]
pub mod windows;
