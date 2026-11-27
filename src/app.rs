use crate::{
    config::{self, Config, Session, Startup, Theme},
    downloads::Downloads,
    engine::{self, DataKinds, Engine, LoadEvent},
    store::Store,
    theme, util,
    window::Window,
};
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

pub const APP_ID: &str = "dev.mallow.Browser";

pub struct App {
    pub passwords: crate::passwords::Manager,
    pub gtk: gtk::Application,
    pub config: RefCell<Config>,
    /// Why the config file could not be used as-is, shown in Settings.
    pub config_problem: RefCell<Option<String>>,
    /// Set by Quit so closing windows skips the "close N tabs?" prompt.
    pub quitting: std::cell::Cell<bool>,
    /// The start-up update check runs once per process.
    update_checked: std::cell::Cell<bool>,
    /// Engine state shared by every view (preferences, tracker filter).
    pub engine: Engine,
    pub downloads: Rc<Downloads>,
    store: OnceCell<Option<Store>>,
    session: OnceCell<engine::Session>,
    windows: RefCell<Vec<Rc<Window>>>,
    tokens: gtk::CssProvider,
}

const ACCELS: &[(&str, &[&str])] = &[
    ("win.focus-url", &["<Ctrl>l", "<Alt>d", "F6"]),
    ("win.new-tab", &["<Ctrl>t"]),
    ("win.close-tab", &["<Ctrl>w", "<Ctrl>F4"]),
    ("win.reopen-tab", &["<Ctrl><Shift>t"]),
    ("win.next-tab", &["<Ctrl>Tab", "<Ctrl>Page_Down"]),
    (
        "win.prev-tab",
        &["<Ctrl><Shift>Tab", "<Ctrl><Shift>ISO_Left_Tab", "<Ctrl>Page_Up"],
    ),
    ("win.reload", &["<Ctrl>r", "F5"]),
    ("win.find", &["<Ctrl>f"]),
    ("win.bookmark", &["<Ctrl>d"]),
    ("win.site-info", &["<Ctrl>i"]),
    ("win.back", &["<Alt>Left"]),
    ("win.forward", &["<Alt>Right"]),
    ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
    ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
    ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]),
    ("win.history", &["<Ctrl>h"]),
    ("win.bookmarks", &["<Ctrl><Shift>o"]),
    ("app.new-window", &["<Ctrl>n"]),
    ("app.new-private-window", &["<Ctrl><Shift>p", "<Ctrl><Shift>n"]),
    ("win.downloads", &["<Ctrl><Shift>y"]),
    ("win.settings", &["<Ctrl>comma"]),
    ("app.quit", &["<Ctrl>q"]),
];

impl App {
    pub fn new(gapp: &gtk::Application) -> Rc<Self> {
        let display = gdk::Display::default().expect("GTK initialised without a display");
        gtk::IconTheme::for_display(&display).add_resource_path("/dev/mallow/icons");
        gtk::Window::set_default_icon_name(APP_ID);
        let base = gtk::CssProvider::new();
        base.load_from_string(include_str!("../assets/style.css"));
        gtk::style_context_add_provider_for_display(
            &display,
            &base,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // Without a compositor (e.g. plain dwm/i3 on X11) transparent pixels
        // render black and GTK 4 cannot shape windows. Drop the shadow and
        // paint the area behind rounded corners in the surface colour, which
        // reads as rounded over plain backgrounds.
        if !display.is_composited() {
            let flat = gtk::CssProvider::new();
            flat.load_from_string(
                "popover { background: var(--surface); } \
                 popover > contents { box-shadow: none; margin: 0; \
                 border: 1px solid var(--outline-variant); } \
                 tooltip { box-shadow: none; border-radius: 0; } \
                 window.dialog.message, window.dialog.alert { border-radius: 0; }",
            );
            gtk::style_context_add_provider_for_display(
                &display,
                &flat,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
        let tokens = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &tokens,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let (config, config_problem) = Config::load();
        if let Some(s) = gtk::Settings::default() {
            // GTK's automatic (unhinted) text rendering clips the tops of
            // some glyphs ("T", "r") with the GL renderer at common sizes.
            s.set_gtk_font_rendering(gtk::FontRendering::Manual);
            s.set_gtk_xft_antialias(1);
            s.set_gtk_xft_hintstyle(Some("hintslight"));
        }

        let app = Rc::new(Self {
            gtk: gapp.clone(),
            config: RefCell::new(config),
            config_problem: RefCell::new(config_problem),
            quitting: Default::default(),
            passwords: Default::default(),
            update_checked: Default::default(),
            engine: Engine::new(),
            downloads: Rc::default(),
            store: OnceCell::new(),
            session: OnceCell::new(),
            windows: RefCell::default(),
            tokens,
        });
        app.apply_theme();
        app.apply_engine();
        if let Some(s) = gtk::Settings::default() {
            // Follow the desktop switching light/dark while running.
            let weak = Rc::downgrade(&app);
            s.connect_gtk_interface_color_scheme_notify(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.load_tokens();
                }
            });
        }
        if app.config.borrow().block_trackers {
            app.load_content_filter();
        }
        app.add_actions();
        app
    }

    fn add_actions(self: &Rc<Self>) {
        let add = |name: &str, f: fn(&Rc<App>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(app) = weak.upgrade() {
                    f(&app);
                }
            });
            self.gtk.add_action(&action);
        };
        add("new-window", |a| Window::new(a, false).open_home());
        add("new-private-window", |a| Window::new(a, true).open_home());
        let weak = Rc::downgrade(self);
        self.gtk.connect_shutdown(move |_| {
            if let Some(app) = weak.upgrade() {
                app.passwords.lock();
                if let Some(Some(store)) = app.store.get() {
                    store.shutdown();
                }
            }
        });
        add("quit", |a| a.quit(true));
        for (action, keys) in ACCELS {
            self.gtk.set_accels_for_action(action, keys);
        }
    }

    pub fn apply_theme(&self) {
        let cfg = self.config.borrow();
        if let Some(s) = gtk::Settings::default() {
            match cfg.theme {
                // Drop our override so the desktop's (portal) preference applies.
                Theme::System => {
                    s.reset_property("gtk-interface-color-scheme");
                }
                Theme::Light | Theme::Dark => {
                    let dark = cfg.theme == Theme::Dark;
                    s.set_gtk_interface_color_scheme(if dark {
                        gtk::InterfaceColorScheme::Dark
                    } else {
                        gtk::InterfaceColorScheme::Light
                    });
                }
            }
            if cfg.animations {
                s.reset_property("gtk-enable-animations");
            } else {
                s.set_gtk_enable_animations(false);
            }
        }
        drop(cfg);
        self.load_tokens();
    }

    /// Regenerate colour tokens for the accent and the effective scheme.
    fn load_tokens(&self) {
        let settings = gtk::Settings::default();
        let dark = settings
            .as_ref()
            .is_some_and(|s| s.gtk_interface_color_scheme() == gtk::InterfaceColorScheme::Dark);
        if let Some(s) = settings {
            // WebKitGTK 2.50 derives pages' prefers-color-scheme from this
            // property only, so it must mirror the new setting.
            #[allow(deprecated)]
            s.set_gtk_application_prefer_dark_theme(dark);
        }
        self.tokens
            .load_from_string(&theme::css(&self.config.borrow().accent, dark));
    }

    /// Change config, persist it, and re-apply what depends on it. Only
    /// called from user actions, never per frame or per keystroke.
    pub fn update(&self, f: impl FnOnce(&mut Config)) {
        f(&mut self.config.borrow_mut());
        self.config.borrow().save();
    }

    /// Engine-wide preferences.
    pub fn apply_engine(&self) {
        self.engine.apply(&self.config.borrow());
    }

    /// Cookie policy and tracking prevention on every live network session.
    pub fn apply_network(&self) {
        let sessions: Vec<engine::Session> = self
            .session
            .get()
            .cloned()
            .into_iter()
            .chain(self.windows.borrow().iter().filter_map(|w| w.live_session()))
            .collect();
        for s in sessions {
            s.apply(&self.config.borrow());
        }
    }

    pub fn set_block_trackers(&self, on: bool) {
        self.update(|c| c.block_trackers = on);
        if on {
            self.load_content_filter();
        } else {
            self.engine.clear_block_rules();
        }
    }

    /// Window-level preferences (compact mode, tab bar) in every window.
    pub fn apply_windows(&self) {
        let windows = self.windows.borrow().clone();
        for w in windows {
            w.apply_prefs();
        }
    }

    /// Bookmarks changed: refresh the bar in every window.
    pub fn sync_bookmarks_bars(&self) {
        let windows = self.windows.borrow().clone();
        for w in windows {
            w.sync_bookmarks_bar();
        }
    }

    pub fn apply_unloading(&self) {
        let windows = self.windows.borrow().clone();
        for w in windows {
            w.apply_unloading();
        }
    }

    pub fn tab_count(&self) -> usize {
        self.windows.borrow().iter().map(|w| w.tab_count()).sum()
    }

    /// Tabs that currently hold a web view (not new-tab, settings or unloaded).
    pub fn loaded_tab_count(&self) -> usize {
        self.windows.borrow().iter().map(|w| w.loaded_count()).sum()
    }

    pub fn set_theme(&self, theme: Theme) {
        self.config.borrow_mut().theme = theme;
        self.apply_theme();
        self.config.borrow().save();
    }

    pub fn set_accent(&self, accent: &str) {
        self.config.borrow_mut().accent = accent.into();
        self.apply_theme();
        self.config.borrow().save();
    }

    /// Compiled asynchronously by WebKit; pages loaded in the first moments
    /// after startup may not be filtered yet.
    fn load_content_filter(&self) {
        let custom = config::config_dir().join("blocklist.txt");
        let list = std::fs::read_to_string(&custom)
            .unwrap_or_else(|_| include_str!("../assets/blocklist.txt").into());
        self.engine
            .set_block_rules(util::block_rules(&list), &config::cache_dir().join("filters"));
    }

    /// History/bookmarks database, opened on first use.
    pub fn store(&self) -> Option<&Store> {
        self.store
            .get_or_init(|| {
                Store::open(&config::data_dir().join("mallow.db"))
                    .map_err(|e| eprintln!("mallow: history and bookmarks unavailable: {e}"))
                    .ok()
            })
            .as_ref()
    }

    /// Persistent session for normal windows, a fresh in-memory one for each
    /// private window.
    pub fn network_session(self: &Rc<Self>, private_downloads: Option<&Rc<Downloads>>) -> engine::Session {
        if let Some(downloads) = private_downloads {
            return self.configure_session(engine::Session::ephemeral(), downloads);
        }
        self.session
            .get_or_init(|| {
                let s = engine::Session::persistent(
                    &config::data_dir().join("web"),
                    &config::cache_dir().join("web"),
                );
                self.configure_session(s, &self.downloads)
            })
            .clone()
    }

    fn configure_session(self: &Rc<Self>, s: engine::Session, downloads: &Rc<Downloads>) -> engine::Session {
        s.apply(&self.config.borrow());
        let weak = Rc::downgrade(self);
        let downloads = Rc::downgrade(downloads);
        s.connect_download(move |dl| {
            let Some(app) = weak.upgrade() else { return };
            let Some(downloads) = downloads.upgrade() else {
                return;
            };
            let view = dl.view();
            let parent = view
                .as_ref()
                .and_then(|v| v.widget().root())
                .and_downcast::<gtk::Window>();
            downloads.track(parent.as_ref(), &dl, app.config.borrow().download_dir());
            // A tab opened only to fetch a file has nothing to show; close it
            // once the transfer ends (closing earlier would cancel it).
            let Some(view) = view else { return };
            if !view.has_history() {
                let navigated = Rc::new(std::cell::Cell::new(false));
                let flag = navigated.clone();
                let handler = std::cell::Cell::new(Some(view.connect_load(move |_, event| {
                    if event == LoadEvent::Started {
                        flag.set(true);
                    }
                })));
                let (app, weak_view) = (Rc::downgrade(&app), view.downgrade());
                dl.connect_end(move |_, _| {
                    if let (Some(app), Some(view), Some(h)) =
                        (app.upgrade(), weak_view.upgrade(), handler.take())
                    {
                        view.disconnect(h);
                        if !navigated.get() && !view.has_history() {
                            app.close_view(&view);
                        }
                    }
                });
            }
        });
        s
    }

    /// Clears history plus cookies, cache and site storage of normal windows.
    pub fn clear_browsing_data(self: &Rc<Self>) {
        self.clear_data(true, DataKinds::ALL);
    }

    /// Clear history (optionally) and the given site data of normal windows.
    pub fn clear_data(self: &Rc<Self>, history: bool, kinds: DataKinds) {
        if history && let Some(store) = self.store() {
            store.clear_history();
        }
        if kinds != DataKinds::NONE {
            self.network_session(None).clear(kinds);
        }
    }

    /// Round site badge: the name's initial, replaced by the site's favicon
    /// once WebKit's favicon database has one. `tone` picks the letter colour.
    pub fn site_avatar(self: &Rc<Self>, url: &str, name: &str, tone: usize) -> gtk::Box {
        let avatar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        // Children expand to centre inside the badge; the badge itself must not.
        avatar.set_hexpand(false);
        avatar.add_css_class("tile-avatar");
        avatar.add_css_class(&format!("tone-{}", tone % 3));
        let letter = gtk::Label::builder()
            .label(util::initial(name).to_string())
            .hexpand(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        avatar.append(&letter);
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return avatar;
        }
        let session = self.network_session(None);
        let (weak, url) = (avatar.downgrade(), url.to_string());
        glib::spawn_future_local(async move {
            // No icon recorded yet is the common case; keep the letter.
            let Some(texture) = session.favicon(&url).await else {
                return;
            };
            let Some(avatar) = weak.upgrade() else { return };
            let icon = gtk::Image::builder()
                .paintable(&texture)
                .hexpand(true)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            avatar.remove(&letter);
            avatar.append(&icon);
            for tone in 0..3 {
                avatar.remove_css_class(&format!("tone-{tone}"));
            }
            avatar.add_css_class("has-favicon");
        });
        avatar
    }

    fn close_view(&self, view: &engine::View) {
        let windows = self.windows.borrow().clone();
        for w in windows {
            if w.close_view(view) {
                return;
            }
        }
    }

    pub fn register(&self, w: Rc<Window>) {
        self.windows.borrow_mut().push(w);
    }

    pub fn unregister(&self, w: &Window) {
        self.windows.borrow_mut().retain(|x| !std::ptr::eq(x.as_ref(), w));
    }

    pub fn window_count(&self) -> usize {
        self.windows.borrow().len()
    }

    /// Downloads in progress across the shared list and every private window's.
    pub fn running_downloads(&self) -> usize {
        let windows = self.windows.borrow();
        let private: usize = windows
            .iter()
            .filter(|w| w.private)
            .map(|w| w.downloads.running())
            .sum();
        self.downloads.running() + private
    }

    /// Close every window. With `ask`, first confirm cancelling running downloads.
    pub fn quit(self: &Rc<Self>, ask: bool) {
        let running = self.running_downloads();
        if ask && running > 0 {
            let dialog = gtk::AlertDialog::builder()
                .modal(true)
                .message(if running == 1 {
                    "Cancel 1 download?".to_string()
                } else {
                    format!("Cancel {running} downloads?")
                })
                .detail("Quitting stops downloads that haven't finished.")
                .buttons(["Keep downloading", "Quit"])
                .cancel_button(0)
                .default_button(0)
                .build();
            let app = Rc::downgrade(self);
            let parent = self.gtk.active_window();
            dialog.choose(parent.as_ref(), None::<&gio::Cancellable>, move |r| {
                match (r, app.upgrade()) {
                    (Ok(1), Some(app)) => app.quit(false),
                    _ => crate::update::cancel_restart(),
                }
            });
            return;
        }
        self.quitting.set(true);
        let windows = self.windows.borrow().clone();
        for w in windows {
            w.win.close();
        }
    }

    pub fn normal_window_count(&self) -> usize {
        self.windows.borrow().iter().filter(|w| !w.private).count()
    }

    /// Start-up check: tell the user, in the active window, about a newer
    /// release. Failures stay silent; the About page reports them on demand.
    fn startup_update_check(self: &Rc<Self>) {
        if self.update_checked.replace(true) || !self.config.borrow().check_updates {
            return;
        }
        let weak = Rc::downgrade(self);
        crate::update::check(move |outcome| {
            let Some(app) = weak.upgrade() else { return };
            if let crate::update::Outcome::Newer { version, url } = outcome {
                let active = app.gtk.active_window();
                let windows = app.windows.borrow().clone();
                let target = windows
                    .iter()
                    .find(|w| Some(w.win.upcast_ref::<gtk::Window>()) == active.as_ref())
                    .or(windows.first())
                    .cloned();
                if let Some(w) = target {
                    w.show_update_toast(&version, &url);
                }
            }
        });
    }

    pub fn activate(self: &Rc<Self>) {
        self.startup_update_check();
        let first = self.windows.borrow().is_empty();
        let w = Window::new(self, false);
        let startup = if first {
            self.config.borrow().startup()
        } else {
            Startup::Homepage
        };
        match startup {
            Startup::Restore => match Session::load().filter(|s| !s.tabs.is_empty()) {
                Some(s) => w.restore(s),
                None => w.open_home(),
            },
            Startup::Homepage => w.open_home(),
            Startup::NewTab => {
                w.new_tab(None, true);
            }
        }
    }

    pub fn open(self: &Rc<Self>, files: &[gio::File]) {
        self.startup_update_check();
        let active = self.gtk.active_window();
        let existing = self
            .windows
            .borrow()
            .iter()
            .find(|w| Some(w.win.upcast_ref()) == active.as_ref())
            .cloned();
        let w = existing.unwrap_or_else(|| Window::new(self, false));
        for f in files {
            w.new_tab(Some(&f.uri()), true);
        }
        w.win.present();
    }
}
