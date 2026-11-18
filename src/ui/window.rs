//! Main window: hero block, stat cards, live refresh and empty states.

use crate::ui::cards::{StatCard, stat_card};
use crate::ui::chart::Charts;
use adw::prelude::*;
use battmon::estimate::{LiveEstimate, Tracker};
use battmon::format;
use battmon::model::{BatteryInfo, Reading, Status};
use battmon::store::{Sample, Store, default_db_path};
use battmon::sysfs::Sysfs;
use gtk::glib;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// Live sysfs refresh while the window is visible.
const LIVE_SECS: u32 = 5;
/// History re-query.
const HISTORY_SECS: u32 = 60;
/// How much history is replayed into the estimator on startup.
const SEED_WINDOW_SECS: i64 = 3_600;
/// Loaded once a minute so both chart ranges switch without a query.
const HISTORY_WINDOW_SECS: i64 = 8 * 86_400;

const BANNER_NO_HISTORY: &str = "History is collected by the background service. \
                                  Run systemctl --user enable --now battmond.";

pub fn build(app: &adw::Application) -> adw::ApplicationWindow {
    let sysfs = Sysfs::from_env();
    let info = sysfs
        .battery()
        .map(|battery| battery.info)
        .unwrap_or_default();

    let widgets = Widgets::build(&info, &sysfs);
    let db_path = match std::env::var_os("BATTMON_DB") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => default_db_path().unwrap_or_else(|_| PathBuf::from("battmon.db")),
    };

    let state = Rc::new(RefCell::new(State {
        sysfs,
        tracker: Tracker::new(),
        store: None,
        db_path,
        widgets: widgets.clone(),
        last_reading: None,
        seeded: false,
        median_discharge_w: None,
    }));

    let timers = Rc::new(RefCell::new(Timers::default()));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Battery")
        .default_width(720)
        .default_height(800)
        .content(&widgets.view)
        .build();

    window.connect_visible_notify({
        let state = Rc::clone(&state);
        let timers = Rc::clone(&timers);
        move |window| {
            if window.is_visible() {
                start_timers(&state, &timers);
                refresh_all(&state);
            } else {
                stop_timers(&timers);
            }
        }
    });

    // Open the database and replay recent history before the first paint, so
    // the estimator does not start from "estimating…" on every launch.
    {
        let mut state = state.borrow_mut();
        state.ensure_store();
        seed_tracker(&mut state);
    }
    refresh_all(&state);

    window.present();
    window
}

#[derive(Default)]
struct Timers {
    live: Option<glib::SourceId>,
    history: Option<glib::SourceId>,
}

#[derive(Clone)]
struct Widgets {
    view: adw::ToolbarView,
    stack: gtk::Stack,
    banner: adw::Banner,
    level: gtk::Label,
    detail: gtk::Label,
    typical: gtk::Label,
    health: StatCard,
    cycles: StatCard,
    power: StatCard,
    extra: Option<StatCard>,
    charts: Charts,
    footer: gtk::Label,
    footer_base: String,
}

impl Widgets {
    fn build(info: &BatteryInfo, sysfs: &Sysfs) -> Self {
        let subtitle = if info.name.is_empty() {
            "No battery".to_owned()
        } else {
            info.display_name()
        };

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Battery", &subtitle)));

        let banner = adw::Banner::new(BANNER_NO_HISTORY);
        banner.set_revealed(false);

        let level = gtk::Label::new(Some("…"));
        level.add_css_class("hero-level");
        level.set_justify(gtk::Justification::Center);

        let detail = gtk::Label::new(None);
        detail.add_css_class("hero-detail");
        detail.add_css_class("dim-label");
        detail.set_wrap(true);
        detail.set_justify(gtk::Justification::Center);

        let typical = gtk::Label::new(None);
        typical.add_css_class("caption");
        typical.add_css_class("dim-label");
        typical.set_visible(false);

        let hero = gtk::Box::new(gtk::Orientation::Vertical, 2);
        hero.append(&level);
        hero.append(&detail);
        hero.append(&typical);

        let cards = adw::WrapBox::new();
        cards.set_child_spacing(12);
        cards.set_line_spacing(12);
        cards.set_justify(adw::JustifyMode::Fill);
        cards.set_natural_line_length(0);

        let health = stat_card("Health");
        let cycles = stat_card("Cycles");
        let power = stat_card("Power");
        cards.append(&health.root);
        cards.append(&cycles.root);
        cards.append(&power.root);

        // Temperature and charge limits are absent on this HP; the card is only
        // built when the firmware actually has one of them.
        let extra = if info.has_temp {
            let card = stat_card("Temperature");
            cards.append(&card.root);
            Some(card)
        } else if info.has_charge_limit {
            let card = stat_card("Charge limit");
            cards.append(&card.root);
            Some(card)
        } else {
            None
        };

        let charts = Charts::new();

        let footer = gtk::Label::new(Some(&subtitle));
        footer.add_css_class("caption");
        footer.add_css_class("dim-label");
        footer.set_justify(gtk::Justification::Center);
        footer.set_margin_top(6);

        let main_page = gtk::Box::new(gtk::Orientation::Vertical, 18);
        main_page.set_margin_top(12);
        main_page.set_margin_bottom(28);
        main_page.set_margin_start(18);
        main_page.set_margin_end(18);
        main_page.append(&banner);
        main_page.append(&hero);
        main_page.append(&cards);
        main_page.append(&charts.root);
        main_page.append(&footer);

        let status_page = adw::StatusPage::builder()
            .icon_name("battery-missing-symbolic")
            .title("No battery detected")
            .description(format!(
                "Nothing with type=Battery was found under {}.",
                sysfs.power_supply_dir().display()
            ))
            .build();

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);
        stack.add_named(&main_page, Some("main"));
        stack.add_named(&status_page, Some("status"));
        stack.set_visible_child_name("main");

        // Threshold above maximum: AdwClamp then behaves as a hard cap at
        // 720 px instead of easing the page narrower across a 960 px ramp.
        let clamp = adw::Clamp::builder()
            .maximum_size(720)
            .tightening_threshold(1000)
            .child(&stack)
            .build();

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&scroll));

        Self {
            view,
            stack,
            banner,
            level,
            detail,
            typical,
            health,
            cycles,
            power,
            extra,
            charts,
            footer,
            footer_base: subtitle,
        }
    }
}

struct State {
    sysfs: Sysfs,
    tracker: Tracker,
    store: Option<Store>,
    db_path: PathBuf,
    widgets: Widgets,
    last_reading: Option<Reading>,
    seeded: bool,
    median_discharge_w: Option<f64>,
}

impl State {
    /// Open the history database, reporting the reason in the banner if it is
    /// missing (daemon never started) or unreadable.
    fn ensure_store(&mut self) -> bool {
        if self.store.is_some() {
            return true;
        }

        match Store::open_read_only(&self.db_path) {
            Ok(store) => {
                self.store = Some(store);
                true
            }
            Err(err) => {
                let message = if self.db_path.exists() {
                    format!(
                        "History unavailable: {}",
                        short_reason(&err.to_string())
                    )
                } else {
                    BANNER_NO_HISTORY.to_owned()
                };
                self.widgets.banner.set_title(&message);
                self.widgets.banner.set_revealed(true);
                false
            }
        }
    }

    /// "Typically 3 h 40 min at this level", from the median discharge draw over
    /// the last week. Optional extra from spec §6.2.
    fn paint_typical(&self, reading: &Reading) {
        let runtime = if reading.status == Status::Discharging {
            match (reading.energy_wh, self.median_discharge_w) {
                (Some(energy), Some(median)) => {
                    battmon::estimate::typical_runtime_secs(energy, median)
                }
                _ => None,
            }
        } else {
            None
        };

        match runtime {
            Some(seconds) => {
                self.widgets.typical.set_text(&format!(
                    "Typically {} at this level",
                    format::format_duration(seconds)
                ));
                self.widgets.typical.set_visible(true);
            }
            None => self.widgets.typical.set_visible(false),
        }
    }

    fn estimated_cycles(&self, reading: &Reading) -> Option<(f64, Option<String>)> {        let store = self.store.as_ref()?;
        let cycles = battmon::estimate::estimate_cycles(
            store.cumulative_discharge_wh().ok()?,
            reading.design_wh?,
        )?;
        let since = store
            .meta("tracking_since")
            .ok()
            .flatten()
            .and_then(|raw| raw.parse::<i64>().ok())
            .and_then(|ts| format::local_date_human(ts).ok());
        Some((cycles, since))
    }

    fn paint(&self, reading: &Reading, estimate: &LiveEstimate) {
        let widgets = &self.widgets;

        widgets.level.set_text(&match reading.level_pct() {
            Some(level) => format!("{level:.0}%"),
            None => "n/a".to_owned(),
        });
        widgets.detail.set_text(&hero_text(reading, estimate));
        self.paint_typical(reading);

        match reading.health_pct() {
            Some(health) => {
                let subtitle = match (reading.full_wh, reading.design_wh) {
                    (Some(full), Some(design)) => format!("{full:.1} / {design:.1} Wh"),
                    _ => String::new(),
                };
                widgets.health.set(&format!("{health:.0}%"), &subtitle);
            }
            None => widgets.health.set("n/a", ""),
        }

        match reading.cycle_count {
            Some(cycles) => widgets.cycles.set(&cycles.to_string(), "reported by firmware"),
            None => match self.estimated_cycles(reading) {
                Some((cycles, since)) => {
                    let subtitle = match since {
                        Some(day) => format!("estimated · since {day}"),
                        None => "estimated".to_owned(),
                    };
                    widgets.cycles.set(&format!("≈ {cycles:.0}"), &subtitle);
                }
                None => widgets.cycles.set("n/a", "no cycle data"),
            },
        }

        match reading.power_w {
            Some(power) => {
                let subtitle = match (reading.status, reading.rate_pct_per_h()) {
                    (Status::Charging, Some(rate)) => format!("+{rate:.1} %/h"),
                    (Status::Discharging, Some(rate)) => format!("−{rate:.1} %/h"),
                    _ => String::new(),
                };
                widgets.power.set(&format!("{power:.1} W"), &subtitle);
            }
            None => widgets.power.set("n/a", ""),
        }

        if let Some(extra) = &widgets.extra {
            if let Some(temp) = reading.temp_c {
                extra.set(&format!("{temp:.1} °C"), "battery");
            } else if let Some(limit) = reading.charge_limit_pct {
                extra.set(&format!("{limit}%"), "charge limit");
            } else {
                extra.set("n/a", "");
            }
        }

        match reading.design_wh {
            Some(design) => widgets.footer.set_text(&format!(
                "{} · design capacity {design:.1} Wh",
                widgets.footer_base
            )),
            None => widgets.footer.set_text(&widgets.footer_base),
        }
    }
}

/// Replay recent samples so the estimator is warm at startup instead of showing
/// "estimating…" for the first two minutes after every launch.
fn seed_tracker(state: &mut State) {
    if state.seeded {
        return;
    }
    state.seeded = true;

    let Some(store) = &state.store else {
        return;
    };
    let since = format::now_secs() - SEED_WINDOW_SECS;
    let Ok(samples) = store.samples_since(since) else {
        return;
    };
    for sample in samples {
        state.tracker.observe(&sample.to_reading());
    }
}

fn refresh_all(state: &Rc<RefCell<State>>) {
    refresh_history(state);
    refresh_live(state);
}

fn refresh_live(state: &Rc<RefCell<State>>) {
    let mut state = state.borrow_mut();

    let Some(reading) = state.sysfs.read(format::now_secs()) else {
        state.widgets.stack.set_visible_child_name("status");
        state.widgets.banner.set_revealed(false);
        return;
    };

    state.widgets.stack.set_visible_child_name("main");
    state.tracker.observe(&reading);
    let estimate = state.tracker.estimate(&reading);
    state.paint(&reading, &estimate);
    state.last_reading = Some(reading);
}

fn refresh_history(state: &Rc<RefCell<State>>) {
    let mut state = state.borrow_mut();
    if !state.ensure_store() {
        return;
    }

    let now = format::now_secs();
    let (samples, health) = match &state.store {
        Some(store) => (
            store
                .samples_since(now - HISTORY_WINDOW_SECS)
                .unwrap_or_default(),
            store.daily_health().unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new()),
    };

    if samples.is_empty() {
        state.widgets.banner.set_title(BANNER_NO_HISTORY);
        state.widgets.banner.set_revealed(true);
    } else {
        state.widgets.banner.set_revealed(false);
    }

    state.median_discharge_w = median_discharge_power(&samples, now);
    state.widgets.charts.set_data(samples, health);

    // The cycle estimate depends on the store, so repaint once it is open.
    if let Some(reading) = state.last_reading.clone() {
        let estimate = state.tracker.estimate(&reading);
        state.paint(&reading, &estimate);
    }
}

/// Median draw of the last week's discharging samples, for the "typically"
/// line. Robust against the spike from a compile or a video call.
///
/// Needs an hour of discharging samples before it is shown: a median of five
/// readings is noise pretending to be a habit.
fn median_discharge_power(samples: &[Sample], now: i64) -> Option<f64> {
    let cutoff = now - 7 * 86_400;
    let powers: Vec<f64> = samples
        .iter()
        .filter(|sample| sample.ts >= cutoff && sample.status == Status::Discharging)
        .filter_map(|sample| sample.power_w)
        .collect();

    if powers.len() < 60 {
        return None;
    }
    battmon::estimate::median_power_w(powers)
}

fn start_timers(state: &Rc<RefCell<State>>, timers: &Rc<RefCell<Timers>>) {
    let mut timers = timers.borrow_mut();

    if timers.live.is_none() {
        let state = Rc::clone(state);
        timers.live = Some(glib::timeout_add_seconds_local(LIVE_SECS, move || {
            refresh_live(&state);
            glib::ControlFlow::Continue
        }));
    }

    if timers.history.is_none() {
        let state = Rc::clone(state);
        timers.history = Some(glib::timeout_add_seconds_local(HISTORY_SECS, move || {
            refresh_history(&state);
            glib::ControlFlow::Continue
        }));
    }
}

fn stop_timers(timers: &Rc<RefCell<Timers>>) {
    let mut timers = timers.borrow_mut();
    if let Some(id) = timers.live.take() {
        id.remove();
    }
    if let Some(id) = timers.history.take() {
        id.remove();
    }
}

/// One-line status under the big number.
fn hero_text(reading: &Reading, estimate: &LiveEstimate) -> String {
    match reading.status {
        Status::Charging => match estimate.time_to_full_secs {
            Some(seconds) if seconds > 0.0 => format!(
                "Charging · {} to full",
                format::format_duration_fine(seconds)
            ),
            Some(_) => "Charging · almost full".to_owned(),
            None => "Charging · estimating…".to_owned(),
        },
        Status::Discharging => match estimate.time_left_secs {
            Some(seconds) => format!(
                "Discharging · about {} left",
                format::format_duration(seconds)
            ),
            None if estimate.warming_up => "Discharging · estimating…".to_owned(),
            None => "Discharging".to_owned(),
        },
        Status::Full => "Plugged in · fully charged".to_owned(),
        Status::NotCharging => "Plugged in · not charging".to_owned(),
        Status::Unknown => match reading.ac_online {
            Some(true) => "Plugged in".to_owned(),
            Some(false) => "On battery".to_owned(),
            None => "Status unavailable".to_owned(),
        },
    }
}

/// Keep the banner readable rather than dumping a whole error chain.
fn short_reason(message: &str) -> String {
    message.lines().next().unwrap_or(message).to_owned()
}


#[cfg(test)]
mod tests {
    use super::*;

    fn reading(status: Status) -> Reading {
        Reading {
            ts: 0,
            status,
            ac_online: Some(status.is_on_ac()),
            energy_wh: Some(20.0),
            full_wh: Some(31.0),
            design_wh: Some(42.0),
            power_w: Some(9.0),
            voltage_v: Some(12.0),
            cycle_count: None,
            temp_c: None,
            charge_limit_pct: None,
            capacity_pct: None,
        }
    }

    #[test]
    fn charging_line_uses_time_to_full() {
        let estimate = LiveEstimate {
            time_to_full_secs: Some(38.0 * 60.0),
            ..LiveEstimate::default()
        };
        assert_eq!(
            hero_text(&reading(Status::Charging), &estimate),
            "Charging · 38 min to full"
        );
    }

    #[test]
    fn discharging_line_uses_time_left() {
        let estimate = LiveEstimate {
            time_left_secs: Some(4.0 * 3_600.0 + 20.0 * 60.0),
            ..LiveEstimate::default()
        };
        assert_eq!(
            hero_text(&reading(Status::Discharging), &estimate),
            "Discharging · about 4 h 20 min left"
        );
    }

    #[test]
    fn warming_up_says_estimating() {
        let estimate = LiveEstimate {
            warming_up: true,
            ..LiveEstimate::default()
        };
        assert_eq!(
            hero_text(&reading(Status::Discharging), &estimate),
            "Discharging · estimating…"
        );
    }

    #[test]
    fn held_and_full_states_are_explicit() {
        assert_eq!(
            hero_text(&reading(Status::NotCharging), &LiveEstimate::default()),
            "Plugged in · not charging"
        );
        assert_eq!(
            hero_text(&reading(Status::Full), &LiveEstimate::default()),
            "Plugged in · fully charged"
        );
    }

    #[test]
    fn unknown_status_falls_back_to_ac_state() {
        let mut reading = reading(Status::Unknown);
        reading.ac_online = Some(false);
        assert_eq!(hero_text(&reading, &LiveEstimate::default()), "On battery");
    }

    #[test]
    fn near_zero_estimate_does_not_say_zero_minutes() {
        let estimate = LiveEstimate {
            time_to_full_secs: Some(0.0),
            ..LiveEstimate::default()
        };
        assert_eq!(
            hero_text(&reading(Status::Charging), &estimate),
            "Charging · almost full"
        );
    }

    #[test]
    fn banner_reason_is_one_line() {
        assert_eq!(
            short_reason("first line\nsecond line"),
            "first line".to_owned()
        );
    }
}
