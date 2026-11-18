//! juicetop: measure, store and price the power draw of a Linux PC.
//!
//! The binary is a thin CLI over these modules. See `AGENTS.md` for a map of the code.

pub mod awattar;
pub mod config;
pub mod daemon;
pub mod dashboard;
pub mod demo;
pub mod live;
pub mod pricing;
pub mod procs;
pub mod report;
pub mod sensors;
pub mod service;
pub mod store;
pub mod tui;
