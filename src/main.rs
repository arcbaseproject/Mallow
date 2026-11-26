mod app;
mod config;
mod downloads;
mod engine;
mod history;
mod lists;
mod passwords;
mod settings;
mod store;
mod suggest;
mod theme;
mod update;
mod util;
mod view;
mod window;

use gtk::{gio, glib, prelude::*};
use std::{cell::OnceCell, rc::Rc};

fn main() -> glib::ExitCode {
    gio::resources_register_include!("mallow.gresource").expect("embedded resources are valid");
    // Single instance: a second launch opens a window in the running process.
    let gapp = gtk::Application::builder()
        .application_id(app::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    let state: Rc<OnceCell<Rc<app::App>>> = Rc::default();
    gapp.connect_startup({
        let state = state.clone();
        move |g| {
            let _ = state.set(app::App::new(g));
            // SIGHUP, SIGINT, SIGTERM: quit normally so the session is saved,
            // without asking about downloads.
            #[cfg(unix)]
            for signum in [1, 2, 15] {
                let state = state.clone();
                glib_unix::unix_signal_add_local(signum, move || {
                    if let Some(a) = state.get() {
                        a.quit(false);
                    }
                    glib::ControlFlow::Continue
                });
            }
        }
    });
    gapp.connect_activate({
        let state = state.clone();
        move |_| {
            if let Some(a) = state.get() {
                a.activate();
            }
        }
    });
    gapp.connect_open(move |_, files, _| {
        if let Some(a) = state.get() {
            a.open(files);
        }
    });
    let code = gapp.run();
    update::relaunch_if_requested();
    code
}
