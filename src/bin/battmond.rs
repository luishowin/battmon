//! Battmon sampler daemon.
//!
//! M1 scope: `battmond --once` prints one normalised reading.

use anyhow::{Context, Result, bail};
use battmon::model::Reading;
use battmon::sysfs::Sysfs;

const USAGE: &str = "\
battmond - Battmon battery sampler

USAGE:
    battmond --once    print one normalised reading and exit
    battmond --help    show this help

Reads /sys (or $BATTMON_SYSFS_ROOT) instead of UPower.
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--once") => once(),
        Some("--help" | "-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown argument `{other}`\n\n{USAGE}"),
        None => bail!("the sampler loop arrives in milestone 2; use `battmond --once` for now"),
    }
}

fn once() -> Result<()> {
    let sysfs = Sysfs::from_env();
    let battery = sysfs.battery().with_context(|| {
        format!(
            "no battery found under {}",
            sysfs.power_supply_dir().display()
        )
    })?;

    let ts = unix_now()?;
    let reading = battery.read(ts, sysfs.ac_online());

    print!("{}", render(&reading, &battery.info));
    Ok(())
}

fn unix_now() -> Result<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the unix epoch")?;
    i64::try_from(now.as_secs()).context("system clock is out of range")
}

fn render(reading: &Reading, info: &battmon::model::BatteryInfo) -> String {
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
