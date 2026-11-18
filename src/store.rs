//! SQLite storage for samples, daily health and long-lived counters.
//!
//! WAL mode so the GUI can read while the daemon writes. The daemon is the only
//! writer; the GUI opens the same file read-only in practice.

use crate::model::{Reading, Status};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bumped whenever `SCHEMA` changes in a way that needs code to migrate.
pub const SCHEMA_VERSION: i64 = 1;

/// Raw samples are pruned after 30 days; `daily_health` is kept forever.
pub const RETENTION_DAYS: i64 = 30;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS samples (
  ts         INTEGER PRIMARY KEY,
  status     INTEGER NOT NULL,
  ac_online  INTEGER NOT NULL,
  energy_wh  REAL NOT NULL,
  full_wh    REAL NOT NULL,
  power_w    REAL,
  voltage_v  REAL,
  temp_c     REAL
);

CREATE TABLE IF NOT EXISTS daily_health (
  day            TEXT PRIMARY KEY,
  full_wh        REAL NOT NULL,
  design_wh      REAL NOT NULL,
  cycle_count    INTEGER,
  est_cycles     REAL
);

CREATE TABLE IF NOT EXISTS meta (
  k TEXT PRIMARY KEY,
  v TEXT NOT NULL
);
"#;

/// One row of the health trend.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyHealth {
    /// Local date, `YYYY-MM-DD`.
    pub day: String,
    pub full_wh: f64,
    pub design_wh: f64,
    pub cycle_count: Option<u32>,
    pub est_cycles: Option<f64>,
}

impl DailyHealth {
    pub fn health_pct(&self) -> Option<f64> {
        (self.design_wh > 0.0).then(|| self.full_wh / self.design_wh * 100.0)
    }
}

/// One raw sample as read back from the database.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub ts: i64,
    pub status: Status,
    pub ac_online: bool,
    pub energy_wh: f64,
    pub full_wh: f64,
    pub power_w: Option<f64>,
    pub voltage_v: Option<f64>,
    pub temp_c: Option<f64>,
}

impl Sample {
    /// Charge level in percent, clamped the same way [`Reading::level_pct`] does.
    pub fn level_pct(&self) -> f64 {
        if self.full_wh > 0.0 {
            (self.energy_wh / self.full_wh * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        }
    }

    /// Rebuild a [`Reading`] so history can be replayed into the estimators.
    pub fn to_reading(&self) -> Reading {
        Reading {
            ts: self.ts,
            status: self.status,
            ac_online: Some(self.ac_online),
            energy_wh: Some(self.energy_wh),
            full_wh: Some(self.full_wh),
            design_wh: None,
            power_w: self.power_w,
            voltage_v: self.voltage_v,
            cycle_count: None,
            temp_c: self.temp_c.map(|temp| temp as f32),
            charge_limit_pct: None,
            capacity_pct: None,
        }
    }
}

/// Handle on the Battmon database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the database at `path`, with WAL enabled.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;

        // WAL lets the GUI read while the daemon writes. This returns a row
        // with the resulting mode, so it needs query_row rather than execute.
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .context("enabling WAL")?;
        if !mode.eq_ignore_ascii_case("wal") {
            eprintln!("battmon: warning: journal_mode is {mode}, not wal");
        }

        Self::init(conn)
    }

    /// In-memory database for tests. WAL is not available there.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Open an existing database without the ability to write it, which is what
    /// the GUI does. Fails with a readable message when the daemon has not
    /// created it yet.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("opening database {} read-only", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("setting busy timeout")?;

        let has_samples: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'samples')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !has_samples {
            bail!("{} has no samples table yet", path.display());
        }

        let store = Self { conn };
        let version = store.schema_version()?;
        if version > SCHEMA_VERSION {
            bail!("database schema {version} is newer than this build ({SCHEMA_VERSION})");
        }
        Ok(store)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))
            .context("setting busy timeout")?;
        // NORMAL is durable enough for a sampler and avoids an fsync per sample.
        conn.execute_batch("PRAGMA synchronous=NORMAL;")
            .context("setting synchronous")?;
        conn.execute_batch(SCHEMA).context("creating schema")?;

        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        match self.meta("schema_version")? {
            None => {
                self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
            }
            Some(raw) => {
                let version: i64 = raw
                    .parse()
                    .with_context(|| format!("schema_version is not a number: {raw}"))?;
                if version > SCHEMA_VERSION {
                    bail!(
                        "database schema version {version} is newer than this build ({SCHEMA_VERSION}); \
                         upgrade battmon"
                    );
                }
                // Future migrations go here, one `if version < N` block each.
            }
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .meta("schema_version")?
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(0))
    }

    /// Store one sample.
    ///
    /// Returns `false` when the reading lacks the Wh values the schema requires;
    /// that is a normal condition on machines with an unreadable battery node.
    pub fn insert_sample(&self, reading: &Reading) -> Result<bool> {
        let (Some(energy_wh), Some(full_wh)) = (reading.energy_wh, reading.full_wh) else {
            return Ok(false);
        };

        self.conn
            .execute(
                "INSERT OR REPLACE INTO samples
                   (ts, status, ac_online, energy_wh, full_wh, power_w, voltage_v, temp_c)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    reading.ts,
                    reading.status.as_i64(),
                    i64::from(reading.ac_online.unwrap_or(false)),
                    energy_wh,
                    full_wh,
                    reading.power_w,
                    reading.voltage_v,
                    reading.temp_c.map(f64::from),
                ],
            )
            .with_context(|| format!("inserting sample at {}", reading.ts))?;

        Ok(true)
    }

    pub fn sample_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM samples", [], |row| row.get(0))?)
    }

    /// Raw samples at or after `since`, oldest first. Charts and the estimator
    /// seed use this; gaps are preserved rather than filled.
    pub fn samples_since(&self, since: i64) -> Result<Vec<Sample>> {
        let mut statement = self.conn.prepare(
            "SELECT ts, status, ac_online, energy_wh, full_wh, power_w, voltage_v, temp_c
             FROM samples WHERE ts >= ?1 ORDER BY ts",
        )?;

        let rows = statement.query_map(params![since], |row| {
            Ok(Sample {
                ts: row.get(0)?,
                status: Status::from_i64(row.get(1)?),
                ac_online: row.get::<_, i64>(2)? != 0,
                energy_wh: row.get(3)?,
                full_wh: row.get(4)?,
                power_w: row.get(5)?,
                voltage_v: row.get(6)?,
                temp_c: row.get(7)?,
            })
        })?;

        let mut samples = Vec::new();
        for row in rows {
            samples.push(row?);
        }
        Ok(samples)
    }

    /// Delete raw samples older than `keep_days` before ` now`.
    pub fn prune(&self, keep_days: i64, now: i64) -> Result<usize> {
        let cutoff = now - keep_days * 86_400;
        let removed = self
            .conn
            .execute("DELETE FROM samples WHERE ts < ?1", params![cutoff])
            .context("pruning samples")?;
        Ok(removed)
    }

    /// Upsert today's health row. Cheap enough to call whenever the day rolls over.
    pub fn upsert_daily_health(
        &self,
        day: &str,
        full_wh: f64,
        design_wh: f64,
        cycle_count: Option<u32>,
        est_cycles: Option<f64>,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO daily_health (day, full_wh, design_wh, cycle_count, est_cycles)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(day) DO UPDATE SET
                   full_wh     = excluded.full_wh,
                   design_wh   = excluded.design_wh,
                   cycle_count = excluded.cycle_count,
                   est_cycles  = excluded.est_cycles",
                params![
                    day,
                    full_wh,
                    design_wh,
                    cycle_count.map(i64::from),
                    est_cycles
                ],
            )
            .context("upserting daily health")?;
        Ok(())
    }

    /// Full health history, oldest first.
    pub fn daily_health(&self) -> Result<Vec<DailyHealth>> {
        let mut stmt = self.conn.prepare(
            "SELECT day, full_wh, design_wh, cycle_count, est_cycles
             FROM daily_health ORDER BY day",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DailyHealth {
                day: row.get(0)?,
                full_wh: row.get(1)?,
                design_wh: row.get(2)?,
                cycle_count: row
                    .get::<_, Option<i64>>(3)?
                    .and_then(|count| u32::try_from(count).ok()),
                est_cycles: row.get(4)?,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT v FROM meta WHERE k = ?1", params![key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO meta (k, v) VALUES (?1, ?2)
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                params![key, value],
            )
            .with_context(|| format!("writing meta key {key}"))?;
        Ok(())
    }

    /// Only writes when the key is absent; used for `tracking_since`.
    pub fn ensure_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO meta (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO NOTHING",
                params![key, value],
            )
            .with_context(|| format!("initialising meta key {key}"))?;
        Ok(())
    }

    pub fn meta_f64(&self, key: &str) -> Result<Option<f64>> {
        Ok(self.meta(key)?.and_then(|raw| raw.parse().ok()))
    }

    pub fn set_meta_f64(&self, key: &str, value: f64) -> Result<()> {
        self.set_meta(key, &format!("{value:.6}"))
    }

    /// Add to the lifetime discharge counter used for the cycle estimate.
    pub fn add_cumulative_discharge_wh(&self, delta_wh: f64) -> Result<()> {
        if !delta_wh.is_finite() || delta_wh <= 0.0 {
            return Ok(());
        }
        let total = self
            .meta_f64("cumulative_discharge_wh")?
            .unwrap_or(0.0)
            .max(0.0);
        self.set_meta_f64("cumulative_discharge_wh", total + delta_wh)
    }

    pub fn cumulative_discharge_wh(&self) -> Result<f64> {
        Ok(self
            .meta_f64("cumulative_discharge_wh")?
            .unwrap_or(0.0)
            .max(0.0))
    }
}

/// `$XDG_DATA_HOME/battmon`, else `~/.local/share/battmon`.
pub fn data_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            PathBuf::from(home).join(".local").join("share")
        }
    };
    Ok(base.join("battmon"))
}

/// Default database location.
pub fn default_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("battmon.db"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    fn reading(ts: i64, energy: f64, status: Status) -> Reading {
        Reading {
            ts,
            status,
            ac_online: Some(status.is_on_ac()),
            energy_wh: Some(energy),
            full_wh: Some(30.0),
            design_wh: Some(42.0),
            power_w: Some(8.0),
            voltage_v: Some(12.0),
            cycle_count: None,
            temp_c: None,
            charge_limit_pct: None,
            capacity_pct: None,
        }
    }

    #[test]
    fn inserts_and_counts_samples() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        assert!(store.insert_sample(&reading(100, 20.0, Status::Discharging)).unwrap());
        assert!(store.insert_sample(&reading(160, 19.5, Status::Discharging)).unwrap());
        assert_eq!(store.sample_count().unwrap(), 2);
    }

    #[test]
    fn duplicate_timestamp_replaces_instead_of_failing() {
        let store = Store::open_in_memory().unwrap();
        store.insert_sample(&reading(100, 20.0, Status::Discharging)).unwrap();
        store.insert_sample(&reading(100, 21.0, Status::Charging)).unwrap();
        assert_eq!(store.sample_count().unwrap(), 1);

        let (energy, status): (f64, i64) = store
            .conn
            .query_row("SELECT energy_wh, status FROM samples", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(energy, 21.0);
        assert_eq!(status, Status::Charging.as_i64());
    }

    #[test]
    fn unstorable_reading_writes_nothing() {
        let store = Store::open_in_memory().unwrap();
        let mut r = reading(100, 20.0, Status::Discharging);
        r.full_wh = None;
        assert!(!store.insert_sample(&r).unwrap());
        assert_eq!(store.sample_count().unwrap(), 0);
    }

    #[test]
    fn prunes_only_old_samples() {
        let store = Store::open_in_memory().unwrap();
        let now = 1_000 * 86_400;
        store.insert_sample(&reading(now - 40 * 86_400, 20.0, Status::Discharging)).unwrap();
        store.insert_sample(&reading(now - 1 * 86_400, 20.0, Status::Discharging)).unwrap();

        assert_eq!(store.prune(RETENTION_DAYS, now).unwrap(), 1);
        assert_eq!(store.sample_count().unwrap(), 1);
    }

    #[test]
    fn cumulative_discharge_accumulates_and_ignores_junk() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.cumulative_discharge_wh().unwrap(), 0.0);

        store.add_cumulative_discharge_wh(1.5).unwrap();
        store.add_cumulative_discharge_wh(2.0).unwrap();
        store.add_cumulative_discharge_wh(-3.0).unwrap();
        store.add_cumulative_discharge_wh(f64::NAN).unwrap();
        assert_eq!(store.cumulative_discharge_wh().unwrap(), 3.5);
    }

    #[test]
    fn daily_health_upserts_one_row_per_day() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_daily_health("2026-11-18", 30.9, 42.0, Some(333), Some(12.0))
            .unwrap();
        store
            .upsert_daily_health("2026-11-18", 31.0, 42.0, Some(333), Some(12.0))
            .unwrap();
        store
            .upsert_daily_health("2026-11-19", 31.1, 42.0, None, Some(12.5))
            .unwrap();

        let history = store.daily_health().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].day, "2026-11-18");
        assert_eq!(history[0].full_wh, 31.0);
        assert!(history[1].cycle_count.is_none());
        assert!((history[0].health_pct().unwrap() - 73.8).abs() < 0.1);
    }

    #[test]
    fn meta_round_trips() {
        let store = Store::open_in_memory().unwrap();
        store.set_meta("tracking_since", "1000").unwrap();
        store.ensure_meta("tracking_since", "2000").unwrap();
        assert_eq!(store.meta("tracking_since").unwrap().as_deref(), Some("1000"));
        assert_eq!(store.meta("absent").unwrap(), None);
    }

    #[test]
    fn file_database_is_in_wal_mode() {
        let path = std::env::temp_dir().join(format!(
            "battmon-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        {
            let store = Store::open(&path).unwrap();
            let mode: String = store
                .conn
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            store
                .insert_sample(&reading(100, 20.0, Status::Discharging))
                .unwrap();
        }

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn samples_since_returns_rows_in_order() {
        let store = Store::open_in_memory().unwrap();
        for ts in [300, 100, 200] {
            store
                .insert_sample(&reading(ts, 20.0, Status::Discharging))
                .unwrap();
        }

        let rows = store.samples_since(150).unwrap();
        assert_eq!(
            rows.iter().map(|sample| sample.ts).collect::<Vec<_>>(),
            vec![200, 300]
        );
        assert_eq!(rows[0].to_reading().status, Status::Discharging);
        assert_eq!(rows[0].to_reading().energy_wh, Some(20.0));
    }

    #[test]
    fn read_only_store_cannot_write() {
        let path = std::env::temp_dir().join(format!(
            "battmon-test-ro-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        std::fs::write(&path, b"").unwrap();
        assert!(
            Store::open_read_only(&path).is_err(),
            "a database without a schema must be refused"
        );

        {
            let store = Store::open(&path).unwrap();
            store
                .insert_sample(&reading(100, 20.0, Status::Discharging))
                .unwrap();
        }

        let store = Store::open_read_only(&path).unwrap();
        assert_eq!(store.sample_count().unwrap(), 1);
        assert!(
            store
                .insert_sample(&reading(200, 19.0, Status::Discharging))
                .is_err()
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn newer_schema_is_refused() {
        let path = std::env::temp_dir().join(format!(
            "battmon-test-newer-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        {
            let store = Store::open(&path).unwrap();
            store.set_meta("schema_version", "999").unwrap();
        }
        assert!(Store::open(&path).is_err());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}
