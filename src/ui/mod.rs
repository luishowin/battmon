//! GTK/libadwaita front end.

pub mod cards;
pub mod chart;
pub mod window;

/// Only layout is styled here; every colour comes from the Adwaita theme, so
/// dark/light and the accent colour follow the system automatically.
const CSS: &str = r#"
.hero-level {
  font-size: 54px;
  font-weight: 800;
}
.hero-detail {
  font-size: 15px;
}
.stat-card {
  padding: 14px 16px;
}
.stat-value {
  font-size: 22px;
  font-weight: 700;
}
"#;

pub fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);

    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
