//! The live snapshot: a small JSON file the daemon rewrites every second (on tmpfs),
//! holding the last few minutes of per-second data that is not yet in the database.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::sensors::SourceInfo;

/// How many seconds of history the snapshot carries. Must exceed the flush interval (60 s).
pub const LIVE_SECONDS: usize = 180;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSource {
    pub id: i64,
    #[serde(flatten)]
    pub info: SourceInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSample {
    pub ts: i64,
    /// Joules per source, parallel to `LiveSnapshot::sources`. `None` when a read failed.
    pub joules: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSnapshot {
    pub pid: u32,
    /// Unix seconds of the last write.
    pub updated: i64,
    /// The database holds every second before this timestamp.
    pub flushed_until: i64,
    pub sources: Vec<LiveSource>,
    pub samples: Vec<LiveSample>,
    /// Process name and watts, averaged over the last few seconds, largest first.
    pub procs: Vec<(String, f64)>,
    /// Probe notes, e.g. "RAPL needs root".
    pub notes: Vec<String>,
    /// Like `procs`, split into total and GPU watts. Empty from older collectors.
    #[serde(default)]
    pub proc_split: Vec<LiveProc>,
    /// Utilization, temperature and limits per source.
    #[serde(default)]
    pub devices: Vec<DeviceStat>,
    /// Local model servers and their token rates.
    #[serde(default)]
    pub models: Vec<ModelLive>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveProc {
    pub name: String,
    pub total_w: f64,
    pub gpu_w: f64,
}

/// What a device is doing right now, besides drawing power. Every field is optional because
/// not every driver reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceStat {
    /// Source key, e.g. "cpu" or "nvidia:0".
    pub key: String,
    pub util_pct: Option<f64>,
    pub temp_c: Option<f64>,
    pub clock_mhz: Option<f64>,
    pub mem_used_mb: Option<f64>,
    pub mem_total_mb: Option<f64>,
    pub power_limit_w: Option<f64>,
}

/// A local model server found by the collector.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelLive {
    /// "host:port"
    pub endpoint: String,
    /// The process that serves it, as named in the process list.
    pub process: String,
    pub model: String,
    /// Generated tokens per second over the last seconds.
    pub gen_tps: f64,
    pub prompt_tps: f64,
}

impl LiveSnapshot {
    pub fn write(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// (name, total watts, GPU watts) per process, from the split list when available.
    pub fn proc_watts(&self) -> Vec<(&str, f64, f64)> {
        if self.proc_split.is_empty() {
            self.procs
                .iter()
                .map(|(n, w)| (n.as_str(), *w, 0.0))
                .collect()
        } else {
            self.proc_split
                .iter()
                .map(|p| (p.name.as_str(), p.total_w, p.gpu_w))
                .collect()
        }
    }

    pub fn device(&self, key: &str) -> Option<&DeviceStat> {
        self.devices.iter().find(|d| d.key == key)
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

/// Where the database and live snapshot live.
#[derive(Debug, Clone, PartialEq)]
pub struct DataPaths {
    pub db: PathBuf,
    pub live: PathBuf,
}

impl DataPaths {
    /// Where a collector running as root (or as SYSTEM on Windows) keeps its data.
    #[cfg(target_os = "linux")]
    pub fn system() -> Self {
        Self {
            db: "/var/lib/juicetop/juicetop.db".into(),
            live: "/run/juicetop/live.json".into(),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn system() -> Self {
        Self {
            db: "/Library/Application Support/juicetop/juicetop.db".into(),
            live: "/var/run/juicetop/live.json".into(),
        }
    }

    #[cfg(windows)]
    pub fn system() -> Self {
        let dir = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"))
            .join("juicetop");
        Self {
            db: dir.join("juicetop.db"),
            live: dir.join("live.json"),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    pub fn system() -> Self {
        Self {
            db: "/var/db/juicetop/juicetop.db".into(),
            live: "/var/run/juicetop/live.json".into(),
        }
    }

    pub fn user() -> Self {
        let data = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("juicetop");
        let run = dirs::runtime_dir()
            .or_else(dirs::cache_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("juicetop");
        Self {
            db: data.join("juicetop.db"),
            live: run.join("live.json"),
        }
    }

    /// Paths for a daemon run by the current user: system paths for root, per-user otherwise.
    pub fn for_daemon() -> Self {
        if is_root() {
            Self::system()
        } else {
            Self::user()
        }
    }

    /// Paths for a reader: config overrides first, then a system daemon's data, then the user's.
    pub fn for_reader(cfg: &Config) -> Self {
        let auto = if Self::system().db.exists() {
            Self::system()
        } else {
            Self::user()
        };
        Self {
            db: cfg.data.db_path.clone().unwrap_or(auto.db),
            live: cfg.data.live_path.clone().unwrap_or(auto.live),
        }
    }
}

/// Root on Unix, an elevated process on Windows.
#[cfg(unix)]
pub fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(windows)]
pub fn is_root() -> bool {
    crate::windows::is_elevated()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::SourceKind;

    #[test]
    fn snapshot_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("live.json");
        let snap = LiveSnapshot {
            pid: 1,
            updated: 100,
            flushed_until: 60,
            sources: vec![LiveSource {
                id: 1,
                info: SourceInfo {
                    key: "cpu".into(),
                    label: "CPU".into(),
                    kind: SourceKind::Cpu,
                },
            }],
            samples: vec![LiveSample {
                ts: 99,
                joules: vec![Some(42.0)],
            }],
            procs: vec![("llama-server".into(), 180.0)],
            notes: vec![],
            proc_split: vec![],
            devices: vec![DeviceStat {
                key: "cpu".into(),
                util_pct: Some(12.0),
                ..Default::default()
            }],
            models: vec![],
        };
        snap.write(&path).unwrap();
        assert_eq!(LiveSnapshot::read(&path).unwrap(), snap);
    }

    #[test]
    fn config_overrides_reader_paths() {
        let mut cfg = Config::default();
        cfg.data.db_path = Some("/tmp/x.db".into());
        assert_eq!(DataPaths::for_reader(&cfg).db, PathBuf::from("/tmp/x.db"));
    }
}
