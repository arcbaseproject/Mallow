use crate::{
    config::{self, Config, Cookies, Session, Theme},
    downloads::Downloads,
    store::Store,
    theme, util,
    window::Window,
};
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};
use webkit6::{NetworkSession, prelude::*};

pub const APP_ID: &str = "dev.mallow.Browser";

pub struct App {
    pub gtk: gtk::Application,
    pub config: RefCell<Config>,
    /// Shared by every view so the content filter is compiled once.
    pub content: webkit6::UserContentManager,
    pub settings: webkit6::Settings,
    pub downloads: Rc<Downloads>,
    store: OnceCell<Option<Store>>,
    session: OnceCell<NetworkSession>,
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
                 tooltip { box-shadow: none; border-radius: 0; }",
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

        let settings = webkit6::Settings::new();
        settings.set_enable_developer_extras(true);

        let app = Rc::new(Self {
            gtk: gapp.clone(),
            config: RefCell::new(Config::load()),
            content: webkit6::UserContentManager::new(),
            settings,
            downloads: Rc::default(),
            store: OnceCell::new(),
            session: OnceCell::new(),
            windows: RefCell::default(),
            tokens,
        });
        app.apply_theme();
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
            if let Some(app) = weak.upgrade()
                && let Some(Some(store)) = app.store.get()
            {
                store.shutdown();
            }
        });
        add("quit", |a| {
            let windows = a.windows.borrow().clone();
            for w in windows {
                w.win.close();
            }
        });
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
            if !cfg.animations {
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
        let rules = util::block_rules(&list);
        let dir = config::cache_dir().join("filters");
        let store = webkit6::UserContentFilterStore::new(&dir.to_string_lossy());
        let content = self.content.clone();
        store.save(
            "mallow-trackers",
            &glib::Bytes::from_owned(rules),
            None::<&gio::Cancellable>,
            move |r| match r {
                Ok(filter) => content.add_filter(&filter),
                Err(e) => eprintln!("mallow: content filter failed to compile: {e}"),
            },
        );
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
    pub fn network_session(self: &Rc<Self>, private_downloads: Option<&Rc<Downloads>>) -> NetworkSession {
        if let Some(downloads) = private_downloads {
            return self.configure_session(NetworkSession::new_ephemeral(), downloads);
        }
        self.session
            .get_or_init(|| {
                let data = config::data_dir().join("web");
                let cache = config::cache_dir().join("web");
                let s = NetworkSession::new(Some(&data.to_string_lossy()), Some(&cache.to_string_lossy()));
                if let Some(cm) = s.cookie_manager() {
                    cm.set_persistent_storage(
                        &data.join("cookies.sqlite").to_string_lossy(),
                        webkit6::CookiePersistentStorage::Sqlite,
                    );
                }
                self.configure_session(s, &self.downloads)
            })
            .clone()
    }

    fn configure_session(self: &Rc<Self>, s: NetworkSession, downloads: &Rc<Downloads>) -> NetworkSession {
        if let Some(dm) = s.website_data_manager() {
            dm.set_favicons_enabled(true);
        }
        s.set_itp_enabled(true);
        if let Some(cm) = s.cookie_manager() {
            cm.set_accept_policy(match self.config.borrow().cookies {
                Cookies::NoThirdParty => webkit6::CookieAcceptPolicy::NoThirdParty,
                Cookies::Always => webkit6::CookieAcceptPolicy::Always,
                Cookies::Never => webkit6::CookieAcceptPolicy::Never,
            });
        }
        let weak = Rc::downgrade(self);
        let downloads = Rc::downgrade(downloads);
        s.connect_download_started(move |_, dl| {
            let Some(app) = weak.upgrade() else { return };
            let Some(downloads) = downloads.upgrade() else {
                return;
            };
            let parent = dl
                .web_view()
                .and_then(|view| view.root())
                .and_downcast::<gtk::Window>();
            downloads.track(parent.as_ref(), dl);
            // A tab opened only to fetch a file has nothing to show; close it
            // once the transfer ends (closing earlier would cancel it).
            let Some(view) = dl.web_view() else { return };
            if view.back_forward_list().and_then(|l| l.current_item()).is_none() {
                let close = {
                    let navigated = Rc::new(std::cell::Cell::new(false));
                    let flag = navigated.clone();
                    let handler = view.connect_load_changed(move |_, event| {
                        if event == webkit6::LoadEvent::Started {
                            flag.set(true);
                        }
                    });
                    let (app, view) = (Rc::downgrade(&app), view.downgrade());
                    move || {
                        if let (Some(app), Some(view)) = (app.upgrade(), view.upgrade()) {
                            view.disconnect(handler);
                            if !navigated.get()
                                && view.back_forward_list().and_then(|l| l.current_item()).is_none()
                            {
                                app.close_view(&view);
                            }
                        }
                    }
                };
                let close = std::cell::Cell::new(Some(close));
                dl.connect_finished(move |_| {
                    if let Some(close) = close.take() {
                        close();
                    }
                });
            }
        });
        s
    }

    /// Clears history plus cookies, cache and site storage of normal windows.
    pub fn clear_browsing_data(self: &Rc<Self>) {
        if let Some(store) = self.store() {
            store.clear_history();
        }
        if let Some(dm) = self.network_session(None).website_data_manager() {
            dm.clear(
                webkit6::WebsiteDataTypes::all(),
                glib::TimeSpan::from_seconds(0),
                None::<&gio::Cancellable>,
                |r| {
                    if let Err(e) = r {
                        eprintln!("mallow: clearing site data failed: {e}");
                    }
                },
            );
            if let Some(fav) = dm.favicon_database() {
                fav.clear();
            }
        }
    }

    fn close_view(&self, view: &webkit6::WebView) {
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

    pub fn normal_window_count(&self) -> usize {
        self.windows.borrow().iter().filter(|w| !w.private).count()
    }

    pub fn activate(self: &Rc<Self>) {
        let first = self.windows.borrow().is_empty();
        let w = Window::new(self, false);
        if first
            && self.config.borrow().restore_session
            && let Some(s) = Session::load().filter(|s| !s.tabs.is_empty())
        {
            w.restore(s);
            return;
        }
        w.open_home();
    }

    pub fn open(self: &Rc<Self>, files: &[gio::File]) {
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
