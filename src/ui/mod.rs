//! GTK/libadwaita front end.

pub mod cards;
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

/// Render a widget to a PNG. Temporary QA hook used to check layout without a
/// screenshot portal; not part of the normal UI flow.
pub fn snapshot(window: &impl gtk::glib::prelude::IsA<gtk::Widget>, path: &str) {
    use gtk::gdk::prelude::*;
    use gtk::prelude::*;

    let paintable = gtk::WidgetPaintable::new(Some(window));
    let width = f64::from(window.width().max(1));
    let height = f64::from(window.height().max(1));

    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width, height);

    let Some(node) = snapshot.to_node() else {
        eprintln!("snapshot: widget produced no render node");
        return;
    };

    let renderer = gtk::gsk::CairoRenderer::new();
    let Some(display) = gtk::gdk::Display::default() else {
        eprintln!("snapshot: no display");
        return;
    };
    if let Err(err) = renderer.realize_for_display(&display) {
        eprintln!("snapshot: realizing renderer failed: {err}");
        return;
    }

    let texture = renderer.render_texture(&node, None);
    if let Err(err) = texture.save_to_png(path) {
        eprintln!("snapshot: saving {path} failed: {err}");
    }
    dump_layout(window.upcast_ref(), 0);
}

fn dump_layout(widget: &gtk::Widget, depth: usize) {
    use gtk::glib::prelude::*;
    use gtk::prelude::*;

    eprintln!(
        "{}{} {}x{}",
        "  ".repeat(depth),
        widget.type_().name(),
        widget.width(),
        widget.height()
    );
    let mut child = widget.first_child();
    while let Some(child_widget) = child {
        dump_layout(&child_widget, depth + 1);
        child = child_widget.next_sibling();
    }
}
