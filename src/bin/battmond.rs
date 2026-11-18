//! Battmon sampler daemon.
//!
//! Samples sysfs every few seconds and persists a row every minute, plus one
//! immediately whenever the battery status or wall power changes.

use anyhow::{Context, Result, bail};
use battmon::model::{BatteryInfo, Reading, Status};
use battmon::store::{RETENTION_DAYS, Store, default_db_path};
use battmon::sysfs::Sysfs;
use signal_hook::consts::{SIGINT, SIGTERM};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Cheap sysfs poll interval.
const DEFAULT_TICK_SECS: u64 = 10;
/// How often a sample reaches SQLite when nothing changes.
const DEFAULT_PERSIST_SECS: i64 = 60;
/// A longer gap between persisted samples means suspend or a clock jump, so the
/// energy delta across it must not be counted as discharge.
const MAX_GAP_SECS: i64 = 300;

const USAGE: &str = "\
battmond - Battmon battery sampler

USAGE:
    battmond                 run the sampler until SIGTERM
    battmond --once          print one normalised reading and exit
    battmond --help          show this help

OPTIONS:
    --db PATH                database path (default: $XDG_DATA_HOME/battmon/battmon.db)
    --tick SECONDS           sysfs poll interval (default: 10)
    --persist SECONDS        sample interval (default: 60)

Reads /sys (or $BATTMON_SYSFS_ROOT) instead of UPower.
";

fn main() -> Result<()> {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{USAGE}");
            return Ok(());
        }
        Err(err) => {
            eprintln!("battmond: {err:#}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    if options.once {
        return once();
    }
    run_daemon(&options)
}

#[derive(Debug, Clone)]
struct Options {
    once: bool,
    db: Option<PathBuf>,
    tick: u64,
    persist: i64,
    help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            once: false,
            db: None,
            tick: DEFAULT_TICK_SECS,
            persist: DEFAULT_PERSIST_SECS,
            help: false,
        }
    }
}

impl Options {
    /// Returns `Ok(None)` when help was requested.
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>> {
        let mut options = Self::default();
        let mut args = args.peekable();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--once" => options.once = true,
                "--help" | "-h" => options.help = true,
                "--db" => {
                    let path = args.next().context("--db needs a path")?;
                    options.db = Some(PathBuf::from(path));
                }
                "--tick" => {
                    let value = args.next().context("--tick needs a number")?;
                    options.tick = parse_secs(&value, "--tick")?;
                }
                "--persist" => {
                    let value = args.next().context("--persist needs a number")?;
                    options.persist = parse_secs(&value, "--persist")? as i64;
                }
                other if other.starts_with("--db=") => {
                    options.db = Some(PathBuf::from(&other["--db=".len()..]));
                }
                other if other.starts_with("--tick=") => {
                    options.tick = parse_secs(&other["--tick=".len()..], "--tick")?;
                }
                other if other.starts_with("--persist=") => {
                    options.persist = parse_secs(&other["--persist=".len()..], "--persist")? as i64;
                }
                other => bail!("unknown argument `{other}`"),
            }
        }

        if options.tick == 0 || options.persist == 0 {
            bail!("--tick and --persist must be at least 1 second");
        }
        if options.help {
            return Ok(None);
        }
        Ok(Some(options))
    }
}

fn parse_secs(value: &str, flag: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .with_context(|| format!("{flag} expects whole seconds, got `{value}`"))
}

fn once() -> Result<()> {
    let sysfs = Sysfs::from_env();
    let battery = sysfs.battery().with_context(|| {
        format!(
            "no battery found under {}",
            sysfs.power_supply_dir().display()
        )
    })?;

    let reading = battery.read(unix_now()?, sysfs.ac_online());
    print!("{}", render(&reading, &battery.info));
    Ok(())
}

fn run_daemon(options: &Options) -> Result<()> {
    let sysfs = Sysfs::from_env();
    let battery = sysfs
        .battery()
        .with_context(|| format!("no battery found under {}", sysfs.power_supply_dir().display()))?;

    let db_path = match &options.db {
        Some(path) => path.clone(),
        None => default_db_path()?,
    };
    let store = Store::open(&db_path)
        .with_context(|| format!("opening database {}", db_path.display()))?;

    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGTERM, Arc::clone(&stop))
        .context("installing SIGTERM handler")?;
    signal_hook::flag::register(SIGINT, Arc::clone(&stop)).context("installing SIGINT handler")?;

    store.ensure_meta("tracking_since", &unix_now()?.to_string())?;

    eprintln!(
        "battmond: sampling {} every {}s into {} (persist every {}s)",
        battery.info.name,
        options.tick,
        db_path.display(),
        options.persist
    );

    let mut sampler = Sampler::default();
    let mut next_tick = 0;

    loop {
        let now = unix_now()?;
        if now >= next_tick {
            let reading = battery.read(now, sysfs.ac_online());
            if let Err(err) = sampler.tick(&store, &reading, options.persist) {
                // A transient database error must not kill the sampler.
                eprintln!("battmond: sample rejected: {err:#}");
            }
            next_tick = now + options.tick as i64;
        }

        if stop.load(Ordering::Relaxed) {
            break;
        }

        let now = unix_now()?;
        let wait_ms = (next_tick - now).clamp(0, 1_000) as u64;
        std::thread::sleep(Duration::from_millis(wait_ms.max(1)));
    }

    // Final flush so stopping the service never loses the last few seconds.
    let reading = battery.read(unix_now()?, sysfs.ac_online());
    if let Err(err) = sampler.tick_forced(&store, &reading) {
        eprintln!("battmond: final flush failed: {err:#}");
    }
    eprintln!("battmond: stopped");
    Ok(())
}

/// Decides when a reading becomes a database row, and tracks the energy that
/// has flowed out of the battery for the cycle estimate.
#[derive(Debug, Default)]
struct Sampler {
    last: Option<Reading>,
    health_day: Option<String>,
}

impl Sampler {
    /// Persist when due, when the status/AC state flips, or on the very first
    /// tick. Does nothing when the reading lacks the Wh values the schema needs.
    fn tick(&mut self, store: &Store, reading: &Reading, persist_secs: i64) -> Result<bool> {
        let changed = self.last.as_ref().is_none_or(|previous| {
            previous.status != reading.status || previous.ac_online != reading.ac_online
        });
        let due = self.last.as_ref().is_none_or(|previous| {
            // A clock that jumped backwards also forces a row.
            reading.ts - previous.ts >= persist_secs || reading.ts < previous.ts
        });

        if !(changed || due) {
            return Ok(false);
        }
        self.persist(store, reading)
    }

    /// Persist unconditionally (shutdown flush), subject to the schema needs.
    fn tick_forced(&mut self, store: &Store, reading: &Reading) -> Result<bool> {
        self.persist(store, reading)
    }

    fn persist(&mut self, store: &Store, reading: &Reading) -> Result<bool> {
        if !reading.is_storable() {
            return Ok(false);
        }
        if !store.insert_sample(reading)? {
            return Ok(false);
        }

        self.accumulate_discharge(store, reading)?;
        self.last = Some(reading.clone());
        self.refresh_daily_health(store, reading)?;
        Ok(true)
    }

    /// Sum the energy lost between two consecutive discharging samples.
    /// Gaps longer than [`MAX_GAP_SECS`] (suspend, clock jump) are skipped.
    fn accumulate_discharge(&self, store: &Store, reading: &Reading) -> Result<()> {
        let Some(previous) = &self.last else {
            return Ok(());
        };
        if previous.status != Status::Discharging || reading.status != Status::Discharging {
            return Ok(());
        }

        let delta_secs = reading.ts - previous.ts;
        if !(1..=MAX_GAP_SECS).contains(&delta_secs) {
            return Ok(());
        }

        let (Some(before), Some(after)) = (previous.energy_wh, reading.energy_wh) else {
            return Ok(());
        };
        let dropped = before - after;
        if dropped.is_finite() && dropped > 0.0 {
            store.add_cumulative_discharge_wh(dropped)?;
        }
        Ok(())
    }

    /// Write today's health row (and prune) once per local day.
    fn refresh_daily_health(&mut self, store: &Store, reading: &Reading) -> Result<()> {
        let day = battmon::format::local_day(reading.ts)?;
        if self.health_day.as_deref() == Some(day.as_str()) {
            return Ok(());
        }

        let (Some(full_wh), Some(design_wh)) = (reading.full_wh, reading.design_wh) else {
            return Ok(());
        };

        let cumulative = store.cumulative_discharge_wh()?;
        let estimated = (design_wh > 0.0).then(|| cumulative / design_wh);
        store.upsert_daily_health(&day, full_wh, design_wh, reading.cycle_count, estimated)?;

        match store.prune(RETENTION_DAYS, reading.ts) {
            Ok(0) => {}
            Ok(removed) => eprintln!("battmond: pruned {removed} samples older than {RETENTION_DAYS} days"),
            Err(err) => eprintln!("battmond: prune failed: {err:#}"),
        }

        self.health_day = Some(day);
        Ok(())
    }
}

fn unix_now() -> Result<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the unix epoch")?;
    i64::try_from(now.as_secs()).context("system clock is out of range")
}

fn render(reading: &Reading, info: &BatteryInfo) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", info.display_name()));
    out.push_str(&format!("{:<13}{}\n", "node", info.name));
    out.push_str(&format!("{:<13}{}\n", "status", reading.status.label()));
    out.push_str(&format!("{:<13}{}\n", "ac_online", yes_no(reading.ac_online)));
    out.push_str(&format!("{:<13}{}\n", "level", pct(reading.level_pct(), 1)));
    out.push_str(&format!("{:<13}{}\n", "energy", wh(reading.energy_wh)));
    out.push_str(&format!("{:<13}{}\n", "full", wh(reading.full_wh)));
    out.push_str(&format!("{:<13}{}\n", "design", wh(reading.design_wh)));
    out.push_str(&format!("{:<13}{}\n", "health", pct(reading.health_pct(), 1)));
    out.push_str(&format!("{:<13}{}\n", "power", watt(reading.power_w)));
    out.push_str(&format!(
        "{:<13}{}\n",
        "rate",
        pct(reading.rate_pct_per_h(), 1).replace("%", "%/h")
    ));
    out.push_str(&format!(
        "{:<13}{}\n",
        "voltage",
        match reading.voltage_v {
            Some(volts) => format!("{volts:.2} V"),
            None => "n/a".to_owned(),
        }
    ));
    out.push_str(&format!(
        "{:<13}{}\n",
        "cycles",
        match reading.cycle_count {
            Some(count) => format!("{count} (firmware)"),
            None => "n/a".to_owned(),
        }
    ));
    out.push_str(&format!(
        "{:<13}{}\n",
        "temperature",
        match reading.temp_c {
            Some(celsius) => format!("{celsius:.1} °C"),
            None => "n/a".to_owned(),
        }
    ));
    out.push_str(&format!(
        "{:<13}{}\n",
        "charge_limit",
        match reading.charge_limit_pct {
            Some(limit) => format!("{limit} %"),
            None => "n/a".to_owned(),
        }
    ));
    out.push_str(&format!(
        "{:<13}{}\n",
        "capacity",
        match reading.capacity_pct {
            Some(capacity) => format!("{capacity} %"),
            None => "n/a".to_owned(),
        }
    ));
    out.push_str(&format!("{:<13}{}\n", "ts", reading.ts));
    out
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "n/a",
    }
}

fn pct(value: Option<f64>, decimals: usize) -> String {
    match value {
        Some(value) => format!("{value:.decimals$} %"),
        None => "n/a".to_owned(),
    }
}

fn wh(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.2} Wh"),
        None => "n/a".to_owned(),
    }
}

fn watt(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.2} W"),
        None => "n/a".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(ts: i64, energy_wh: f64, status: Status, ac: bool) -> Reading {
        Reading {
            ts,
            status,
            ac_online: Some(ac),
            energy_wh: Some(energy_wh),
            full_wh: Some(31.0),
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
    fn writes_immediately_then_waits_for_the_interval() {
        let store = Store::open_in_memory().unwrap();
        let mut sampler = Sampler::default();

        assert!(sampler.tick(&store, &reading(1_000, 25.0, Status::Discharging, false), 60).unwrap());
        assert!(!sampler.tick(&store, &reading(1_010, 24.9, Status::Discharging, false), 60).unwrap());
        assert!(sampler.tick(&store, &reading(1_060, 24.7, Status::Discharging, false), 60).unwrap());
        assert_eq!(store.sample_count().unwrap(), 2);
    }

    #[test]
    fn status_flip_writes_immediately() {
        let store = Store::open_in_memory().unwrap();
        let mut sampler = Sampler::default();

        sampler.tick(&store, &reading(1_000, 25.0, Status::Discharging, false), 60).unwrap();
        // Only five seconds later, but AC came on: must be persisted.
        assert!(sampler.tick(&store, &reading(1_005, 25.0, Status::Charging, true), 60).unwrap());
        assert_eq!(store.sample_count().unwrap(), 2);
    }

    #[test]
    fn discharge_accumulates_between_short_gaps() {
        let store = Store::open_in_memory().unwrap();
        let mut sampler = Sampler::default();

        sampler.tick(&store, &reading(1_000, 25.0, Status::Discharging, false), 60).unwrap();
        sampler.tick(&store, &reading(1_060, 24.0, Status::Discharging, false), 60).unwrap();
        assert!((store.cumulative_discharge_wh().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn suspend_gap_is_not_counted_as_discharge() {
        let store = Store::open_in_memory().unwrap();
        let mut sampler = Sampler::default();

        sampler.tick(&store, &reading(1_000, 25.0, Status::Discharging, false), 60).unwrap();
        // Six hours later: the battery really did drain, but we cannot attribute
        // it to the tracked window.
        sampler.tick(&store, &reading(1_000 + 6 * 3_600, 10.0, Status::Discharging, false), 60).unwrap();
        assert_eq!(store.cumulative_discharge_wh().unwrap(), 0.0);
    }

    #[test]
    fn charging_never_accumulates() {
        let store = Store::open_in_memory().unwrap();
        let mut sampler = Sampler::default();

        sampler.tick(&store, &reading(1_000, 20.0, Status::Charging, true), 60).unwrap();
        // Energy went up; the signed drop is negative anyway.
        sampler.tick(&store, &reading(1_060, 22.0, Status::Charging, true), 60).unwrap();
        assert_eq!(store.cumulative_discharge_wh().unwrap(), 0.0);
    }

    #[test]
    fn clock_jump_backwards_forces_a_row_and_skips_accumulation() {
        let store = Store::open_in_memory().unwrap();
        let mut sampler = Sampler::default();

        sampler.tick(&store, &reading(10_000, 25.0, Status::Discharging, false), 60).unwrap();
        assert!(sampler.tick(&store, &reading(9_000, 24.0, Status::Discharging, false), 60).unwrap());
        assert_eq!(store.sample_count().unwrap(), 2);
        assert_eq!(store.cumulative_discharge_wh().unwrap(), 0.0);
    }

    #[test]
    fn unstorable_reading_is_retried_next_tick() {
        let store = Store::open_in_memory().unwrap();
        let mut sampler = Sampler::default();

        let mut broken = reading(1_000, 25.0, Status::Discharging, false);
        broken.energy_wh = None;
        assert!(!sampler.tick(&store, &broken, 60).unwrap());

        assert!(sampler.tick(&store, &reading(1_010, 25.0, Status::Discharging, false), 60).unwrap());
        assert_eq!(store.sample_count().unwrap(), 1);
    }

    #[test]
    fn daily_health_is_written_once_per_local_day() {
        let store = Store::open_in_memory().unwrap();
        let mut sampler = Sampler::default();

        sampler.tick(&store, &reading(1_000, 25.0, Status::Discharging, false), 60).unwrap();
        sampler.tick(&store, &reading(1_060, 24.5, Status::Discharging, false), 60).unwrap();
        // Same local day in every timezone (1000 and 1060 are 16 minutes apart).
        assert_eq!(store.daily_health().unwrap().len(), 1);
    }

    #[test]
    fn local_day_is_iso_dated() {
        let day = battmon::format::local_day(1_765_000_000).unwrap();
        assert_eq!(day.len(), 10);
        assert_eq!(&day[4..5], "-");
        assert_eq!(&day[7..8], "-");
    }

    #[test]
    fn options_reject_zero_and_unknown_flags() {
        assert!(Options::parse(["--tick".to_owned(), "0".to_owned()].into_iter()).is_err());
        assert!(Options::parse(["--bogus".to_owned()].into_iter()).is_err());
        assert!(Options::parse(["--help".to_owned()].into_iter()).unwrap().is_none());

        let options = Options::parse(
            ["--tick=5".to_owned(), "--db=/tmp/x.db".to_owned()].into_iter(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(options.tick, 5);
        assert_eq!(options.db.unwrap(), PathBuf::from("/tmp/x.db"));
    }
}
