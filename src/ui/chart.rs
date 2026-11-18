//! History charts drawn with Cairo on `gtk::DrawingArea`.
//!
//! Two stacked plots (level, power) plus a health sparkline, all reading from a
//! shared state so the crosshair stays in sync. Gaps larger than
//! [`GAP_SECS`] break the line instead of interpolating across suspend.

use adw::prelude::*;
use battmon::format;
use battmon::model::Status;
use battmon::store::{DailyHealth, Sample};
use gtk::cairo;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

/// A gap longer than this is a suspend or a shutdown, not a straight line.
pub const GAP_SECS: i64 = 180;

/// Left gutter wide enough for `100%` axis labels.
const LEFT: f64 = 38.0;
const RIGHT: f64 = 12.0;
const TOP: f64 = 10.0;
const BOTTOM: f64 = 22.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
}

impl Range {
    fn seconds(self) -> i64 {
        match self {
            Self::Day => 86_400,
            Self::Week => 7 * 86_400,
        }
    }
}

/// Everything the painters need. Updates come from the window's 60 s history
/// refresh, so drawing itself is pure and cheap.
pub struct ChartState {
    samples: Vec<Sample>,
    health: Vec<DailyHealth>,
    range: Range,
    hover_ts: Option<i64>,
    now: i64,
}

impl Default for ChartState {
    fn default() -> Self {
        Self {
            samples: Vec::new(),
            health: Vec::new(),
            range: Range::Day,
            hover_ts: None,
            now: format::now_secs(),
        }
    }
}

/// The chart block: range switch, level plot, power plot and health sparkline.
#[derive(Clone)]
pub struct Charts {
    pub root: gtk::Box,
    level: gtk::DrawingArea,
    power: gtk::DrawingArea,
    spark: gtk::DrawingArea,
    spark_note: gtk::Label,
    state: Rc<RefCell<ChartState>>,
}

impl Charts {
    pub fn new() -> Self {
        let state = Rc::new(RefCell::new(ChartState::default()));

        let title = gtk::Label::new(Some("History"));
        title.set_xalign(0.0);
        title.add_css_class("heading");

        let day = adw::Toggle::new();
        day.set_label(Some("24 h"));
        let week = adw::Toggle::new();
        week.set_label(Some("7 days"));

        let group = adw::ToggleGroup::new();
        group.add(day.clone());
        group.add(week.clone());
        group.set_active(0);
        group.set_halign(gtk::Align::End);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.append(&title);
        header.set_hexpand(true);
        let header_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header_spacer.set_hexpand(true);
        header.append(&header_spacer);
        header.append(&group);

        let level = gtk::DrawingArea::new();
        level.set_content_height(170);
        level.set_hexpand(true);
        level.set_draw_func({
            let state = Rc::clone(&state);
            move |area, cr, width, height| {
                let state = state.borrow();
                draw_level(&colors(area), cr, width, height, &state);
            }
        });

        let power = gtk::DrawingArea::new();
        power.set_content_height(100);
        power.set_hexpand(true);
        power.set_draw_func({
            let state = Rc::clone(&state);
            move |area, cr, width, height| {
                let state = state.borrow();
                draw_power(&colors(area), cr, width, height, &state);
            }
        });

        let spark = gtk::DrawingArea::new();
        spark.set_content_height(56);
        spark.set_hexpand(true);
        spark.set_draw_func({
            let state = Rc::clone(&state);
            move |area, cr, width, height| {
                let state = state.borrow();
                draw_health(&colors(area), cr, width, height, &state);
            }
        });

        let spark_note = gtk::Label::new(Some("Collecting health data…"));
        spark_note.add_css_class("caption");
        spark_note.add_css_class("dim-label");
        spark_note.set_xalign(0.0);
        spark_note.set_visible(false);

        let health_title = gtk::Label::new(Some("Health trend"));
        health_title.set_xalign(0.0);
        health_title.add_css_class("caption");
        health_title.add_css_class("dim-label");

        let health_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        health_box.append(&health_title);
        health_box.append(&spark);
        health_box.append(&spark_note);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(6);
        root.append(&header);
        root.append(&level);
        root.append(&power);
        root.append(&health_box);

        // Range switching is instant: all 8 days are already in memory.
        group.connect_active_notify({
            let state = Rc::clone(&state);
            let level = level.clone();
            let power = power.clone();
            let group = group.clone();
            move |_| {
                let range = if group.active() == 0 {
                    Range::Day
                } else {
                    Range::Week
                };
                let mut state = state.borrow_mut();
                if state.range != range {
                    state.range = range;
                    state.hover_ts = None;
                    drop(state);
                    level.queue_draw();
                    power.queue_draw();
                }
            }
        });

        attach_hover(&level, &power, &state, Which::Level);
        attach_hover(&power, &level, &state, Which::Power);
        redraw_on_theme_change(&[&level, &power, &spark]);

        Self {
            root,
            level,
            power,
            spark,
            spark_note,
            state,
        }
    }

    /// Replace the data set (8 days of samples, all daily health rows).
    pub fn set_data(&self, samples: Vec<Sample>, health: Vec<DailyHealth>) {
        let mut state = self.state.borrow_mut();
        state.samples = samples;
        state.health = health;
        state.now = format::now_secs();
        // Keep an active hover: the timestamp is still meaningful after a refresh.
        drop(state);

        let weeks = self.state.borrow().health.len() >= 7;
        self.spark.set_visible(weeks);
        self.spark_note.set_visible(!weeks);

        self.root.set_visible(!self.state.borrow().samples.is_empty());
        self.redraw();
    }

    pub fn redraw(&self) {
        self.level.queue_draw();
        self.power.queue_draw();
        self.spark.queue_draw();
    }

}

impl Default for Charts {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
enum Which {
    Level,
    Power,
}

#[derive(Clone, Copy)]
struct Plot {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Plot {
    fn for_level(width: f64, height: f64) -> Self {
        Self {
            x: LEFT,
            y: TOP,
            width: (width - LEFT - RIGHT).max(1.0),
            height: (height - TOP - BOTTOM).max(1.0),
        }
    }

    fn for_power(width: f64, height: f64) -> Self {
        Self::for_level(width, height)
    }

    fn for_which(which: Which, width: f64, height: f64) -> Self {
        match which {
            Which::Level => Self::for_level(width, height),
            Which::Power => Self::for_power(width, height),
        }
    }

    fn x_at(&self, ts: i64, start: i64, end: i64) -> f64 {
        if end <= start {
            return self.x;
        }
        let ratio = (ts - start) as f64 / (end - start) as f64;
        self.x + ratio.clamp(0.0, 1.0) * self.width
    }

    fn y_at(&self, value: f64, min: f64, max: f64) -> f64 {
        if max <= min {
            return self.y + self.height;
        }
        let ratio = ((value - min) / (max - min)).clamp(0.0, 1.0);
        self.y + (1.0 - ratio) * self.height
    }

    fn ts_at(&self, x: f64, start: i64, end: i64) -> i64 {
        let ratio = ((x - self.x) / self.width).clamp(0.0, 1.0);
        start + (ratio * (end - start) as f64).round() as i64
    }
}

#[derive(Clone, Copy)]
struct Colors {
    fg: (f64, f64, f64),
    accent: (f64, f64, f64),
}

fn colors(widget: &gtk::DrawingArea) -> Colors {
    let fg = widget.color();
    let accent = adw::StyleManager::default().accent_color_rgba();
    Colors {
        fg: (f64::from(fg.red()), f64::from(fg.green()), f64::from(fg.blue())),
        accent: (
            f64::from(accent.red()),
            f64::from(accent.green()),
            f64::from(accent.blue()),
        ),
    }
}

fn draw_level(
    colors: &Colors,
    cr: &cairo::Context,
    width: i32,
    height: i32,
    state: &ChartState,
) {
    let plot = Plot::for_level(f64::from(width), f64::from(height));
    let end = state.now;
    let start = end - state.range.seconds();
    let visible = visible_samples(&state.samples, start, end);

    draw_charging_bands(cr, &plot, &visible, start, end, colors);

    for (value, label) in [(100.0, "100%"), (50.0, "50%"), (0.0, "0%")] {
        let y = plot.y_at(value, 0.0, 100.0);
        line(cr, plot.x, y, plot.x + plot.width, y, colors.fg, 0.08, 1.0);
        text_right(
            cr,
            plot.x - 6.0,
            y + 4.0,
            10.5,
            colors.fg,
            0.55,
            label,
        );
    }

    draw_time_axis(cr, &plot, start, end, state.range, colors);

    for segment in segments(&visible) {
        let points: Vec<(f64, f64)> = segment
            .iter()
            .filter_map(|sample| {
                let level = sample.level_pct();
                Some((
                    plot.x_at(sample.ts, start, end),
                    plot.y_at(level, 0.0, 100.0),
                ))
            })
            .collect();
        if points.is_empty() {
            continue;
        }

        let points = thin(&points, (plot.width as usize).saturating_mul(2).max(16));

        if points.len() == 1 {
            // A lone sample between two gaps still deserves a dot.
            cr.arc(points[0].0, points[0].1, 2.0, 0.0, std::f64::consts::TAU);
            set_rgba(cr, colors.accent, 0.9);
            let _ = cr.fill();
            continue;
        }

        if points.len() > 1 {
            cr.move_to(points[0].0, plot.y + plot.height);
            for &(x, y) in &points {
                cr.line_to(x, y);
            }
            cr.line_to(points[points.len() - 1].0, plot.y + plot.height);
            cr.close_path();
            set_rgba(cr, colors.accent, 0.14);
            let _ = cr.fill();
        }

        cr.move_to(points[0].0, points[0].1);
        for &(x, y) in points.iter().skip(1) {
            cr.line_to(x, y);
        }
        set_rgba(cr, colors.accent, 1.0);
        cr.set_line_width(2.0);
        cr.set_line_join(cairo::LineJoin::Round);
        let _ = cr.stroke();
    }

    if let Some(hover) = state.hover_ts
        && hover >= start
        && hover <= end
    {
        let x = plot.x_at(hover, start, end);
        line(cr, x, plot.y, x, plot.y + plot.height, colors.fg, 0.35, 1.0);

        if let Some(sample) = nearest_sample(&visible, hover) {
            draw_tooltip(cr, &plot, x, sample, state.range, colors);
        }
    }
}

fn draw_power(
    colors: &Colors,
    cr: &cairo::Context,
    width: i32,
    height: i32,
    state: &ChartState,
) {
    let plot = Plot::for_power(f64::from(width), f64::from(height));
    let end = state.now;
    let start = end - state.range.seconds();
    let visible = visible_samples(&state.samples, start, end);

    let peak = visible
        .iter()
        .filter_map(|sample| sample.power_w)
        .fold(0.0_f64, f64::max);
    let max_w = nice_max(peak);

    for (fraction, label) in [(1.0, format_watt(max_w)), (0.5, format_watt(max_w / 2.0))] {
        let y = plot.y_at(max_w * fraction, 0.0, max_w);
        line(cr, plot.x, y, plot.x + plot.width, y, colors.fg, 0.08, 1.0);
        text_right(cr, plot.x - 6.0, y + 4.0, 10.5, colors.fg, 0.55, &label);
    }
    text_right(cr, plot.x - 6.0, plot.y + plot.height + 4.0, 10.5, colors.fg, 0.55, "0");

    draw_time_axis(cr, &plot, start, end, state.range, colors);

    for segment in segments(&visible) {
        let points: Vec<(f64, f64)> = segment
            .iter()
            .filter_map(|sample| {
                let power = sample.power_w?;
                Some((
                    plot.x_at(sample.ts, start, end),
                    plot.y_at(power, 0.0, max_w),
                ))
            })
            .collect();
        if points.len() < 2 {
            continue;
        }

        let points = thin(&points, (plot.width as usize).saturating_mul(2).max(16));
        if points.len() < 2 {
            cr.arc(points[0].0, points[0].1, 1.5, 0.0, std::f64::consts::TAU);
            set_rgba(cr, colors.accent, 0.85);
            let _ = cr.fill();
            continue;
        }
        cr.move_to(points[0].0, points[0].1);
        for &(x, y) in points.iter().skip(1) {
            cr.line_to(x, y);
        }
        set_rgba(cr, colors.accent, 0.85);
        cr.set_line_width(1.5);
        let _ = cr.stroke();
    }

    if let Some(hover) = state.hover_ts
        && hover >= start
        && hover <= end
    {
        let x = plot.x_at(hover, start, end);
        line(cr, x, plot.y, x, plot.y + plot.height, colors.fg, 0.35, 1.0);
    }
}

fn draw_health(
    colors: &Colors,
    cr: &cairo::Context,
    width: i32,
    height: i32,
    state: &ChartState,
) {
    if state.health.len() < 7 {
        return;
    }

    let plot = Plot {
        x: 0.0,
        y: 4.0,
        width: f64::from(width).max(1.0),
        height: (f64::from(height) - 8.0).max(1.0),
    };

    let values: Vec<f64> = state
        .health
        .iter()
        .filter_map(|day| day.health_pct())
        .collect();
    if values.len() < 7 {
        return;
    }

    let min = values.iter().copied().fold(f64::INFINITY, f64::min) - 0.5;
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max) + 0.5;
    let count = values.len().max(2) - 1;

    let points: Vec<(f64, f64)> = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            (
                plot.x + plot.width * index as f64 / count as f64,
                plot.y_at(*value, min, max),
            )
        })
        .collect();

    cr.move_to(points[0].0, points[0].1);
    for &(x, y) in points.iter().skip(1) {
        cr.line_to(x, y);
    }
    set_rgba(cr, colors.accent, 1.0);
    cr.set_line_width(1.5);
    let _ = cr.stroke();

    if let Some(&(x, y)) = points.last() {
        cr.arc(x - 2.0, y, 2.5, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}

fn draw_charging_bands(
    cr: &cairo::Context,
    plot: &Plot,
    samples: &[Sample],
    start: i64,
    end: i64,
    colors: &Colors,
) {
    for (band_start, band_end) in charging_bands(samples) {
        let x0 = plot.x_at(band_start, start, end);
        let x1 = plot.x_at(band_end, start, end);
        if x1 <= x0 {
            continue;
        }
        cr.rectangle(x0, plot.y, (x1 - x0).max(1.0), plot.height);
        set_rgba(cr, colors.accent, 0.10);
        let _ = cr.fill();
    }
}

fn draw_time_axis(
    cr: &cairo::Context,
    plot: &Plot,
    start: i64,
    end: i64,
    range: Range,
    colors: &Colors,
) {
    let ticks = match range {
        Range::Day => four_hour_ticks(start, end),
        Range::Week => midnight_ticks(start, end),
    };

    for ts in ticks {
        let x = plot.x_at(ts, start, end);
        line(
            cr,
            x,
            plot.y,
            x,
            plot.y + plot.height,
            colors.fg,
            0.05,
            1.0,
        );

        let label = match range {
            Range::Day => format::local_clock(ts).unwrap_or_default(),
            Range::Week => format::local_day_month(ts).unwrap_or_default(),
        };
        text_center(
            cr,
            x,
            plot.y + plot.height + 14.0,
            10.0,
            colors.fg,
            0.55,
            &label,
        );
    }
}

fn draw_tooltip(
    cr: &cairo::Context,
    plot: &Plot,
    crosshair_x: f64,
    sample: &Sample,
    range: Range,
    colors: &Colors,
) {
    let when = match range {
        Range::Day => format::local_clock(sample.ts).unwrap_or_default(),
        Range::Week => {
            let date = format::local_day_month(sample.ts).unwrap_or_default();
            let clock = format::local_clock(sample.ts).unwrap_or_default();
            format!("{date} {clock}")
        }
    };

    let power = sample
        .power_w
        .map(format_watt)
        .unwrap_or_else(|| "n/a".to_owned());
    let line1 = when;
    let line2 = format!(
        "{:.0}% · {power} · {}",
        sample.level_pct(),
        status_label(sample.status)
    );

    cr.select_font_face(
        "Sans",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Normal,
    );
    cr.set_font_size(11.0);
    let width1 = cr.text_extents(&line1).map(|e| e.width()).unwrap_or(0.0);
    let width2 = cr.text_extents(&line2).map(|e| e.width()).unwrap_or(0.0);
    let box_width = width1.max(width2) + 18.0;
    let box_height = 40.0;

    let mut x = crosshair_x + 12.0;
    if x + box_width > plot.x + plot.width {
        x = crosshair_x - box_width - 12.0;
    }
    let x = x.max(plot.x);
    let y = plot.y + 6.0;

    rounded_rect(cr, x, y, box_width, box_height, 8.0);
    set_rgba(cr, colors.fg, 0.10);
    let _ = cr.fill_preserve();
    set_rgba(cr, colors.fg, 0.22);
    cr.set_line_width(1.0);
    let _ = cr.stroke();

    text_left(cr, x + 9.0, y + 17.0, 11.0, colors.fg, 0.95, &line1);
    text_left(cr, x + 9.0, y + 32.0, 11.0, colors.fg, 0.75, &line2);
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let radius = radius.min(width / 2.0).min(height / 2.0);
    cr.new_sub_path();
    cr.arc(
        x + width - radius,
        y + radius,
        radius,
        -std::f64::consts::FRAC_PI_2,
        0.0,
    );
    cr.arc(
        x + width - radius,
        y + height - radius,
        radius,
        0.0,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        x + radius,
        y + height - radius,
        radius,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        x + radius,
        y + radius,
        radius,
        std::f64::consts::PI,
        1.5 * std::f64::consts::PI,
    );
    cr.close_path();
}

fn attach_hover(
    area: &gtk::DrawingArea,
    other: &gtk::DrawingArea,
    state: &Rc<RefCell<ChartState>>,
    which: Which,
) {
    let other_motion = glib::WeakRef::new();
    other_motion.set(Some(other));
    let other_leave = glib::WeakRef::new();
    other_leave.set(Some(other));

    let controller = gtk::EventControllerMotion::new();

    controller.connect_motion({
        let state = Rc::clone(state);
        move |controller, x, _y| {
            let Some(area) = controller.widget() else {
                return;
            };
            let plot = Plot::for_which(
                which,
                f64::from(area.width()).max(1.0),
                f64::from(area.height()).max(1.0),
            );

            let mut state = state.borrow_mut();
            let end = state.now;
            let start = end - state.range.seconds();
            state.hover_ts = Some(plot.ts_at(x, start, end));
            drop(state);

            if let Some(other) = other_motion.upgrade() {
                other.queue_draw();
            }
            area.queue_draw();
        }
    });

    controller.connect_leave({
        let state = Rc::clone(state);
        move |controller| {
            state.borrow_mut().hover_ts = None;
            if let Some(other) = other_leave.upgrade() {
                other.queue_draw();
            }
            if let Some(area) = controller.widget() {
                area.queue_draw();
            }
        }
    });

    area.add_controller(controller);
}

fn redraw_on_theme_change(areas: &[&gtk::DrawingArea]) {
    let style = adw::StyleManager::default();

    for area in areas {
        let dark_area = (*area).clone();
        style.connect_dark_notify(move |_| dark_area.queue_draw());

        let accent_area = (*area).clone();
        style.connect_accent_color_rgba_notify(move |_| accent_area.queue_draw());
    }
}

fn visible_samples(samples: &[Sample], start: i64, end: i64) -> Vec<Sample> {
    samples
        .iter()
        .filter(|sample| sample.ts >= start && sample.ts <= end)
        .cloned()
        .collect()
}

fn nearest_sample(samples: &[Sample], ts: i64) -> Option<&Sample> {
    samples.iter().min_by_key(|sample| (sample.ts - ts).abs())
}

/// Split a series wherever the gap is too large to draw a straight line.
fn segments(samples: &[Sample]) -> Vec<&[Sample]> {
    if samples.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut begin = 0;
    for index in 1..samples.len() {
        if samples[index].ts - samples[index - 1].ts > GAP_SECS {
            out.push(&samples[begin..index]);
            begin = index;
        }
    }
    out.push(&samples[begin..]);
    out
}

/// Where the battery was actually charging, merged across contiguous samples.
fn charging_bands(samples: &[Sample]) -> Vec<(i64, i64)> {
    let mut bands = Vec::new();
    let mut open: Option<i64> = None;

    for (index, sample) in samples.iter().enumerate() {
        let gap_after = samples
            .get(index + 1)
            .is_some_and(|next| next.ts - sample.ts > GAP_SECS);

        if sample.status == Status::Charging {
            open.get_or_insert(sample.ts);
        } else if let Some(start) = open.take() {
            bands.push((start, sample.ts));
        }

        if gap_after && let Some(start) = open.take() {
            bands.push((start, sample.ts));
        }
    }

    if let Some(start) = open {
        let end = samples.last().map(|sample| sample.ts).unwrap_or(start);
        bands.push((start, end));
    }

    bands
}

/// Keep at most `max_points` samples while preserving per-bucket extremes, so
/// a short spike in the power plot does not disappear.
fn thin(points: &[(f64, f64)], max_points: usize) -> Vec<(f64, f64)> {
    if points.len() <= max_points || max_points < 4 {
        return points.to_vec();
    }

    let bucket = points.len().div_ceil(max_points / 2);
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(max_points);

    for chunk in points.chunks(bucket) {
        let mut lowest = chunk[0];
        let mut highest = chunk[0];
        for &point in chunk {
            if point.1 < lowest.1 {
                lowest = point;
            }
            if point.1 > highest.1 {
                highest = point;
            }
        }
        if lowest.0 <= highest.0 {
            out.push(lowest);
            if highest.0 != lowest.0 {
                out.push(highest);
            }
        } else {
            out.push(highest);
            out.push(lowest);
        }
    }

    if let Some(last) = points.last()
        && out.last() != Some(last)
    {
        out.push(*last);
    }
    out
}

fn four_hour_ticks(start: i64, end: i64) -> Vec<i64> {
    let mut tick = local_midnight(start);
    while tick < start {
        tick += 4 * 3_600;
    }

    let mut ticks = Vec::new();
    while tick <= end && ticks.len() < 12 {
        ticks.push(tick);
        tick += 4 * 3_600;
    }
    ticks
}

fn midnight_ticks(start: i64, end: i64) -> Vec<i64> {
    let mut tick = local_midnight(start);
    while tick < start {
        tick += 86_400;
    }

    let mut ticks = Vec::new();
    while tick <= end && ticks.len() < 10 {
        ticks.push(tick);
        tick += 86_400;
    }
    ticks
}

/// Unix timestamp of local midnight for the day containing `ts`.
fn local_midnight(ts: i64) -> i64 {
    format::local_seconds_of_day(ts)
        .map(|seconds| ts - seconds)
        .unwrap_or(ts)
}

fn nice_max(peak: f64) -> f64 {
    let peak = if peak.is_finite() { peak.max(0.0) } else { 0.0 };
    if peak <= 5.0 {
        5.0
    } else if peak <= 10.0 {
        10.0
    } else {
        (peak / 5.0).ceil() * 5.0
    }
}

fn format_watt(watts: f64) -> String {
    if watts >= 10.0 {
        format!("{watts:.0} W")
    } else {
        format!("{watts:.1} W")
    }
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Charging => "charging",
        Status::Discharging => "discharging",
        Status::Full => "full",
        Status::NotCharging => "not charging",
        Status::Unknown => "unknown",
    }
}

fn set_rgba(cr: &cairo::Context, color: (f64, f64, f64), alpha: f64) {
    cr.set_source_rgba(color.0, color.1, color.2, alpha);
}

fn line(
    cr: &cairo::Context,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    color: (f64, f64, f64),
    alpha: f64,
    width: f64,
) {
    set_rgba(cr, color, alpha);
    cr.set_line_width(width);
    cr.move_to(x0, y0);
    cr.line_to(x1, y1);
    let _ = cr.stroke();
}

fn text_left(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    size: f64,
    color: (f64, f64, f64),
    alpha: f64,
    text: &str,
) {
    cr.select_font_face(
        "Sans",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Normal,
    );
    cr.set_font_size(size);
    set_rgba(cr, color, alpha);
    cr.move_to(x, y);
    let _ = cr.show_text(text);
}

fn text_right(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    size: f64,
    color: (f64, f64, f64),
    alpha: f64,
    text: &str,
) {
    cr.select_font_face(
        "Sans",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Normal,
    );
    cr.set_font_size(size);
    let width = cr.text_extents(text).map(|extents| extents.width()).unwrap_or(0.0);
    text_left(cr, x - width, y, size, color, alpha, text);
}

fn text_center(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    size: f64,
    color: (f64, f64, f64),
    alpha: f64,
    text: &str,
) {
    cr.select_font_face(
        "Sans",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Normal,
    );
    cr.set_font_size(size);
    let width = cr.text_extents(text).map(|extents| extents.width()).unwrap_or(0.0);
    text_left(cr, x - width / 2.0, y, size, color, alpha, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, status: Status, energy: f64, power: Option<f64>) -> Sample {
        Sample {
            ts,
            status,
            ac_online: status.is_on_ac(),
            energy_wh: energy,
            full_wh: 31.0,
            power_w: power,
            voltage_v: Some(12.0),
            temp_c: None,
        }
    }

    fn series(gaps: &[(i64, Status)]) -> Vec<Sample> {
        gaps.iter()
            .enumerate()
            .map(|(index, (ts, status))| sample(*ts, *status, 30.0 - index as f64, Some(8.0)))
            .collect()
    }

    #[test]
    fn segments_break_on_long_gaps() {
        let samples = series(&[(0, Status::Discharging), (60, Status::Discharging), (300, Status::Discharging)]);
        let segments = segments(&samples);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].len(), 2);
        assert_eq!(segments[1].len(), 1);
    }

    #[test]
    fn segments_keep_three_minute_cadence_together() {
        let samples = series(&[(0, Status::Discharging), (180, Status::Discharging)]);
        assert_eq!(segments(&samples).len(), 1);
    }

    #[test]
    fn charging_bands_cover_contiguous_charge() {
        let samples = series(&[
            (0, Status::Discharging),
            (60, Status::Charging),
            (120, Status::Charging),
            (180, Status::Discharging),
        ]);
        assert_eq!(charging_bands(&samples), vec![(60, 180)]);
    }

    #[test]
    fn charging_band_stops_at_a_gap() {
        let samples = series(&[
            (0, Status::Charging),
            (60, Status::Charging),
            (3_600, Status::Charging),
        ]);
        let bands = charging_bands(&samples);
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0], (0, 60));
        assert_eq!(bands[1], (3_600, 3_600));
    }

    #[test]
    fn charging_band_closes_at_the_end() {
        let samples = series(&[(0, Status::Charging), (60, Status::Charging)]);
        assert_eq!(charging_bands(&samples), vec![(0, 60)]);
    }

    #[test]
    fn no_bands_when_never_charging() {
        let samples = series(&[(0, Status::Discharging), (60, Status::Discharging)]);
        assert!(charging_bands(&samples).is_empty());
    }

    #[test]
    fn thin_preserves_first_last_and_spikes() {
        let mut points: Vec<(f64, f64)> = (0..1_000).map(|i| (i as f64, 1.0)).collect();
        points[500].1 = 40.0;

        let thinned = thin(&points, 100);
        assert!(thinned.len() <= 104, "got {}", thinned.len());
        assert_eq!(thinned.first(), Some(&(0.0, 1.0)));
        assert_eq!(thinned.last(), Some(&(999.0, 1.0)));
        assert!(
            thinned.iter().any(|point| point.1 == 40.0),
            "the spike must survive downsampling"
        );
    }

    #[test]
    fn thin_keeps_short_series_untouched() {
        let points = vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)];
        assert_eq!(thin(&points, 100), points);
    }

    #[test]
    fn nice_max_has_a_sane_floor() {
        assert_eq!(nice_max(0.0), 5.0);
        assert_eq!(nice_max(3.2), 5.0);
        assert_eq!(nice_max(7.8), 10.0);
        assert_eq!(nice_max(17.5), 20.0);
        assert_eq!(nice_max(f64::NAN), 5.0);
    }

    #[test]
    fn watt_labels_switch_precision() {
        assert_eq!(format_watt(4.6), "4.6 W");
        assert_eq!(format_watt(17.5), "18 W");
    }

    #[test]
    fn ticks_are_four_hourly_and_bounded() {
        let start = 1_765_000_000;
        let ticks = four_hour_ticks(start, start + 86_400);
        assert!(ticks.len() >= 5 && ticks.len() <= 7, "got {}", ticks.len());
        assert!(ticks.iter().all(|tick| *tick >= start));
        assert!(
            ticks
                .windows(2)
                .all(|pair| pair[1] - pair[0] == 4 * 3_600)
        );
    }

    #[test]
    fn midnight_ticks_are_daily() {
        let start = 1_765_000_000;
        let ticks = midnight_ticks(start, start + 7 * 86_400);
        assert!(ticks.len() >= 6);
        assert!(ticks.windows(2).all(|pair| pair[1] - pair[0] == 86_400));
    }

    #[test]
    fn plot_maps_time_and_level() {
        let plot = Plot {
            x: 40.0,
            y: 10.0,
            width: 600.0,
            height: 200.0,
        };
        assert!((plot.x_at(0, 0, 100) - 40.0).abs() < 1e-6);
        assert!((plot.x_at(100, 0, 100) - 640.0).abs() < 1e-6);
        assert!((plot.y_at(100.0, 0.0, 100.0) - 10.0).abs() < 1e-6);
        assert!((plot.y_at(0.0, 0.0, 100.0) - 210.0).abs() < 1e-6);
        assert_eq!(plot.ts_at(40.0, 0, 100), 0);
        assert_eq!(plot.ts_at(640.0, 0, 100), 100);
    }

    #[test]
    fn nearest_sample_finds_the_closest_in_time() {
        let samples = series(&[(0, Status::Discharging), (600, Status::Discharging)]);
        assert_eq!(nearest_sample(&samples, 100).unwrap().ts, 0);
        assert_eq!(nearest_sample(&samples, 500).unwrap().ts, 600);
        assert!(nearest_sample(&[], 0).is_none());
    }

    fn painted_pixels(state: &ChartState) -> usize {
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 600, 220).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        let colors = Colors {
            fg: (1.0, 1.0, 1.0),
            accent: (0.2, 0.5, 1.0),
        };

        draw_level(&colors, &cr, 600, 220, state);
        draw_power(&colors, &cr, 600, 220, state);
        draw_health(&colors, &cr, 600, 56, state);
        drop(cr);
        surface.flush();

        let data = surface.data().unwrap();
        data.iter().filter(|byte| **byte != 0).count()
    }

    #[test]
    fn charts_paint_a_day_of_history_with_a_crosshair() {
        let now = 1_765_000_000;
        let full = 31.0;
        let mut samples = Vec::new();

        for step in 0..280_i64 {
            let ts = now - 24 * 3_600 + step * 300;
            // A gap in the middle, as if the laptop slept.
            if (90..160).contains(&step) {
                continue;
            }
            let status = if step < 80 {
                Status::Discharging
            } else {
                Status::Charging
            };
            let level = 0.9 - step as f64 * 0.002;
            samples.push(Sample {
                ts,
                status,
                ac_online: status.is_on_ac(),
                energy_wh: full * level.clamp(0.2, 1.0),
                full_wh: full,
                power_w: Some(if status == Status::Charging { 18.0 } else { 7.5 }),
                voltage_v: Some(12.0),
                temp_c: None,
            });
        }

        let health = (0..10)
            .map(|day| DailyHealth {
                day: format!("2026-11-{:02}", day + 9),
                full_wh: 31.0 - day as f64 * 0.05,
                design_wh: 42.0,
                cycle_count: Some(333),
                est_cycles: None,
            })
            .collect();

        let state = ChartState {
            samples,
            health,
            range: Range::Day,
            hover_ts: Some(now - 3 * 3_600),
            now,
        };

        assert!(painted_pixels(&state) > 1_000, "the charts should paint content");
    }

    #[test]
    fn empty_charts_paint_only_the_axes() {
        let state = ChartState {
            samples: Vec::new(),
            health: Vec::new(),
            range: Range::Week,
            hover_ts: None,
            now: 1_765_000_000,
        };
        // Should not panic, and the empty sparkline must stay blank.
        assert!(painted_pixels(&state) > 0);
    }
}
