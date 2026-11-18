//! Small stat cards used in the summary grid.

use adw::prelude::*;
use gtk::pango;

/// A title / big value / subtitle card.
#[derive(Clone)]
pub struct StatCard {
    pub root: gtk::Box,
    pub value: gtk::Label,
    pub subtitle: gtk::Label,
}

/// Build a card. `title` is fixed; the value and subtitle are updated live.
pub fn stat_card(title: &str) -> StatCard {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.add_css_class("card");
    root.add_css_class("stat-card");
    root.set_size_request(200, -1);
    root.set_hexpand(true);

    let title_label = gtk::Label::new(Some(title));
    title_label.set_xalign(0.0);
    title_label.add_css_class("caption");
    title_label.add_css_class("dim-label");

    let value = gtk::Label::new(None);
    value.set_xalign(0.0);
    value.add_css_class("stat-value");

    let subtitle = gtk::Label::new(None);
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("caption");
    subtitle.add_css_class("dim-label");
    subtitle.set_ellipsize(pango::EllipsizeMode::End);

    root.append(&title_label);
    root.append(&value);
    root.append(&subtitle);

    StatCard {
        root,
        value,
        subtitle,
    }
}

impl StatCard {
    pub fn set(&self, value: &str, subtitle: &str) {
        self.value.set_text(value);
        self.subtitle.set_text(subtitle);
        self.subtitle.set_visible(!subtitle.is_empty());
    }

    /// Tint the card's numbers with the theme's success colour.
    pub fn set_success(&self, success: bool) {
        for label in [&self.value, &self.subtitle] {
            if success {
                label.add_css_class("success");
            } else {
                label.remove_css_class("success");
            }
        }
    }
}
