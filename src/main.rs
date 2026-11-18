//! Application entry point.

mod ui;

use adw::prelude::*;
use gtk::glib;

const APP_ID: &str = "design.beben.Battmon";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| ui::install_css());
    app.connect_activate(|app| {
        let window = ui::window::build(app);

        // Temporary QA hook: render the window to a file and exit.
        if let Ok(path) = std::env::var("BATTMON_SNAPSHOT") {
            glib::timeout_add_seconds_local(2, move || {
                ui::snapshot(&window, &path);
                std::process::exit(0)
            });
        }
    });

    app.run()
}
