//! SQLite storage.
//!
//! * `samples`: joules per source per second, kept for a limited time (default 48 h).
//! * `minutes`: joules per source per minute plus covered seconds, kept forever.
//! * `proc_minutes`: attributed joules per process name per minute, kept forever.
//!
//! The daemon writes one transaction per minute, so the disk sees about 1 commit a minute.
//! The newest, not yet flushed seconds are published through the live snapshot instead.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, params};

use crate::sensors::{SourceInfo, SourceKind};

pub const SCHEMA_VERSION: i64 = 1;

pub struct Store {
    conn: Connection,
}

/// Everything collected during one minute.
#[derive(Debug, Default, Clone)]
pub struct MinuteBatch {
    /// Unix seconds of the minute start.
    pub minute: i64,
    /// Seconds with a sample in this minute.
    pub secs: u32,
    /// (unix second, source id, joules)
    pub samples: Vec<(i64, i64, f64)>,
    /// source id -> joules
    pub totals: BTreeMap<i64, f64>,
    /// process name -> joules
    pub procs: BTreeMap<String, f64>,
}

/// Energy in one time bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    /// Unix seconds of the bucket start.
    pub ts: i64,
    /// Seconds of data inside the bucket.
    pub secs: f64,
    /// source id -> joules
    pub joules: BTreeMap<i64, f64>,
}

impl Bucket {
    pub fn new(ts: i64) -> Self {
        Self {
            ts,
            secs: 0.0,
            joules: BTreeMap::new(),
        }
    }

    pub fn measured(&self) -> f64 {
        self.joules.values().sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSource {
    pub id: i64,
    pub info: SourceInfo,
}

impl Store {
    pub fn open_rw(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // Rollback journal (not WAL) so that unprivileged readers can open a root-owned database.
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Skips fsync. Only for throwaway databases (tests, demo data).
    pub fn unsafe_fast_writes(&self) {
        self.conn.pragma_update(None, "synchronous", "OFF").unwrap();
        self.conn
            .pragma_update(None, "journal_mode", "MEMORY")
            .unwrap();
    }

    pub fn open_ro(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        Ok(Self { conn })
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS sources (
                 id INTEGER PRIMARY KEY,
                 key TEXT NOT NULL UNIQUE,
                 label TEXT NOT NULL,
                 kind TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS samples (
                 ts INTEGER NOT NULL,
                 source_id INTEGER NOT NULL,
                 joules REAL NOT NULL,
                 PRIMARY KEY (ts, source_id)
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS minutes (
                 ts INTEGER NOT NULL,
                 source_id INTEGER NOT NULL,
                 joules REAL NOT NULL,
                 secs INTEGER NOT NULL,
                 PRIMARY KEY (ts, source_id)
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS proc_minutes (
                 ts INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 joules REAL NOT NULL,
                 PRIMARY KEY (ts, name)
             ) WITHOUT ROWID;",
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Inserts or updates sources and returns their ids in the same order.
    pub fn register_sources(&self, sources: &[SourceInfo]) -> Result<Vec<i64>> {
        let mut ids = Vec::with_capacity(sources.len());
        for s in sources {
            self.conn.execute(
                "INSERT INTO sources (key, label, kind) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET label = excluded.label, kind = excluded.kind",
                params![s.key, s.label, kind_str(s.kind)],
            )?;
            let id: i64 = self.conn.query_row(
                "SELECT id FROM sources WHERE key = ?1",
                params![s.key],
                |r| r.get(0),
            )?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub fn sources(&self) -> Result<Vec<StoredSource>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, key, label, kind FROM sources ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            let kind: String = r.get(3)?;
            Ok(StoredSource {
                id: r.get(0)?,
                info: SourceInfo {
                    key: r.get(1)?,
                    label: r.get(2)?,
                    kind: if kind == "gpu" {
                        SourceKind::Gpu
                    } else {
                        SourceKind::Cpu
                    },
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Writes one minute and drops raw samples older than `raw_cutoff`.
    pub fn flush(&mut self, batch: &MinuteBatch, raw_cutoff: i64) -> Result<()> {
        self.flush_all(std::slice::from_ref(batch), raw_cutoff)
    }

    /// Writes several minutes in one transaction.
    pub fn flush_all(&mut self, batches: &[MinuteBatch], raw_cutoff: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut s = tx.prepare_cached(
                "INSERT OR REPLACE INTO samples (ts, source_id, joules) VALUES (?1, ?2, ?3)",
            )?;
            // Merge, in case the daemon restarted within the same minute.
            let mut m = tx.prepare_cached(
                "INSERT INTO minutes (ts, source_id, joules, secs) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(ts, source_id) DO UPDATE SET
                     joules = joules + excluded.joules,
                     secs = MIN(60, secs + excluded.secs)",
            )?;
            let mut p = tx.prepare_cached(
                "INSERT INTO proc_minutes (ts, name, joules) VALUES (?1, ?2, ?3)
                 ON CONFLICT(ts, name) DO UPDATE SET joules = joules + excluded.joules",
            )?;
            for batch in batches {
                for (ts, id, j) in &batch.samples {
                    s.execute(params![ts, id, j])?;
                }
                for (id, j) in &batch.totals {
                    m.execute(params![batch.minute, id, j, batch.secs])?;
                }
                for (name, j) in &batch.procs {
                    p.execute(params![batch.minute, name, j])?;
                }
            }
            tx.execute("DELETE FROM samples WHERE ts < ?1", params![raw_cutoff])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Per-second buckets from raw samples in `[from, to)`.
    pub fn second_buckets(&self, from: i64, to: i64, bucket_secs: i64) -> Result<Vec<Bucket>> {
        let bucket_secs = bucket_secs.max(1);
        let mut stmt = self.conn.prepare_cached(
            "SELECT ts, source_id, joules FROM samples WHERE ts >= ?1 AND ts < ?2 ORDER BY ts",
        )?;
        let mut out: BTreeMap<i64, (Bucket, std::collections::BTreeSet<i64>)> = BTreeMap::new();
        let rows = stmt.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })?;
        for row in rows {
            let (ts, id, j) = row?;
            let b = ts.div_euclid(bucket_secs) * bucket_secs;
            let entry = out
                .entry(b)
                .or_insert_with(|| (Bucket::new(b), Default::default()));
            *entry.0.joules.entry(id).or_default() += j;
            entry.1.insert(ts);
        }
        Ok(out
            .into_values()
            .map(|(mut b, secs)| {
                b.secs = secs.len() as f64;
                b
            })
            .collect())
    }

    /// Buckets of `bucket_secs` (a multiple of 60) from minute rows in `[from, to)`.
    pub fn minute_buckets(&self, from: i64, to: i64, bucket_secs: i64) -> Result<Vec<Bucket>> {
        let bucket_secs = (bucket_secs.max(60) / 60) * 60;
        let mut stmt = self.conn.prepare_cached(
            "SELECT ts, source_id, joules, secs FROM minutes WHERE ts >= ?1 AND ts < ?2 ORDER BY ts",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        // Seconds are the same for every source of a minute, so count each minute once.
        let mut out: BTreeMap<i64, (Bucket, BTreeMap<i64, i64>)> = BTreeMap::new();
        for row in rows {
            let (ts, id, j, secs) = row?;
            let b = ts.div_euclid(bucket_secs) * bucket_secs;
            let entry = out
                .entry(b)
                .or_insert_with(|| (Bucket::new(b), BTreeMap::new()));
            *entry.0.joules.entry(id).or_default() += j;
            let s = entry.1.entry(ts).or_default();
            *s = (*s).max(secs);
        }
        Ok(out
            .into_values()
            .map(|(mut b, minutes)| {
                b.secs = minutes.values().sum::<i64>() as f64;
                b
            })
            .collect())
    }

    /// Joules per process name in `[from, to)`, largest first.
    pub fn proc_totals(&self, from: i64, to: i64) -> Result<Vec<(String, f64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT name, SUM(joules) AS j FROM proc_minutes WHERE ts >= ?1 AND ts < ?2
             GROUP BY name ORDER BY j DESC",
        )?;
        let rows = stmt.query_map(params![from, to], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Unix seconds of the oldest stored minute.
    pub fn first_minute(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT MIN(ts) FROM minutes", [], |r| r.get(0))?)
    }
}

fn kind_str(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Cpu => "cpu",
        SourceKind::Gpu => "gpu",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(key: &str, kind: SourceKind) -> SourceInfo {
        SourceInfo {
            key: key.into(),
            label: key.to_uppercase(),
            kind,
        }
    }

    #[test]
    fn flush_and_query_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("j.db");
        let mut store = Store::open_rw(&path).unwrap();
        let ids = store
            .register_sources(&[
                src("cpu", SourceKind::Cpu),
                src("nvidia:0", SourceKind::Gpu),
            ])
            .unwrap();
        let (cpu, gpu) = (ids[0], ids[1]);
        // Registering again keeps ids stable.
        assert_eq!(
            store
                .register_sources(&[src("cpu", SourceKind::Cpu)])
                .unwrap(),
            vec![cpu]
        );

        let mut batch = MinuteBatch {
            minute: 600,
            secs: 2,
            ..Default::default()
        };
        batch.samples = vec![
            (600, cpu, 50.0),
            (600, gpu, 200.0),
            (601, cpu, 60.0),
            (601, gpu, 210.0),
        ];
        batch.totals.insert(cpu, 110.0);
        batch.totals.insert(gpu, 410.0);
        batch.procs.insert("llama-server".into(), 300.0);
        batch.procs.insert("(idle)".into(), 20.0);
        store.flush(&batch, 0).unwrap();
        // Same minute again (daemon restart) adds up.
        let again = MinuteBatch {
            minute: 600,
            secs: 1,
            totals: [(cpu, 10.0)].into(),
            ..Default::default()
        };
        store.flush(&again, 0).unwrap();

        let ro = Store::open_ro(&path).unwrap();
        assert_eq!(ro.sources().unwrap().len(), 2);
        let secs = ro.second_buckets(600, 602, 1).unwrap();
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[0].secs, 1.0);
        assert_eq!(secs[1].joules[&cpu], 60.0);

        let mins = ro.minute_buckets(0, 3600, 900).unwrap();
        assert_eq!(mins.len(), 1);
        assert_eq!(mins[0].ts, 0);
        assert_eq!(mins[0].secs, 3.0);
        assert_eq!(mins[0].joules[&cpu], 120.0);
        assert_eq!(mins[0].measured(), 530.0);

        let procs = ro.proc_totals(0, 3600).unwrap();
        assert_eq!(procs[0], ("llama-server".to_string(), 300.0));
        assert_eq!(ro.first_minute().unwrap(), Some(600));
    }

    #[test]
    fn flush_prunes_old_samples() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::open_rw(&tmp.path().join("j.db")).unwrap();
        let id = store
            .register_sources(&[src("cpu", SourceKind::Cpu)])
            .unwrap()[0];
        let old = MinuteBatch {
            minute: 0,
            secs: 1,
            samples: vec![(0, id, 1.0)],
            ..Default::default()
        };
        store.flush(&old, 0).unwrap();
        let new = MinuteBatch {
            minute: 120,
            secs: 1,
            samples: vec![(120, id, 1.0)],
            ..Default::default()
        };
        store.flush(&new, 60).unwrap();
        assert_eq!(store.second_buckets(0, 1000, 1).unwrap().len(), 1);
    }
}
