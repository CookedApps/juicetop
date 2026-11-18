//! The live snapshot: a small JSON file the daemon rewrites every second (on tmpfs),
//! holding the last few minutes of per-second data that is not yet in the database.

use std::os::unix::fs::MetadataExt;
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
}

impl LiveSnapshot {
    pub fn write(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
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

const SYSTEM_DB: &str = "/var/lib/juicetop/juicetop.db";
const SYSTEM_LIVE: &str = "/run/juicetop/live.json";

impl DataPaths {
    pub fn system() -> Self {
        Self {
            db: SYSTEM_DB.into(),
            live: SYSTEM_LIVE.into(),
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
        let auto = if Path::new(SYSTEM_DB).exists() {
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

pub fn is_root() -> bool {
    std::fs::metadata("/proc/self")
        .map(|m| m.uid() == 0)
        .unwrap_or(false)
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
