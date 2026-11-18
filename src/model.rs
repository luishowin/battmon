//! Plain data types shared by the reader, the store and the UI.
//!
//! Everything is normalised to Wh / W / V / °C / percent here; the sysfs
//! micro-units never leave [`crate::sysfs`].

use std::fmt;

/// Kernel `status` values, plus the codes used in the `samples` table.
///
/// The discriminants are part of the database format: do not renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(i64)]
pub enum Status {
    #[default]
    Unknown = 0,
    Charging = 1,
    Discharging = 2,
    Full = 3,
    /// AC connected, but the battery is held (charge limit / battery care).
    NotCharging = 4,
}

impl Status {
    /// Parse the kernel's `status` file. Unknown strings map to [`Status::Unknown`].
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "charging" => Self::Charging,
            "discharging" => Self::Discharging,
            "full" => Self::Full,
            "not charging" => Self::NotCharging,
            _ => Self::Unknown,
        }
    }

    /// Decode a value previously written to the database.
    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => Self::Charging,
            2 => Self::Discharging,
            3 => Self::Full,
            4 => Self::NotCharging,
            _ => Self::Unknown,
        }
    }

    pub fn as_i64(self) -> i64 {
        self as i64
    }

    /// Short human label used in the UI and by `battmond --once`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Charging => "Charging",
            Self::Discharging => "Discharging",
            Self::Full => "Full",
            Self::NotCharging => "Not charging",
        }
    }

    /// True only when energy is actually flowing into the battery.
    pub fn is_charging(self) -> bool {
        self == Self::Charging
    }

    /// True when the machine is on wall power.
    pub fn is_on_ac(self) -> bool {
        matches!(self, Self::Charging | Self::Full | Self::NotCharging)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Static identity of a battery, read once at startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatteryInfo {
    /// Sysfs node name, e.g. `BAT0`.
    pub name: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub technology: Option<String>,
    /// True when the firmware exposes `energy_*` (Wh) rather than `charge_*` (Ah).
    pub has_energy: bool,
    pub has_power_now: bool,
    pub has_temp: bool,
    pub has_charge_limit: bool,
}

impl BatteryInfo {
    /// `Hewlett-Packard Primary`, falling back to just the node name.
    pub fn display_name(&self) -> String {
        match (&self.manufacturer, &self.model) {
            (Some(maker), Some(model)) => format!("{maker} {model}"),
            (None, Some(model)) => model.clone(),
            (Some(maker), None) => maker.clone(),
            (None, None) => self.name.clone(),
        }
    }
}

/// One normalised sample of everything the machine will tell us.
///
/// Every measurement is optional: a missing sysfs file becomes `None` and the
/// UI shows `n/a` instead of the daemon guessing.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// Unix seconds.
    pub ts: i64,
    pub status: Status,
    /// Wall power; `None` if the machine exposes no `Mains` supply.
    pub ac_online: Option<bool>,
    pub energy_wh: Option<f64>,
    pub full_wh: Option<f64>,
    /// Capacity when the battery was new.
    pub design_wh: Option<f64>,
    /// Unsigned magnitude in watts; direction comes from [`Reading::status`].
    pub power_w: Option<f64>,
    pub voltage_v: Option<f64>,
    /// `None` when the firmware reports 0 or nothing at all.
    pub cycle_count: Option<u32>,
    pub temp_c: Option<f32>,
    pub charge_limit_pct: Option<u8>,
    /// Integer `capacity` file, used only when the Wh numbers are unavailable.
    pub capacity_pct: Option<u8>,
}

impl Reading {
    /// Charge level in percent, from Wh when possible, else the integer
    /// `capacity` file. Clamped to 0–100 because some firmwares overshoot.
    pub fn level_pct(&self) -> Option<f64> {
        if let (Some(energy), Some(full)) = (self.energy_wh, self.full_wh)
            && full > 0.0
        {
            return Some((energy / full * 100.0).clamp(0.0, 100.0));
        }
        self.capacity_pct.map(f64::from)
    }

    /// Full-charge capacity as a percentage of design capacity.
    pub fn health_pct(&self) -> Option<f64> {
        match (self.full_wh, self.design_wh) {
            (Some(full), Some(design)) if design > 0.0 => Some(full / design * 100.0),
            _ => None,
        }
    }

    /// Charge or discharge rate as percent of a full battery per hour.
    pub fn rate_pct_per_h(&self) -> Option<f64> {
        match (self.power_w, self.full_wh) {
            (Some(power), Some(full)) if full > 0.0 => Some(power / full * 100.0),
            _ => None,
        }
    }

    /// True when the sample has the two values the `samples` table requires.
    pub fn is_storable(&self) -> bool {
        self.energy_wh.is_some() && self.full_wh.is_some() && self.level_pct().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(energy: Option<f64>, full: Option<f64>) -> Reading {
        Reading {
            ts: 0,
            status: Status::Discharging,
            ac_online: Some(false),
            energy_wh: energy,
            full_wh: full,
            design_wh: Some(51.0),
            power_w: Some(7.8),
            voltage_v: None,
            cycle_count: None,
            temp_c: None,
            charge_limit_pct: None,
            capacity_pct: None,
        }
    }

    #[test]
    fn status_round_trips_through_db_codes() {
        for status in [
            Status::Unknown,
            Status::Charging,
            Status::Discharging,
            Status::Full,
            Status::NotCharging,
        ] {
            assert_eq!(Status::from_i64(status.as_i64()), status);
            assert_eq!(Status::parse(status.label()), status);
        }
    }

    #[test]
    fn unknown_status_string_does_not_panic() {
        assert_eq!(Status::parse("bogus"), Status::Unknown);
        assert_eq!(Status::parse(""), Status::Unknown);
    }

    #[test]
    fn level_prefers_wh_then_capacity() {
        assert_eq!(reading(Some(23.0), Some(46.0)).level_pct(), Some(50.0));

        let mut r = reading(None, None);
        r.capacity_pct = Some(73);
        assert_eq!(r.level_pct(), Some(73.0));
    }

    #[test]
    fn level_survives_zero_full_capacity() {
        assert_eq!(reading(Some(23.0), Some(0.0)).level_pct(), None);
    }

    #[test]
    fn level_is_clamped() {
        assert_eq!(reading(Some(47.0), Some(46.0)).level_pct(), Some(100.0));
        assert_eq!(reading(Some(-1.0), Some(46.0)).level_pct(), Some(0.0));
    }

    #[test]
    fn health_and_rate() {
        let r = reading(Some(23.0), Some(46.0));
        assert_eq!(r.health_pct(), Some(46.0 / 51.0 * 100.0));
        assert_eq!(r.rate_pct_per_h(), Some(7.8 / 46.0 * 100.0));
    }
}
