//! Human-facing formatting shared by the daemon and the UI.
//!
//! Times are stored as unix seconds and only converted to local time here, so
//! there is exactly one place that knows about time zones.

use anyhow::{Context, Result};
use std::time::{SystemTime, UNIX_EPOCH};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Unix seconds now, saturating to 0 if the clock is before the epoch.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

fn local(ts: i64) -> Result<jiff::ZonedDateTime> {
    let timestamp = jiff::Timestamp::from_second(ts).context("timestamp out of range")?;
    Ok(timestamp.to_zoned_datetime(jiff::tz::TimeZone::system()))
}

/// `YYYY-MM-DD`, the key used by `daily_health`.
pub fn local_day(ts: i64) -> Result<String> {
    let date = local(ts)?.date();
    Ok(format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month(),
        date.day()
    ))
}

/// `18 Nov 2026`, for the "tracked since" label.
pub fn local_date_human(ts: i64) -> Result<String> {
    let date = local(ts)?.date();
    Ok(format!(
        "{} {} {}",
        date.day(),
        month_name(date.month()),
        date.year()
    ))
}

/// `14:05`, for chart axes and tooltips.
pub fn local_clock(ts: i64) -> Result<String> {
    let time = local(ts)?;
    Ok(format!("{:02}:{:02}", time.hour(), time.minute()))
}

/// `18 Nov`, for the weekly chart axis.
pub fn local_day_month(ts: i64) -> Result<String> {
    let date = local(ts)?.date();
    Ok(format!("{} {}", date.day(), month_name(date.month())))
}

/// Seconds since local midnight, used to align axis ticks to the wall clock.
pub fn local_seconds_of_day(ts: i64) -> Result<i64> {
    let time = local(ts)?;
    Ok(i64::from(time.hour()) * 3_600 + i64::from(time.minute()) * 60 + i64::from(time.second()))
}

fn month_name(month: i8) -> &'static str {
    MONTHS
        .get(usize::from(month.saturating_sub(1) as u8))
        .copied()
        .unwrap_or("???")
}

/// `4 h 20 min`, rounded to the nearest five minutes so it does not flicker.
pub fn format_duration(seconds: f64) -> String {
    let rounded = crate::estimate::round_to_5_min(seconds);
    if rounded <= 0 {
        return "under 5 min".to_owned();
    }

    let total_minutes = rounded / 60;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    match (hours, minutes) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// Same shape, rounded to the nearest minute.
///
/// Charging benefits from the finer grain: "38 min to full" is useful, while
/// "35 min" would hide the taper and jump around less honestly.
pub fn format_duration_fine(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "under a minute".to_owned();
    }

    let total_minutes = (seconds / 60.0).round() as i64;
    if total_minutes <= 0 {
        return "under a minute".to_owned();
    }
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    match (hours, minutes) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_round_to_five_minutes() {
        assert_eq!(format_duration(4.0 * 3_600.0 + 20.0 * 60.0), "4 h 20 min");
        assert_eq!(format_duration(2_280.0), "40 min"); // 38 min rounds up
        assert_eq!(format_duration(3_600.0), "1 h");
        assert_eq!(format_duration(5_400.0), "1 h 30 min");
        assert_eq!(format_duration(60.0), "under 5 min");
        assert_eq!(format_duration(0.0), "under 5 min");
        assert_eq!(format_duration(f64::NAN), "under 5 min");
        assert_eq!(format_duration(17_880.0), "5 h"); // 4 h 58 min rounds up
    }

    #[test]
    fn fine_durations_keep_the_minute() {
        assert_eq!(format_duration_fine(38.0 * 60.0), "38 min");
        assert_eq!(format_duration_fine(90.0 * 60.0), "1 h 30 min");
        assert_eq!(format_duration_fine(3_600.0), "1 h");
        assert_eq!(format_duration_fine(20.0), "under a minute");
        assert_eq!(format_duration_fine(0.0), "under a minute");
    }

    #[test]
    fn day_keys_are_iso_shaped() {
        let day = local_day(1_765_000_000).unwrap();
        assert_eq!(day.len(), 10);
        assert_eq!(&day[4..5], "-");
        assert_eq!(&day[7..8], "-");
        assert!(day.starts_with("20"));
    }

    #[test]
    fn clock_and_human_date_are_stable() {
        let ts = 1_765_000_000;
        assert_eq!(local_clock(ts).unwrap().len(), 5);
        assert!(local_date_human(ts).unwrap().contains(' '));
        assert!(local_day_month(ts).unwrap().contains(' '));
    }
}
