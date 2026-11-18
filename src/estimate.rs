//! Estimators: smoothed power, time left, time to full, health, cycles.
//!
//! Everything here is pure and unit-tested; the GUI feeds it live readings and
//! replays recent history on startup so it does not start from "…" every launch.

use crate::model::{Reading, Status};

/// Time constant of the discharge average: ~10 min of history (3τ).
pub const DISCHARGE_TAU_SECS: f64 = 200.0;
/// Time constant of the charge average: ~5 min of history (3τ).
pub const CHARGE_TAU_SECS: f64 = 100.0;
/// Readings below this are bad reads (state-change noise, rounding to zero).
pub const MIN_POWER_W: f64 = 0.5;
/// Ignore readings for this long after a status change, while the firmware settles.
pub const SETTLE_SECS: i64 = 60;
/// No estimate is shown until this much time has been observed.
pub const WARMUP_SECS: f64 = 120.0;
/// Above this state of charge, charging tapers off.
pub const TAPER_START_PCT: f64 = 80.0;
/// How much slower the taper region is; calibrated conservatively for v1.
pub const TAPER_FACTOR: f64 = 1.8;
/// Display granularity for remaining time.
pub const ROUND_SECS: f64 = 300.0;

/// Time-weighted exponential moving average of a power reading.
///
/// Gaps longer than [`SETTLE_SECS`] minutes (suspend, clock jump) restart the
/// average instead of smearing a stale value across them.
#[derive(Debug, Clone)]
pub struct PowerEma {
    tau_secs: f64,
    min_w: f64,
    value: Option<f64>,
    observed_secs: f64,
    last_ts: Option<i64>,
}

impl PowerEma {
    pub fn new(tau_secs: f64, min_w: f64) -> Self {
        Self {
            tau_secs,
            min_w,
            value: None,
            observed_secs: 0.0,
            last_ts: None,
        }
    }

    pub fn reset(&mut self) {
        self.value = None;
        self.observed_secs = 0.0;
        self.last_ts = None;
    }

    /// Feed one reading. Returns whether it was accepted.
    pub fn update(&mut self, ts: i64, watts: f64) -> bool {
        if !watts.is_finite() || watts < self.min_w {
            return false;
        }

        let mut delta = 0_i64;
        if let Some(previous) = self.last_ts {
            let raw = ts - previous;
            if raw < 0 || raw as f64 > 3.0 * self.tau_secs {
                // Clock jumped, or we were asleep: start over.
                self.reset();
            } else {
                delta = raw;
            }
        }

        let alpha = 1.0 - (-(delta as f64) / self.tau_secs).exp();
        self.value = Some(match self.value {
            None => watts,
            Some(previous) => previous + alpha * (watts - previous),
        });
        self.observed_secs += delta as f64;
        self.last_ts = Some(ts);
        true
    }

    pub fn average(&self) -> Option<f64> {
        self.value
    }

    /// True once enough time has been observed for the average to mean anything.
    pub fn is_warm(&self) -> bool {
        self.value.is_some() && self.observed_secs >= WARMUP_SECS
    }

    pub fn observed_secs(&self) -> f64 {
        self.observed_secs
    }
}

/// What the UI should show for the current reading.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveEstimate {
    /// Seconds until empty, while discharging.
    pub time_left_secs: Option<f64>,
    /// Seconds until full, while charging.
    pub time_to_full_secs: Option<f64>,
    pub avg_discharge_w: Option<f64>,
    pub avg_charge_w: Option<f64>,
    /// True when a discharge average exists but is still warming up.
    pub warming_up: bool,
}

/// Tracks smoothed charge and discharge power across readings.
#[derive(Debug, Clone)]
pub struct Tracker {
    discharge: PowerEma,
    charge: PowerEma,
    status: Option<Status>,
    status_since: i64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub fn new() -> Self {
        Self {
            discharge: PowerEma::new(DISCHARGE_TAU_SECS, MIN_POWER_W),
            charge: PowerEma::new(CHARGE_TAU_SECS, MIN_POWER_W),
            status: None,
            status_since: 0,
        }
    }

    /// Feed a reading, in chronological order.
    pub fn observe(&mut self, reading: &Reading) {
        if self.status != Some(reading.status) {
            // Averages only ever describe the current status, so a flip drops
            // both: the old one would be stale by the time it is usable again.
            self.discharge.reset();
            self.charge.reset();
            self.status = Some(reading.status);
            self.status_since = reading.ts;
        }

        if reading.ts - self.status_since < SETTLE_SECS {
            return;
        }

        let Some(power_w) = reading.power_w else {
            return;
        };
        match reading.status {
            Status::Discharging => {
                self.discharge.update(reading.ts, power_w);
            }
            Status::Charging => {
                self.charge.update(reading.ts, power_w);
            }
            _ => {}
        }
    }

    pub fn estimate(&self, reading: &Reading) -> LiveEstimate {
        LiveEstimate {
            time_left_secs: self.time_left_secs(reading),
            time_to_full_secs: self.time_to_full_secs(reading),
            avg_discharge_w: self.discharge.average(),
            avg_charge_w: self.charge.average(),
            warming_up: reading.status == Status::Discharging
                && self.discharge.average().is_some()
                && !self.discharge.is_warm(),
        }
    }

    pub fn time_left_secs(&self, reading: &Reading) -> Option<f64> {
        if reading.status != Status::Discharging || reading.ts - self.status_since < SETTLE_SECS {
            return None;
        }
        if !self.discharge.is_warm() {
            return None;
        }
        time_left_secs(reading.energy_wh?, self.discharge.average()?)
    }

    pub fn time_to_full_secs(&self, reading: &Reading) -> Option<f64> {
        if reading.status != Status::Charging || reading.ts - self.status_since < SETTLE_SECS {
            return None;
        }
        if !self.charge.is_warm() {
            return None;
        }
        time_to_full_secs(
            reading.energy_wh?,
            reading.full_wh?,
            self.charge.average()?,
        )
    }

    pub fn discharge_average(&self) -> Option<f64> {
        self.discharge.average()
    }

    pub fn charge_average(&self) -> Option<f64> {
        self.charge.average()
    }
}

/// Hours of runtime left at a constant draw, in seconds.
pub fn time_left_secs(energy_wh: f64, average_power_w: f64) -> Option<f64> {
    if !energy_wh.is_finite() || !average_power_w.is_finite() || average_power_w < MIN_POWER_W {
        return None;
    }
    let seconds = energy_wh / average_power_w * 3_600.0;
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

/// Seconds until full at a constant charge rate, with the top-end taper.
///
/// Energy above [`TAPER_START_PCT`] is charged roughly `TAPER_FACTOR` times
/// slower than the bulk region, which is what makes a linear estimate promise
/// "5 min left" for half an hour near 100%.
pub fn time_to_full_secs(energy_wh: f64, full_wh: f64, charge_power_w: f64) -> Option<f64> {
    if !energy_wh.is_finite() || !full_wh.is_finite() || !charge_power_w.is_finite() {
        return None;
    }
    if full_wh <= 0.0 || charge_power_w < MIN_POWER_W {
        return None;
    }

    let remaining_wh = (full_wh - energy_wh).max(0.0);
    if remaining_wh <= 0.0 {
        return Some(0.0);
    }

    let taper_wh = full_wh * TAPER_START_PCT / 100.0;
    let bulk_wh = (taper_wh - energy_wh).clamp(0.0, remaining_wh);
    let tapered_wh = remaining_wh - bulk_wh;
    let effective_wh = bulk_wh + tapered_wh * TAPER_FACTOR;

    Some(effective_wh / charge_power_w * 3_600.0)
}

/// Full capacity as a percentage of design capacity.
pub fn health_pct(full_wh: f64, design_wh: f64) -> Option<f64> {
    if design_wh > 0.0 && full_wh.is_finite() && design_wh.is_finite() {
        Some(full_wh / design_wh * 100.0)
    } else {
        None
    }
}

/// Charge rate as percent of a full battery per hour.
pub fn rate_pct_per_h(power_w: f64, full_wh: f64) -> Option<f64> {
    if full_wh > 0.0 && power_w.is_finite() && full_wh.is_finite() {
        Some(power_w / full_wh * 100.0)
    } else {
        None
    }
}

/// Estimated full cycles from lifetime discharge energy.
pub fn estimate_cycles(cumulative_discharge_wh: f64, design_wh: f64) -> Option<f64> {
    if design_wh <= 0.0 || !cumulative_discharge_wh.is_finite() || cumulative_discharge_wh < 0.0 {
        return None;
    }
    Some(cumulative_discharge_wh / design_wh)
}

/// Median of a set of power readings, ignoring bad reads.
///
/// Used for "typical runtime", which is far more stable than the instantaneous
/// draw because it smooths out spikes from a compile or a video call.
pub fn median_power_w(powers: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut values: Vec<f64> = powers
        .into_iter()
        .filter(|power| power.is_finite() && *power >= MIN_POWER_W)
        .collect();
    if values.is_empty() {
        return None;
    }

    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[middle - 1] + values[middle]) / 2.0)
    } else {
        Some(values[middle])
    }
}

/// Runtime at the typical draw, in seconds.
pub fn typical_runtime_secs(energy_wh: f64, median_power_w: f64) -> Option<f64> {
    time_left_secs(energy_wh, median_power_w)
}

/// Round a duration to the display granularity so it does not flicker.
pub fn round_to_5_min(seconds: f64) -> i64 {
    if !seconds.is_finite() {
        return 0;
    }
    ((seconds / ROUND_SECS).round() as i64) * ROUND_SECS as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(ts: i64, status: Status, energy_wh: f64, power_w: Option<f64>) -> Reading {
        Reading {
            ts,
            status,
            ac_online: Some(status.is_on_ac()),
            energy_wh: Some(energy_wh),
            full_wh: Some(40.0),
            design_wh: Some(50.0),
            power_w,
            voltage_v: Some(12.0),
            cycle_count: None,
            temp_c: None,
            charge_limit_pct: None,
            capacity_pct: None,
        }
    }

    #[test]
    fn ema_of_constant_input_stays_constant() {
        let mut ema = PowerEma::new(200.0, MIN_POWER_W);
        for step in 0..40 {
            ema.update(step * 10, 8.0);
        }
        assert!((ema.average().unwrap() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn ema_rejects_small_and_non_finite_reads() {
        let mut ema = PowerEma::new(200.0, MIN_POWER_W);
        assert!(!ema.update(0, 0.2));
        assert!(!ema.update(10, f64::NAN));
        assert!(!ema.update(20, f64::INFINITY));
        assert_eq!(ema.average(), None);

        assert!(ema.update(30, 7.0));
        assert_eq!(ema.average(), Some(7.0));
    }

    #[test]
    fn ema_moves_towards_a_new_level_without_jumping() {
        let mut ema = PowerEma::new(200.0, MIN_POWER_W);
        ema.update(0, 10.0);
        ema.update(10, 10.0);
        ema.update(20, 20.0);
        let value = ema.average().unwrap();
        assert!(value > 10.0 && value < 13.0, "got {value}");
    }

    #[test]
    fn ema_restarts_after_a_suspend_sized_gap() {
        let mut ema = PowerEma::new(200.0, MIN_POWER_W);
        ema.update(0, 10.0);
        ema.update(600, 10.0);
        ema.update(1_200, 10.0);
        assert!(ema.is_warm());

        // Six hours later: the old average must not leak into the new session.
        ema.update(1_200 + 6 * 3_600, 2.0);
        assert_eq!(ema.average(), Some(2.0));
        assert!(!ema.is_warm());
    }

    #[test]
    fn ema_restarts_when_the_clock_jumps_backwards() {
        let mut ema = PowerEma::new(200.0, MIN_POWER_W);
        ema.update(10_000, 10.0);
        ema.update(9_000, 3.0);
        assert_eq!(ema.average(), Some(3.0));
    }

    #[test]
    fn ema_warmup_requires_two_minutes_of_history() {
        let mut ema = PowerEma::new(200.0, MIN_POWER_W);
        ema.update(0, 8.0);
        ema.update(60, 8.0);
        assert!(!ema.is_warm());
        ema.update(120, 8.0);
        assert!(ema.is_warm());
    }

    #[test]
    fn tracker_ignores_the_settling_window_after_unplugging() {
        let mut tracker = Tracker::new();
        // Still on AC, then unplugged at t=1000.
        tracker.observe(&reading(900, Status::Charging, 30.0, Some(20.0)));
        tracker.observe(&reading(1_000, Status::Discharging, 30.0, Some(9.0)));
        tracker.observe(&reading(1_030, Status::Discharging, 29.9, Some(9.0)));
        assert_eq!(tracker.discharge_average(), None, "settling reads must be ignored");

        tracker.observe(&reading(1_090, Status::Discharging, 29.5, Some(9.0)));
        assert_eq!(tracker.discharge_average(), Some(9.0));
    }

    #[test]
    fn status_flip_resets_the_average() {
        let mut tracker = Tracker::new();
        for step in 0..20 {
            tracker.observe(&reading(step * 10, Status::Discharging, 30.0, Some(9.0)));
        }
        assert!(tracker.discharge_average().is_some());

        // Plug in: the discharge average must not carry over.
        tracker.observe(&reading(200, Status::Charging, 30.0, Some(15.0)));
        assert_eq!(tracker.discharge_average(), None);
    }

    #[test]
    fn time_left_is_energy_over_power() {
        // 20 Wh at 10 W = 2 h.
        assert_eq!(time_left_secs(20.0, 10.0), Some(7_200.0));
    }

    #[test]
    fn time_left_refuses_zero_power() {
        assert_eq!(time_left_secs(20.0, 0.0), None);
        assert_eq!(time_left_secs(20.0, 0.4), None);
        assert_eq!(time_left_secs(0.0, 10.0), None);
    }

    #[test]
    fn bulk_energy_is_charged_faster_than_the_taper_region() {
        // 40 Wh pack at 8 Wh (20%): 24 Wh below the 32 Wh mark plus 8 Wh that
        // costs 1.8x, so 38.4 effective Wh at 10 W.
        let estimate = time_to_full_secs(8.0, 40.0, 10.0).unwrap();
        assert!((estimate - (24.0 + 8.0 * 1.8) / 10.0 * 3_600.0).abs() < 1e-6);
    }

    #[test]
    fn time_to_full_applies_taper_above_eighty_percent() {
        // 40 Wh pack at 36 Wh (90%): 4 Wh left, all in the taper region.
        // 4 * 1.8 / 10 * 3600 = 2592 s, not the linear 1440 s.
        let estimate = time_to_full_secs(36.0, 40.0, 10.0).unwrap();
        assert!((estimate - 2_592.0).abs() < 1e-6, "got {estimate}");

        // Mixed: 30 Wh (75%), 10 Wh left, 2 Wh bulk + 8 Wh tapered.
        let mixed = time_to_full_secs(30.0, 40.0, 10.0).unwrap();
        assert!((mixed - (2.0 + 8.0 * 1.8) / 10.0 * 3_600.0).abs() < 1e-6);
    }

    #[test]
    fn time_to_full_at_empty_is_the_whole_pack() {
        let estimate = time_to_full_secs(0.0, 40.0, 10.0).unwrap();
        // 32 Wh bulk + 8 Wh * 1.8 = 46.4 Wh / 10 W.
        assert!((estimate - 4_640.0 * 3.6).abs() < 1e-6);
    }

    #[test]
    fn time_to_full_refuses_full_or_broken_values() {
        assert_eq!(time_to_full_secs(40.0, 40.0, 10.0), Some(0.0));
        assert_eq!(time_to_full_secs(39.0, 40.0, 0.0), None);
        assert_eq!(time_to_full_secs(39.0, 0.0, 10.0), None);
        assert_eq!(time_to_full_secs(45.0, 40.0, 10.0), Some(0.0));
    }

    #[test]
    fn tracker_only_estimates_for_the_matching_status() {
        let mut tracker = Tracker::new();
        let discharging = reading(0, Status::Discharging, 20.0, Some(10.0));
        tracker.observe(&discharging);
        assert_eq!(tracker.time_to_full_secs(&discharging), None);

        let charging = reading(0, Status::Charging, 30.0, Some(10.0));
        tracker.observe(&charging);
        assert_eq!(tracker.time_left_secs(&charging), None);
    }

    #[test]
    fn tracker_reports_warming_up_before_showing_a_time_left() {
        let mut tracker = Tracker::new();
        tracker.observe(&reading(0, Status::Discharging, 30.0, Some(9.0)));
        tracker.observe(&reading(70, Status::Discharging, 29.8, Some(9.0)));

        let estimate = tracker.estimate(&reading(70, Status::Discharging, 29.8, Some(9.0)));
        assert_eq!(estimate.time_left_secs, None);
        assert!(estimate.warming_up);
    }

    #[test]
    fn health_is_full_over_design() {
        assert_eq!(health_pct(30.988_65, 42.042), Some(30.988_65 / 42.042 * 100.0));
        assert_eq!(health_pct(30.0, 0.0), None);
    }

    #[test]
    fn rate_is_power_over_full_capacity() {
        // 17.475 W into a 30.98865 Wh pack.
        let rate = rate_pct_per_h(17.475, 30.988_65).unwrap();
        assert!((rate - 56.39).abs() < 0.01, "got {rate}");
    }

    #[test]
    fn estimated_cycles_use_design_capacity() {
        assert_eq!(estimate_cycles(504.0, 42.0), Some(12.0));
        assert_eq!(estimate_cycles(0.0, 42.0), Some(0.0));
        assert_eq!(estimate_cycles(100.0, 0.0), None);
        assert_eq!(estimate_cycles(-1.0, 42.0), None);
    }

    #[test]
    fn median_ignores_junk_and_sorts() {
        assert_eq!(median_power_w([5.0, 1.0, 9.0]), Some(5.0));
        assert_eq!(median_power_w([9.0, 1.0, 5.0, 3.0]), Some(4.0));
        assert_eq!(median_power_w([0.1, 0.2]), None);
        assert_eq!(median_power_w([]), None);
        assert_eq!(median_power_w([f64::NAN, 4.0]), Some(4.0));
    }

    #[test]
    fn typical_runtime_uses_the_median() {
        assert_eq!(typical_runtime_secs(20.0, 8.0), Some(9_000.0));
    }

    #[test]
    fn rounding_to_five_minutes() {
        assert_eq!(round_to_5_min(60.0), 0);
        assert_eq!(round_to_5_min(200.0), 300);
        assert_eq!(round_to_5_min(7_200.0), 7_200);
        assert_eq!(round_to_5_min(f64::NAN), 0);
    }
}
