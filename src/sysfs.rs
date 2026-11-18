//! The only module that touches `/sys`.
//!
//! Everything is read through a [`Sysfs`] instance that owns a root path, so
//! tests can point it at `tests/fixtures/<name>/sys` without root or a battery.
//! Nothing here panics: unreadable or malformed files simply produce `None`.

use crate::model::{BatteryInfo, Reading, Status};
use std::fs;
use std::path::{Path, PathBuf};

/// Environment variable that overrides the sysfs root (used by tests).
pub const SYSFS_ENV: &str = "BATTMON_SYSFS_ROOT";

/// Real sysfs root. The power supplies live in `<root>/class/power_supply`.
pub const DEFAULT_SYSFS_ROOT: &str = "/sys";

/// Microwatt-hours per watt-hour.
const UWH_PER_WH: f64 = 1e6;
/// Microvolts per volt.
const UV_PER_V: f64 = 1e6;
/// Microamp-microvolt products per watt (or per watt-hour).
const UA_UV_PER_W: f64 = 1e12;

/// Entry point to the power-supply class.
#[derive(Debug, Clone)]
pub struct Sysfs {
    root: PathBuf,
}

impl Default for Sysfs {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Sysfs {
    pub fn new(sysfs_root: impl Into<PathBuf>) -> Self {
        Self {
            root: sysfs_root.into(),
        }
    }

    /// Root from `BATTMON_SYSFS_ROOT`, falling back to `/sys`.
    pub fn from_env() -> Self {
        match std::env::var_os(SYSFS_ENV) {
            Some(value) if !value.is_empty() => Self::new(value),
            _ => Self::new(DEFAULT_SYSFS_ROOT),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/class/power_supply`.
    pub fn power_supply_dir(&self) -> PathBuf {
        self.root.join("class").join("power_supply")
    }

    /// All present batteries, ordered by node name (`BAT0`, `BAT1`, …).
    pub fn batteries(&self) -> Vec<Battery> {
        let Ok(entries) = fs::read_dir(self.power_supply_dir()) else {
            return Vec::new();
        };

        let mut found: Vec<Battery> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|dir| read_str(dir, "type").as_deref() == Some("Battery"))
            // `present` is missing on a few drivers, so only skip an explicit 0.
            .filter(|dir| read_i64(dir, "present") != Some(0))
            .map(Battery::probe)
            .collect();

        found.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        found
    }

    /// The first present battery, which is the only one a laptop normally has.
    pub fn battery(&self) -> Option<Battery> {
        self.batteries().into_iter().next()
    }

    /// Wall adapter state from any supply with `type == Mains`.
    ///
    /// Some machines have none; then this is `None` and callers fall back to
    /// the battery status alone.
    pub fn ac_online(&self) -> Option<bool> {
        let entries = fs::read_dir(self.power_supply_dir()).ok()?;

        for dir in entries.flatten().map(|entry| entry.path()) {
            if read_str(&dir, "type").as_deref() != Some("Mains") {
                continue;
            }
            if let Some(online) = read_i64(&dir, "online") {
                return Some(online != 0);
            }
        }
        None
    }

    /// Read one normalised sample now, or `None` when there is no battery.
    pub fn read(&self, ts: i64) -> Option<Reading> {
        let battery = self.battery()?;
        Some(battery.read(ts, self.ac_online()))
    }
}

/// One battery node, with its identity read once at probe time.
#[derive(Debug, Clone)]
pub struct Battery {
    dir: PathBuf,
    pub info: BatteryInfo,
}

impl Battery {
    fn probe(dir: PathBuf) -> Self {
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();

        let info = BatteryInfo {
            name,
            manufacturer: read_str(&dir, "manufacturer"),
            model: read_str(&dir, "model_name"),
            serial: read_str(&dir, "serial_number"),
            technology: read_str(&dir, "technology"),
            has_energy: file_exists(&dir, "energy_now") || file_exists(&dir, "energy_full"),
            has_power_now: file_exists(&dir, "power_now"),
            has_temp: file_exists(&dir, "temp"),
            has_charge_limit: file_exists(&dir, "charge_control_end_threshold"),
        };

        Self { dir, info }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Read a normalised sample.
    ///
    /// Wh and W are derived with `voltage_min_design` when the firmware only
    /// exposes charge (Ah) and current (A). That is the battery's nominal
    /// voltage, which is also what UPower uses, and it keeps energy and power
    /// on the same scale so `energy / power` stays honest. The real reported
    /// voltage is still available in [`Reading::voltage_v`].
    pub fn read(&self, ts: i64, ac_online: Option<bool>) -> Reading {
        let dir = &self.dir;
        let convert_uv =
            read_i64(dir, "voltage_min_design").or_else(|| read_i64(dir, "voltage_now"));

        let energy_wh = read_i64(dir, "energy_now")
            .map(|uwh| uwh as f64 / UWH_PER_WH)
            .or_else(|| charge_wh(read_i64(dir, "charge_now"), convert_uv));

        let full_wh = read_i64(dir, "energy_full")
            .map(|uwh| uwh as f64 / UWH_PER_WH)
            .or_else(|| charge_wh(read_i64(dir, "charge_full"), convert_uv));

        let design_wh = read_i64(dir, "energy_full_design")
            .map(|uwh| uwh as f64 / UWH_PER_WH)
            .or_else(|| charge_wh(read_i64(dir, "charge_full_design"), convert_uv));

        let power_w = read_i64(dir, "power_now")
            .map(|uw| (uw as f64 / UWH_PER_WH).abs())
            .or_else(|| {
                current_w(read_i64(dir, "current_now"), convert_uv).map(f64::abs)
            });

        Reading {
            ts,
            status: read_str(dir, "status")
                .map(|raw| Status::parse(&raw))
                .unwrap_or_default(),
            ac_online,
            energy_wh,
            full_wh,
            design_wh,
            power_w,
            voltage_v: read_i64(dir, "voltage_now").map(|uv| uv as f64 / UV_PER_V),
            cycle_count: read_i64(dir, "cycle_count")
                .filter(|count| *count > 0)
                .and_then(|count| u32::try_from(count).ok()),
            temp_c: read_i64(dir, "temp").map(|tenths| tenths as f32 / 10.0),
            charge_limit_pct: read_i64(dir, "charge_control_end_threshold")
                .and_then(|pct| u8::try_from(pct).ok()),
            capacity_pct: read_i64(dir, "capacity").and_then(|pct| u8::try_from(pct).ok()),
        }
    }
}

/// Convert microamp-hours and microvolts to watt-hours.
fn charge_wh(charge_uah: Option<i64>, voltage_uv: Option<i64>) -> Option<f64> {
    match (charge_uah, voltage_uv) {
        (Some(charge), Some(voltage)) => Some(charge as f64 * voltage as f64 / UA_UV_PER_W),
        _ => None,
    }
}

/// Convert microamps and microvolts to watts.
fn current_w(current_ua: Option<i64>, voltage_uv: Option<i64>) -> Option<f64> {
    match (current_ua, voltage_uv) {
        (Some(current), Some(voltage)) => Some(current as f64 * voltage as f64 / UA_UV_PER_W),
        _ => None,
    }
}

/// Read a numeric sysfs attribute. Trailing whitespace and junk are tolerated.
fn read_i64(dir: &Path, file: &str) -> Option<i64> {
    read_str(dir, file)?.parse::<i64>().ok()
}

/// Read a sysfs attribute as trimmed text. Empty files become `None`.
fn read_str(dir: &Path, file: &str) -> Option<String> {
    let raw = fs::read_to_string(dir.join(file)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn file_exists(dir: &Path, file: &str) -> bool {
    dir.join(file).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Sysfs {
        Sysfs::new(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join(name)
                .join("sys"),
        )
    }

    fn approx(actual: Option<f64>, expected: f64) -> bool {
        matches!(actual, Some(value) if (value - expected).abs() < 1e-3)
    }

    #[test]
    fn charge_only_layout_is_converted_with_nominal_voltage() {
        let sysfs = fixture("charge_only");
        let reading = sysfs.read(1_700_000_000).expect("battery");

        assert_eq!(reading.status, Status::Charging);
        assert_eq!(reading.ac_online, Some(true));

        // 2.100 Ah * 11.55 V, the same numbers UPower reports.
        assert!(approx(reading.energy_wh, 24.255));
        assert!(approx(reading.full_wh, 30.988_65));
        assert!(approx(reading.design_wh, 42.042));
        // 1.513 A * 11.55 V; UPower's energy-rate is 17.475 W.
        assert!(approx(reading.power_w, 17.475_15));
        assert!(approx(reading.voltage_v, 12.502));

        assert_eq!(reading.cycle_count, Some(333));
        assert_eq!(reading.temp_c, None);
        assert_eq!(reading.charge_limit_pct, None);
        assert_eq!(reading.capacity_pct, Some(78));
        assert!((reading.level_pct().unwrap() - 78.27).abs() < 0.1);
        assert!((reading.health_pct().unwrap() - 73.71).abs() < 0.1);
    }

    #[test]
    fn energy_layout_prefers_wh_files() {
        let sysfs = fixture("hp_energy");
        let reading = sysfs.read(1).expect("battery");

        assert_eq!(reading.status, Status::Discharging);
        assert_eq!(reading.ac_online, Some(false));
        assert!(approx(reading.energy_wh, 25.0));
        assert!(approx(reading.full_wh, 45.0));
        assert!(approx(reading.design_wh, 51.0));
        assert!(approx(reading.power_w, 7.8));
        assert!(approx(reading.voltage_v, 11.5));
        assert_eq!(reading.cycle_count, Some(214));
        assert_eq!(reading.temp_c, Some(30.5));
        assert_eq!(reading.charge_limit_pct, Some(80));
        assert!(approx(reading.level_pct(), 25.0 / 45.0 * 100.0));
    }

    #[test]
    fn zero_cycle_count_is_treated_as_unknown() {
        let reading = fixture("no_cycles").read(1).expect("battery");
        assert_eq!(reading.cycle_count, None);
        assert_eq!(reading.status, Status::Charging);
    }

    #[test]
    fn no_battery_is_not_an_error() {
        let sysfs = fixture("no_battery");
        assert!(sysfs.battery().is_none());
        assert!(sysfs.read(1).is_none());
        // The AC supply is still readable on its own.
        assert_eq!(sysfs.ac_online(), Some(true));
    }

    #[test]
    fn ac_connected_but_held() {
        let reading = fixture("ac_not_charging").read(1).expect("battery");
        assert_eq!(reading.status, Status::NotCharging);
        assert_eq!(reading.ac_online, Some(true));
        assert!(approx(reading.power_w, 0.0));
        assert_eq!(reading.status.is_on_ac(), true);
        assert!(!reading.status.is_charging());
    }

    #[test]
    fn missing_files_degrade_to_none_instead_of_panicking() {
        let reading = fixture("capacity_only").read(1).expect("battery");
        assert_eq!(reading.energy_wh, None);
        assert_eq!(reading.full_wh, None);
        assert_eq!(reading.power_w, None);
        assert_eq!(reading.voltage_v, None);
        assert_eq!(reading.level_pct(), Some(64.0));
        assert_eq!(reading.health_pct(), None);
        assert!(!reading.is_storable());
    }

    #[test]
    fn missing_root_yields_nothing() {
        let sysfs = Sysfs::new("/nonexistent/battmon/sys");
        assert!(sysfs.batteries().is_empty());
        assert!(sysfs.read(1).is_none());
        assert_eq!(sysfs.ac_online(), None);
    }

    #[test]
    fn garbage_numbers_are_ignored() {
        let reading = fixture("garbage").read(1).expect("battery");
        assert_eq!(reading.energy_wh, None);
        assert_eq!(reading.status, Status::Unknown);
        assert_eq!(reading.cycle_count, None);
        assert_eq!(reading.level_pct(), Some(50.0));
    }
}
