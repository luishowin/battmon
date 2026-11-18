//! Application entry point.

mod ui;

use adw::prelude::*;
use gtk::glib;

const APP_ID: &str = "design.beben.Battmon";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| ui::install_css());
    app.connect_activate(|app| {
        ui::window::build(app);
    });

    app.run()
}
