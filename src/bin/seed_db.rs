//! Generate a realistic week of samples for chart work.
//!
//! Dev tool only (not installed). Requires an explicit `--db` so it can never
//! be pointed at the real database by accident.
//!
//! ```text
//! cargo run --bin seed_db -- --db /tmp/battmon-seed.db --days 7 --reset
//! ```

use anyhow::{Context, Result, bail};
use battmon::format;
use battmon::model::{Reading, Status};
use battmon::store::Store;
use std::path::PathBuf;

const DESIGN_WH: f64 = 42.042;
const STEP_SECS: i64 = 60;

fn main() -> Result<()> {
    let options = Options::parse(std::env::args().skip(1))?;

    if options.reset {
        let _ = std::fs::remove_file(&options.db);
        let _ = std::fs::remove_file(options.db.with_extension("db-wal"));
        let _ = std::fs::remove_file(options.db.with_extension("db-shm"));
    }

    let store = Store::open(&options.db)
        .with_context(|| format!("opening {}", options.db.display()))?;
    let now = format::now_secs();

    let today_midnight = now - format::local_seconds_of_day(now)?;
    let first_midnight = today_midnight - (options.days - 1) * 86_400;
    store.ensure_meta("tracking_since", &first_midnight.to_string())?;

    let mut written = 0_usize;
    let mut last_energy = 0.0_f64;

    for day in 0..options.days {
        let midnight = first_midnight + day * 86_400;
        // Capacity fades very slightly so the health sparkline has a trend.
        let full_wh = 30.99 - day as f64 * 0.02;
        let start_level = 0.97;

        let mut energy_wh = full_wh * start_level;

        // 07:00-16:00 discharging, 16:00-18:00 charging, 18:00-23:00 held.
        for slot in 0..(16 * 60) {
            let offset = 7 * 3_600 + slot * STEP_SECS;
            if offset >= 23 * 3_600 {
                break;
            }
            let ts = midnight + offset;
            if ts > now {
                break;
            }

            let hour = (offset / 3_600) % 24;
            let (status, power_w) = if (7..16).contains(&hour) {
                // Light load with a slow wobble and a lunchtime spike.
                let wobble = ((slot as f64 / 11.0).sin() + 1.0) * 0.5;
                let spike = if (300..312).contains(&slot) { 8.0 } else { 0.0 };
                (Status::Discharging, 3.0 + wobble + spike)
            } else if (16..18).contains(&hour) {
                let level = energy_wh / full_wh;
                let taper = if level > 0.9 {
                    (1.0 - (level - 0.9) * 6.0).clamp(0.15, 1.0)
                } else {
                    1.0
                };
                (Status::Charging, 19.5 * taper)
            } else {
                (Status::Full, 0.0)
            };

            let delta_wh = power_w * STEP_SECS as f64 / 3_600.0;
            match status {
                Status::Charging => energy_wh = (energy_wh + delta_wh).min(full_wh),
                Status::Discharging => energy_wh = (energy_wh - delta_wh).max(0.5),
                _ => {}
            }

            let reading = Reading {
                ts,
                status,
                ac_online: Some(status.is_on_ac()),
                energy_wh: Some(energy_wh),
                full_wh: Some(full_wh),
                design_wh: Some(DESIGN_WH),
                power_w: Some(power_w),
                voltage_v: Some(11.8),
                cycle_count: Some(333),
                temp_c: None,
                charge_limit_pct: None,
                capacity_pct: Some((energy_wh / full_wh * 100.0).round() as u8),
            };

            if store.insert_sample(&reading)? {
                written += 1;
                last_energy = energy_wh;
            }
        }

        store.upsert_daily_health(
            &format::local_day(midnight + 12 * 3_600)?,
            full_wh,
            DESIGN_WH,
            Some(333),
            None,
        )?;
    }

    // Keep the cycle estimate coherent with the seeded discharge.
    store.set_meta_f64("cumulative_discharge_wh", options.days as f64 * 18.0)?;

    println!(
        "seeded {written} samples over {} days into {} (last level {:.1}%)",
        options.days,
        options.db.display(),
        last_energy / 30.99 * 100.0
    );
    Ok(())
}

struct Options {
    db: PathBuf,
    days: i64,
    reset: bool,
}

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut db = None;
        let mut days = 7;
        let mut reset = false;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--db" => db = Some(PathBuf::from(args.next().context("--db needs a path")?)),
                "--days" => {
                    days = args
                        .next()
                        .context("--days needs a number")?
                        .parse()
                        .context("--days expects a whole number")?;
                }
                "--reset" => reset = true,
                "--help" | "-h" => {
                    println!("seed_db --db PATH [--days N] [--reset]");
                    std::process::exit(0);
                }
                other => bail!("unknown argument `{other}`"),
            }
        }

        let db = db.context("--db is required: this tool must never touch the real database")?;
        if !(1..=30).contains(&days) {
            bail!("--days must be between 1 and 30");
        }
        Ok(Self { db, days, reset })
    }
}
