use crate::{
    app::App,
    config::{Session, SessionTab, Theme},
    engine::{self, Permission, PermissionRequest, View},
    lists, theme, util, view,
};
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

const ZOOM_STEP: f64 = 0.1;

/// Built-in pages that live in a tab instead of a WebView.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Internal {
    Settings,
    History,
}

impl Internal {
    pub fn uri(self) -> &'static str {
        match self {
            Self::Settings => crate::config::SETTINGS_URI,
            Self::History => crate::config::HISTORY_URI,
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Settings => "Settings",
            Self::History => "History",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Settings => "m-settings-symbolic",
            Self::History => "m-history-symbolic",
        }
    }
    fn from_uri(uri: &str) -> Option<Self> {
        let u = uri.trim_end_matches('/');
        [Self::Settings, Self::History]
            .into_iter()
            .find(|p| u.eq_ignore_ascii_case(p.uri()))
    }
}

pub struct Tab {
    pub widget: gtk::Revealer,
    root: gtk::Box,
    label: gtk::Label,
    icon: gtk::Image,
    /// Stack child: the new-tab page or the WebView, created on first load.
    pub page: gtk::Box,
    pub view: RefCell<Option<View>>,
    /// URL to load when the tab is first shown (restored sessions).
    pub pending: RefCell<Option<String>>,
    /// Opened on the new-tab page, so Back from the first page returns to it.
    from_ntp: Cell<bool>,
    /// View set aside by Back to the new-tab page; Forward brings it back.
    parked: RefCell<Option<View>>,
    /// Showing the native settings page (`mallow://settings`).
    internal: Cell<Option<Internal>>,
    /// One-shot "unload after inactivity" timer while in the background.
    unload_timer: RefCell<Option<glib::SourceId>>,
    /// History of an unloaded tab, restored when it is shown again.
    suspended: RefCell<Option<engine::SessionState>>,
    /// Shares a web process with a popup it opened or was opened by, so
    /// unloading must not terminate that process.
    shares_process: Cell<bool>,
    /// Address-bar text typed but not submitted, kept while another tab is shown.
    draft: RefCell<Option<String>>,
}

impl Tab {
    pub fn set_title(&self, title: &str) {
        let title = if title.is_empty() { "New tab" } else { title };
        self.label.set_text(title);
        self.root.set_tooltip_text(Some(title));
    }

    pub fn set_icon(&self, texture: Option<&gdk::Texture>) {
        match texture {
            Some(t) => self.icon.set_paintable(Some(t)),
            None => self.icon.set_icon_name(Some("m-public-symbolic")),
        }
    }

    pub fn uri(&self) -> Option<String> {
        if let Some(page) = self.internal.get() {
            return Some(page.uri().into());
        }
        let live = self.view.borrow().as_ref().and_then(|v| v.uri());
        live.or_else(|| self.pending.borrow().clone())
            .filter(|u| !u.is_empty())
    }

    fn title(&self) -> String {
        self.label.text().into()
    }
}

pub struct Window {
    pub app: Rc<App>,
    pub win: gtk::ApplicationWindow,
    pub private: bool,
    pub downloads: Rc<crate::downloads::Downloads>,
    /// Created with the first WebView, so an idle new-tab page starts no
    /// WebKit network process.
    session: std::cell::OnceCell<engine::Session>,
    tabs: RefCell<Vec<Rc<Tab>>>,
    current: RefCell<Option<Rc<Tab>>>,
    strip: gtk::Box,
    tabbar: gtk::Box,
    pub overlay: gtk::Overlay,
    viewport: gtk::Viewport,
    pub stack: gtk::Stack,
    url: gtk::Entry,
    reload: gtk::Button,
    progress: gtk::ProgressBar,
    pub status: gtk::Label,
    find_bar: gtk::Revealer,
    find_entry: gtk::SearchEntry,
    pub find_label: gtk::Label,
    closed: RefCell<Vec<String>>,
    /// The user already agreed to close this window with several tabs.
    close_confirmed: Cell<bool>,
    /// Storage access decisions scoped to the top-level origin and WebKit's domain pair.
    permissions: RefCell<HashMap<(String, String, String), bool>>,
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("icon-btn");
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

impl Window {
    pub fn new(app: &Rc<App>, private: bool) -> Rc<Self> {
        let win = gtk::ApplicationWindow::builder()
            .application(&app.gtk)
            .default_width(1200)
            .default_height(800)
            .title(if private { "Mallow — Private" } else { "Mallow" })
            .build();
        win.add_css_class("mallow");
        if private {
            win.add_css_class("private");
        }

        // Tab strip
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        strip.add_css_class("tabs");
        let viewport = gtk::Viewport::builder()
            .child(&strip)
            .scroll_to_focus(true)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .child(&viewport)
            .build();
        let new_tab = icon_button("m-add-symbolic", "New tab");
        new_tab.set_action_name(Some("win.new-tab"));
        let tabbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tabbar.add_css_class("tabbar");
        tabbar.append(&scroller);
        tabbar.append(&new_tab);
        if private {
            let badge = gtk::Label::new(Some("Private"));
            badge.add_css_class("private-badge");
            badge.set_hexpand(true);
            badge.set_halign(gtk::Align::End);
            badge.set_valign(gtk::Align::Center);
            tabbar.append(&badge);
        }

        // Toolbar
        let back = icon_button("m-arrow_back-symbolic", "Back (Alt+Left)");
        back.set_action_name(Some("win.back"));
        let forward = icon_button("m-arrow_forward-symbolic", "Forward (Alt+Right)");
        forward.set_action_name(Some("win.forward"));
        let reload = icon_button("m-refresh-symbolic", "Reload (Ctrl+R)");
        let url = gtk::Entry::builder()
            .hexpand(true)
            .placeholder_text("Search or enter address")
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        url.add_css_class("omnibox");
        let menu = gtk::MenuButton::builder()
            .icon_name("m-more_vert-symbolic")
            .tooltip_text("Menu")
            .build();
        menu.add_css_class("icon-btn");
        menu.set_valign(gtk::Align::Center);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        toolbar.add_css_class("toolbar");
        for w in [
            back.upcast_ref::<gtk::Widget>(),
            forward.upcast_ref(),
            reload.upcast_ref(),
            url.upcast_ref(),
            menu.upcast_ref(),
        ] {
            toolbar.append(w);
        }

        // Find bar
        let find_entry = gtk::SearchEntry::builder()
            .placeholder_text("Find in page")
            .width_chars(28)
            .build();
        find_entry.add_css_class("omnibox");
        let find_label = gtk::Label::new(None);
        find_label.add_css_class("dim-label");
        let find_prev = icon_button("m-keyboard_arrow_up-symbolic", "Previous match (Shift+Enter)");
        let find_next = icon_button("m-keyboard_arrow_down-symbolic", "Next match (Enter)");
        let find_close = icon_button("m-close-symbolic", "Close (Escape)");
        let find_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        find_box.add_css_class("findbar");
        find_label.set_hexpand(true);
        find_label.set_xalign(0.0);
        for w in [
            find_entry.upcast_ref::<gtk::Widget>(),
            find_prev.upcast_ref(),
            find_next.upcast_ref(),
            find_label.upcast_ref(),
            find_close.upcast_ref(),
        ] {
            find_box.append(w);
        }
        let find_bar = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(150)
            .child(&find_box)
            .build();

        // Content
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .vexpand(true)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .build();
        let progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Start)
            .visible(false)
            .can_target(false)
            .build();
        progress.add_css_class("loadbar");
        let status = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(80)
            .visible(false)
            .can_target(false)
            .build();
        status.add_css_class("status");
        let overlay = gtk::Overlay::builder().child(&stack).vexpand(true).build();
        overlay.add_overlay(&progress);
        overlay.add_overlay(&status);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&tabbar);
        root.append(&toolbar);
        root.append(&find_bar);
        root.append(&overlay);
        win.set_child(Some(&root));

        let w = Rc::new(Self {
            session: Default::default(),
            app: app.clone(),
            win,
            private,
            downloads: if private {
                Rc::default()
            } else {
                app.downloads.clone()
            },
            tabs: RefCell::default(),
            current: RefCell::default(),
            strip,
            tabbar,
            overlay,
            viewport,
            stack,
            url,
            reload,
            progress,
            status,
            find_bar,
            find_entry,
            find_label,
            closed: RefCell::default(),
            close_confirmed: Cell::new(false),
            permissions: RefCell::default(),
        });
        w.apply_prefs();
        menu.set_popover(Some(&w.build_menu()));
        w.connect_signals(&find_prev, &find_next, &find_close);
        w.add_actions();
        app.register(w.clone());
        w.win.present();
        w
    }

    fn connect_signals(
        self: &Rc<Self>,
        find_prev: &gtk::Button,
        find_next: &gtk::Button,
        find_close: &gtk::Button,
    ) {
        let weak = Rc::downgrade(self);
        let with = move |f: fn(&Rc<Window>)| {
            let weak = weak.clone();
            move || {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            }
        };
        let cb = with(|w| w.navigate_from_omnibox());
        self.url.connect_activate(move |_| cb());
        crate::suggest::attach(&self.url, &self.app);
        let cb = with(|w| {
            let loading = w.current_view().is_some_and(|v| v.is_loading());
            w.with_view(|v| if loading { v.stop() } else { v.reload() });
        });
        self.reload.connect_clicked(move |_| cb());
        let (star, lock) = (with(|w| w.edit_bookmark()), with(|w| w.show_site_info()));
        self.url.connect_icon_press(move |_, pos| match pos {
            gtk::EntryIconPosition::Secondary => star(),
            _ => lock(),
        });
        // Escape in the omnibox restores the page URL and returns to the page.
        let keys = gtk::EventControllerKey::new();
        let cb = with(|w| {
            w.url.set_text(
                &w.current
                    .borrow()
                    .as_ref()
                    .and_then(|t| t.uri())
                    .unwrap_or_default(),
            );
            w.with_view(|v| {
                v.grab_focus();
            });
        });
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                cb();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.url.add_controller(keys);

        let cb = with(|w| w.find(false));
        self.find_entry.connect_search_changed(move |_| cb());
        for (signal_src, backwards) in [(find_next, false), (find_prev, true)] {
            let cb = with(if backwards {
                |w: &Rc<Window>| w.find_step(true)
            } else {
                |w: &Rc<Window>| w.find_step(false)
            });
            signal_src.connect_clicked(move |_| cb());
        }
        let cb = with(|w| w.find_step(false));
        self.find_entry.connect_activate(move |_| cb());
        let cb = with(|w| w.find_step(false));
        self.find_entry.connect_next_match(move |_| cb());
        let cb = with(|w| w.find_step(true));
        self.find_entry.connect_previous_match(move |_| cb());
        let shift_enter = gtk::EventControllerKey::new();
        let cb = with(|w| w.find_step(true));
        shift_enter.connect_key_pressed(move |_, key, _, mods| {
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                && mods.contains(gdk::ModifierType::SHIFT_MASK)
            {
                cb();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.find_entry.add_controller(shift_enter);
        let cb = with(|w| w.close_find());
        self.find_entry.connect_stop_search(move |_| cb());
        let cb = with(|w| w.close_find());
        find_close.connect_clicked(move |_| cb());

        let weak = Rc::downgrade(self);
        self.win.connect_close_request(move |_| {
            if let Some(w) = weak.upgrade() {
                // Closing the last window quits; let quit ask about running downloads.
                if !w.app.quitting.get() && w.app.window_count() == 1 && w.app.running_downloads() > 0 {
                    w.app.quit(true);
                    return glib::Propagation::Stop;
                }
                let tabs = w.tab_count();
                if tabs > 1
                    && w.app.config.borrow().confirm_close
                    && !w.app.quitting.get()
                    && !w.close_confirmed.get()
                {
                    w.confirm_close(tabs);
                    return glib::Propagation::Stop;
                }
                if w.private {
                    w.downloads.close();
                } else if w.app.normal_window_count() == 1 {
                    // Saved whatever the startup mode, so switching to
                    // "Restore" later still finds the last session.
                    w.save_session();
                }
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        self.win.connect_destroy(move |_| {
            if let Some(w) = weak.upgrade() {
                w.tabs.borrow_mut().clear();
                w.current.replace(None);
                w.app.unregister(&w);
            }
        });
    }

    fn add_actions(self: &Rc<Self>) {
        let actions: &[(&str, fn(&Rc<Window>))] = &[
            ("focus-url", |w| {
                w.url.grab_focus();
            }),
            ("new-tab", |w| {
                let home = {
                    let cfg = w.app.config.borrow();
                    (cfg.new_tab == crate::config::NewTab::Homepage && !cfg.homepage.is_empty())
                        .then(|| cfg.homepage.clone())
                };
                w.new_tab(home.as_deref(), true);
            }),
            ("settings", |w| w.open_internal(Internal::Settings)),
            ("close-tab", |w| {
                let current = w.current.borrow().clone();
                if let Some(t) = current {
                    w.close_tab(&t);
                }
            }),
            ("reopen-tab", |w| {
                let last = w.closed.borrow_mut().pop();
                if let Some(u) = last {
                    w.new_tab(Some(&u), true);
                }
            }),
            ("next-tab", |w| w.cycle(1)),
            ("prev-tab", |w| w.cycle(-1)),
            ("reload", |w| w.with_view(|v| v.reload())),
            ("back", |w| w.go_back()),
            ("forward", |w| w.go_forward()),
            ("find", |w| w.open_find()),
            ("bookmark", |w| w.edit_bookmark()),
            ("site-info", |w| w.show_site_info()),
            ("zoom-in", |w| {
                w.with_view(|v| v.set_zoom((v.zoom() + ZOOM_STEP).min(3.0)))
            }),
            ("zoom-out", |w| {
                w.with_view(|v| v.set_zoom((v.zoom() - ZOOM_STEP).max(0.3)))
            }),
            ("zoom-reset", |w| {
                let zoom = w.app.config.borrow().default_zoom;
                w.with_view(|v| v.set_zoom(zoom))
            }),
            ("downloads", |w| w.downloads.show(Some(w.win.upcast_ref()))),
            ("history", |w| w.open_internal(Internal::History)),
            ("bookmarks", |w| w.show_bookmarks()),
            ("clear-data", |w| w.confirm_clear_data()),
        ];
        for &(name, f) in actions {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            });
            self.win.add_action(&action);
        }

        // Mouse side buttons (8 = back, 9 = forward); capture phase so the WebView can't swallow them.
        let side = gtk::GestureClick::builder()
            .button(0)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        side.connect_pressed(|g, _, _, _| {
            let action = match g.current_button() {
                8 => "win.back",
                9 => "win.forward",
                _ => return,
            };
            if let Some(w) = g.widget() {
                let _ = w.activate_action(action, None);
            }
            g.set_state(gtk::EventSequenceState::Claimed);
        });
        self.win.add_controller(side);
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        if let Some(a) = self.win.lookup_action(name).and_downcast::<gio::SimpleAction>() {
            a.set_enabled(enabled);
        }
    }

    fn build_menu(self: &Rc<Self>) -> gtk::PopoverMenu {
        // GtkPopoverMenu hides an item's icon when it has a label, so rows are custom children.
        type Row = (&'static str, &'static str, &'static str);
        let sections: [&[Row]; 4] = [
            &[
                ("New tab", "win.new-tab", "m-add-symbolic"),
                ("New window", "app.new-window", "m-web_asset-symbolic"),
                (
                    "New private window",
                    "app.new-private-window",
                    "m-visibility_off-symbolic",
                ),
            ],
            &[],
            &[
                ("Bookmarks", "win.bookmarks", "m-bookmarks-symbolic"),
                ("History", "win.history", "m-history-symbolic"),
                ("Downloads", "win.downloads", "m-download-symbolic"),
                ("Find in page…", "win.find", "m-search-symbolic"),
            ],
            &[
                ("Settings", "win.settings", "m-settings-symbolic"),
                ("Clear browsing data…", "win.clear-data", "m-cookie-symbolic"),
                ("Quit", "app.quit", "m-logout-symbolic"),
            ],
        ];
        let custom = |section: &gio::Menu, id: &str| {
            let item = gio::MenuItem::new(None, None);
            item.set_attribute_value("custom", Some(&id.to_variant()));
            section.append_item(&item);
        };
        let menu = gio::Menu::new();
        for (i, rows) in sections.iter().enumerate() {
            let section = gio::Menu::new();
            match i {
                1 => custom(&section, "zoom"),
                3 => {
                    let appearance = gio::Menu::new();
                    custom(&appearance, "appearance");
                    menu.append_section(None, &appearance);
                }
                _ => {}
            }
            for (_, action, _) in rows.iter() {
                custom(&section, action);
            }
            menu.append_section(None, &section);
        }
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_has_arrow(false);
        for (label, action, icon) in sections.iter().flat_map(|r| r.iter()) {
            pop.add_child(&self.menu_row(label, action, icon), action);
        }
        pop.add_child(&self.zoom_row(&pop), "zoom");
        pop.add_child(&self.appearance_row(), "appearance");
        pop
    }

    fn menu_row(&self, label: &str, action: &str, icon: &str) -> gtk::Widget {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&gtk::Image::from_icon_name(icon));
        row.append(
            &gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        let accel = self.app.gtk.accels_for_action(action);
        if let Some((key, mods)) = accel.first().and_then(gtk::accelerator_parse) {
            let l = gtk::Label::new(Some(&gtk::accelerator_get_label(key, mods)));
            l.add_css_class("accelerator");
            row.append(&l);
        }
        let b = gtk::Button::builder().child(&row).action_name(action).build();
        b.add_css_class("menu-row");
        b.connect_clicked(|b| {
            if let Some(p) = b
                .ancestor(gtk::Popover::static_type())
                .and_downcast::<gtk::Popover>()
            {
                p.popdown();
            }
        });
        b.upcast()
    }

    fn zoom_row(self: &Rc<Self>, pop: &gtk::PopoverMenu) -> gtk::Widget {
        let pct = gtk::Button::builder()
            .label("100%")
            .action_name("win.zoom-reset")
            .tooltip_text("Reset zoom (Ctrl+0)")
            .build();
        let weak = Rc::downgrade(self);
        let p = pct.clone();
        let refresh = move || {
            let z = weak
                .upgrade()
                .and_then(|w| w.current_view())
                .map_or(1.0, |v| v.zoom());
            p.set_label(&format!("{:.0}%", z * 100.0));
        };
        let r = refresh.clone();
        pop.connect_show(move |_| r());
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.add_css_class("zoom-row");
        row.append(&gtk::Image::from_icon_name("m-search-symbolic"));
        row.append(
            &gtk::Label::builder()
                .label("Zoom")
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        let out = gtk::Button::from_icon_name("m-remove-symbolic");
        out.set_action_name(Some("win.zoom-out"));
        out.set_tooltip_text(Some("Zoom out (Ctrl+−)"));
        let zin = gtk::Button::from_icon_name("m-add-symbolic");
        zin.set_action_name(Some("win.zoom-in"));
        zin.set_tooltip_text(Some("Zoom in (Ctrl++)"));
        for b in [out, pct, zin] {
            let r = refresh.clone();
            b.connect_clicked(move |_| r());
            row.append(&b);
        }
        row.upcast()
    }

    fn appearance_row(self: &Rc<Self>) -> gtk::Widget {
        let current = self.app.config.borrow().theme;
        let themes = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        themes.add_css_class("segmented");
        themes.set_homogeneous(true);
        let mut group: Option<gtk::ToggleButton> = None;
        for (theme, icon, tip) in [
            (Theme::System, "m-brightness_auto-symbolic", "Follow system"),
            (Theme::Light, "m-light_mode-symbolic", "Light"),
            (Theme::Dark, "m-dark_mode-symbolic", "Dark"),
        ] {
            let b = gtk::ToggleButton::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .active(theme == current)
                .build();
            b.set_group(group.as_ref());
            group.get_or_insert_with(|| b.clone());
            let app = Rc::downgrade(&self.app);
            b.connect_toggled(move |b| {
                if let (true, Some(app)) = (b.is_active(), app.upgrade()) {
                    app.set_theme(theme);
                }
            });
            themes.append(&b);
        }
        let swatches = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        swatches.set_halign(gtk::Align::Center);
        for (i, hex) in theme::PRESETS.iter().enumerate() {
            let b = gtk::Button::new();
            b.add_css_class("swatch");
            b.add_css_class(&format!("swatch-{i}"));
            b.set_tooltip_text(Some(hex));
            let app = Rc::downgrade(&self.app);
            b.connect_clicked(move |_| {
                if let Some(app) = app.upgrade() {
                    app.set_accent(hex);
                }
            });
            swatches.append(&b);
        }
        let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
        col.add_css_class("appearance");
        col.append(&themes);
        col.append(&swatches);
        col.upcast()
    }

    // ----- tabs -----

    fn add_tab(self: &Rc<Self>) -> Rc<Tab> {
        let icon = gtk::Image::from_icon_name("m-public-symbolic");
        icon.add_css_class("favicon");
        let label = gtk::Label::builder()
            .label("New tab")
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(12)
            .max_width_chars(16)
            .xalign(0.0)
            .build();
        let inner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        inner.append(&icon);
        inner.append(&label);
        let main = gtk::Button::builder().child(&inner).build();
        main.add_css_class("tab-main");
        let close = gtk::Button::from_icon_name("m-close-symbolic");
        close.add_css_class("tab-close");
        close.set_tooltip_text(Some("Close tab (Ctrl+W)"));
        close.set_valign(gtk::Align::Center);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("tab");
        root.append(&main);
        root.append(&close);
        let widget = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideRight)
            .transition_duration(150)
            .child(&root)
            .build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.add_css_class("page");

        let tab = Rc::new(Tab {
            widget,
            root,
            label,
            icon,
            page,
            view: RefCell::default(),
            pending: RefCell::default(),
            from_ntp: Cell::default(),
            parked: RefCell::default(),
            internal: Cell::new(None),
            unload_timer: RefCell::default(),
            suspended: RefCell::default(),
            shares_process: Cell::new(false),
            draft: RefCell::new(None),
        });

        let (w, t) = (Rc::downgrade(self), Rc::downgrade(&tab));
        let both = move || Some((w.upgrade()?, t.upgrade()?));
        let b = both.clone();
        main.connect_clicked(move |_| {
            if let Some((w, t)) = b() {
                w.select(&t);
            }
        });
        let b = both.clone();
        close.connect_clicked(move |_| {
            if let Some((w, t)) = b() {
                w.close_tab(&t);
            }
        });
        let click = gtk::GestureClick::builder().button(0).build();
        let b = both.clone();
        click.connect_pressed(move |g, _, x, y| {
            let Some((w, t)) = b() else { return };
            match g.current_button() {
                gdk::BUTTON_MIDDLE => w.close_tab(&t),
                gdk::BUTTON_SECONDARY => w.tab_menu(&t, x, y),
                _ => {}
            }
        });
        tab.root.add_controller(click);

        let group = gio::SimpleActionGroup::new();
        let tab_actions: &[(&str, fn(&Rc<Window>, &Rc<Tab>))] = &[
            ("reload", |_, t| {
                if let Some(v) = t.view.borrow().as_ref() {
                    v.reload();
                }
            }),
            ("duplicate", |w, t| w.duplicate(t)),
            ("copy-url", |w, t| {
                if let Some(u) = t.uri() {
                    w.win.clipboard().set_text(&u);
                }
            }),
            ("close", |w, t| w.close_tab(t)),
        ];
        for &(name, f) in tab_actions {
            let action = gio::SimpleAction::new(name, None);
            let b = both.clone();
            action.connect_activate(move |_, _| {
                if let Some((w, t)) = b() {
                    f(&w, &t);
                }
            });
            group.add_action(&action);
        }
        tab.root.insert_action_group("tab", Some(&group));

        self.tabs.borrow_mut().push(tab.clone());
        self.strip.append(&tab.widget);
        self.stack.add_child(&tab.page);
        tab.widget.set_reveal_child(true);
        self.update_tabbar();
        tab
    }

    fn tab_menu(&self, tab: &Tab, x: f64, y: f64) {
        let menu = gio::Menu::new();
        for (label, action) in [
            ("Reload", "tab.reload"),
            ("Duplicate", "tab.duplicate"),
            ("Copy URL", "tab.copy-url"),
            ("Close", "tab.close"),
        ] {
            menu.append(Some(label), Some(action));
        }
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_has_arrow(false);
        pop.set_parent(&tab.root);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    /// Opens a tab with `uri`, or the new-tab page.
    pub fn new_tab(self: &Rc<Self>, uri: Option<&str>, select: bool) -> Rc<Tab> {
        let tab = self.add_tab();
        match uri {
            Some(u) => self.load(&tab, u),
            None => {
                tab.from_ntp.set(true);
                tab.page.append(&self.new_tab_page(&tab));
            }
        }
        if select {
            self.select(&tab);
        }
        tab
    }

    /// For `window.open()` and "open in new window": the view must be related
    /// to its opener and is returned to WebKit unloaded.
    pub fn new_related_tab(self: &Rc<Self>, related: &View) -> View {
        let tab = self.add_tab();
        tab.shares_process.set(true);
        let opener = self
            .tabs
            .borrow()
            .iter()
            .find(|t| t.view.borrow().as_ref() == Some(related))
            .cloned();
        if let Some(o) = opener {
            o.shares_process.set(true);
        }
        let v = self.ensure_view(&tab, Some(related));
        self.select(&tab);
        v
    }

    pub fn open_home(self: &Rc<Self>) {
        let home = self.app.config.borrow().homepage.clone();
        self.new_tab(Some(&home).filter(|h| !h.is_empty()).map(String::as_str), true);
    }

    pub fn restore(self: &Rc<Self>, session: Session) {
        let tabs: Vec<_> = session
            .tabs
            .iter()
            .map(|s| {
                let t = self.add_tab();
                t.set_title(if s.title.is_empty() { &s.url } else { &s.title });
                *t.pending.borrow_mut() = Some(s.url.clone());
                t
            })
            .collect();
        if let Some(t) = tabs.get(session.active).or(tabs.last()) {
            self.select(t);
        }
    }

    fn save_session(&self) {
        let current = self.current.borrow().clone();
        let mut s = Session::default();
        for t in self.tabs.borrow().iter() {
            let Some(url) = t.uri() else { continue };
            if current.as_ref().is_some_and(|c| Rc::ptr_eq(c, t)) {
                s.active = s.tabs.len();
            }
            s.tabs.push(SessionTab {
                url,
                title: t.title(),
            });
        }
        s.save();
    }

    fn duplicate(self: &Rc<Self>, tab: &Rc<Tab>) {
        let state = tab.view.borrow().as_ref().and_then(|v| v.session_state());
        match (state, tab.uri()) {
            (Some(state), _) => {
                let t = self.add_tab();
                self.ensure_view(&t, None).restore(&state);
                self.select(&t);
            }
            (None, uri) => {
                self.new_tab(uri.as_deref(), true);
            }
        }
    }

    pub fn ensure_view(self: &Rc<Self>, tab: &Rc<Tab>, related: Option<&View>) -> View {
        if let Some(v) = tab.view.borrow().as_ref() {
            return v.clone();
        }
        tab.parked.replace(None); // new navigation from the new-tab page drops Forward
        if tab.internal.take().is_some() {
            tab.set_icon(None); // drop the built-in page's icon; the site's favicon follows
        }
        let v = view::create(self, tab, related);
        while let Some(child) = tab.page.first_child() {
            tab.page.remove(&child);
        }
        tab.page.append(&v.widget());
        *tab.view.borrow_mut() = Some(v.clone());
        v
    }

    pub fn load(self: &Rc<Self>, tab: &Rc<Tab>, uri: &str) {
        tab.pending.replace(None);
        if let Some(page) = Internal::from_uri(uri) {
            self.show_internal(tab, page);
            return;
        }
        self.ensure_view(tab, None).load(uri);
    }

    pub fn select(self: &Rc<Self>, tab: &Rc<Tab>) {
        let prev = self.current.replace(Some(tab.clone()));
        if let Some(p) = prev {
            p.root.remove_css_class("active");
            if !Rc::ptr_eq(&p, tab) {
                let text = self.url.text();
                let edited = text != p.uri().unwrap_or_default();
                p.draft.replace(edited.then(|| text.to_string()));
                self.schedule_unload(&p);
            }
        }
        self.cancel_unload(tab);
        tab.root.add_css_class("active");
        self.stack.set_visible_child(&tab.page);
        self.viewport.scroll_to(&tab.root, None);
        let suspended = tab.suspended.take();
        if let Some(state) = suspended {
            // Reload an unloaded tab with its back/forward history intact.
            tab.pending.take();
            tab.root.remove_css_class("unloaded");
            self.ensure_view(tab, None).restore(&state);
        } else if let Some(u) = tab.pending.take() {
            self.load(tab, &u);
        }
        self.close_find();
        self.status.set_visible(false);
        // A tab switch always shows the new tab's address (sync_ui leaves the
        // bar alone while it has focus, to protect what the user is typing).
        // An unsent draft comes back with focus, so sync_ui leaves it alone.
        if let Some(draft) = tab.draft.take() {
            self.url.set_text(&draft);
            self.url.grab_focus();
            self.url.set_position(-1);
            self.sync_ui();
            return;
        }
        self.url.set_text(&tab.uri().unwrap_or_default());
        match tab.view.borrow().as_ref() {
            Some(v) => {
                v.grab_focus();
            }
            None if tab.internal.get().is_some() => {
                tab.page.child_focus(gtk::DirectionType::TabForward);
            }
            None => {
                self.url.grab_focus();
            }
        }
        self.sync_ui();
    }

    pub fn close_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        self.cancel_unload(tab);
        let idx = {
            let mut tabs = self.tabs.borrow_mut();
            let Some(i) = tabs.iter().position(|t| Rc::ptr_eq(t, tab)) else {
                return;
            };
            tabs.remove(i);
            i
        };
        if !self.private
            && let Some(u) = tab.uri()
        {
            self.closed.borrow_mut().push(u);
        }
        // Dropping the view releases its web process resources.
        tab.view.replace(None);
        self.stack.remove(&tab.page);
        self.update_tabbar();
        let widget = tab.widget.clone();
        widget.set_reveal_child(false);
        if widget.is_child_revealed() {
            let strip = self.strip.downgrade();
            widget.connect_child_revealed_notify(move |r| {
                if let Some(s) = strip.upgrade() {
                    s.remove(r);
                }
            });
        } else {
            self.strip.remove(&widget);
        }

        let was_current = self.current.borrow().as_ref().is_some_and(|c| Rc::ptr_eq(c, tab));
        if was_current {
            self.current.replace(None);
            let next = {
                let tabs = self.tabs.borrow();
                tabs.get(idx).or_else(|| tabs.get(idx.wrapping_sub(1))).cloned()
            };
            match next {
                Some(t) => self.select(&t),
                None => self.win.close(),
            }
        }
    }

    /// Close the tab showing `view`; false if it is not in this window.
    pub fn close_view(self: &Rc<Self>, view: &View) -> bool {
        let tab = self
            .tabs
            .borrow()
            .iter()
            .find(|t| t.view.borrow().as_ref() == Some(view))
            .cloned();
        tab.map(|t| self.close_tab(&t)).is_some()
    }

    /// Bottom-left toast offering a newer release. "Update" opens the
    /// release page; Mallow never installs anything itself.
    pub fn show_update_toast(self: &Rc<Self>, version: &str, url: &str) {
        let icon = gtk::Image::from_icon_name(crate::app::APP_ID);
        icon.set_pixel_size(32);
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let title = gtk::Label::builder()
            .label("Update available")
            .xalign(0.0)
            .build();
        title.add_css_class("toast-title");
        let sub = gtk::Label::builder()
            .label(format!("Mallow {version} is ready"))
            .xalign(0.0)
            .build();
        sub.add_css_class("toast-sub");
        text.append(&title);
        text.append(&sub);
        let later = gtk::Button::with_label("Later");
        later.add_css_class("text-btn");
        let update = gtk::Button::with_label("Update");
        update.add_css_class("filled");
        let toast = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        toast.add_css_class("toast");
        for w in [
            icon.upcast_ref::<gtk::Widget>(),
            text.upcast_ref(),
            later.upcast_ref(),
            update.upcast_ref(),
        ] {
            toast.append(w);
        }
        toast.update_property(&[gtk::accessible::Property::Label(&format!(
            "Mallow {version} is available"
        ))]);
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(250)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .child(&toast)
            .build();
        self.overlay.add_overlay(&revealer);
        revealer.set_reveal_child(true);

        let dismiss = {
            let (r, overlay) = (revealer.downgrade(), self.overlay.downgrade());
            move || {
                let (Some(r), Some(o)) = (r.upgrade(), overlay.upgrade()) else {
                    return;
                };
                r.set_reveal_child(false);
                let o2 = o.downgrade();
                r.connect_child_revealed_notify(move |r| {
                    if let Some(o) = o2.upgrade() {
                        o.remove_overlay(r);
                    }
                });
                if !r.is_child_revealed() {
                    o.remove_overlay(&r);
                }
            }
        };
        let dismiss = Rc::new(dismiss);
        let d = dismiss.clone();
        later.connect_clicked(move |_| d());
        let (w, url) = (Rc::downgrade(self), url.to_string());
        update.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.new_tab(Some(&url), true);
            }
            dismiss();
        });
    }

    /// Compact mode and tab-bar visibility from the current config.
    pub fn apply_prefs(&self) {
        if self.app.config.borrow().compact {
            self.win.add_css_class("compact");
        } else {
            self.win.remove_css_class("compact");
        }
        self.update_tabbar();
    }

    /// Arm unload timers for background tabs (after the setting turns on).
    pub fn apply_unloading(self: &Rc<Self>) {
        let background: Vec<Rc<Tab>> = self
            .tabs
            .borrow()
            .iter()
            .filter(|t| !self.is_current(t) && t.unload_timer.borrow().is_none())
            .cloned()
            .collect();
        for t in background {
            self.schedule_unload(&t);
        }
    }

    fn cancel_unload(&self, tab: &Tab) {
        if let Some(id) = tab.unload_timer.take() {
            id.remove();
        }
    }

    /// Arm a one-shot timer that unloads `tab` if it stays in the background.
    fn schedule_unload(self: &Rc<Self>, tab: &Rc<Tab>) {
        let (on, minutes) = {
            let c = self.app.config.borrow();
            (c.unload_tabs, c.unload_after)
        };
        if !on || tab.view.borrow().is_none() {
            return;
        }
        self.cancel_unload(tab);
        let (w, t) = (Rc::downgrade(self), Rc::downgrade(tab));
        let id = glib::timeout_add_seconds_local_once(minutes * 60, move || {
            if let (Some(w), Some(t)) = (w.upgrade(), t.upgrade()) {
                t.unload_timer.take(); // fired: the id is no longer valid
                w.unload(&t);
            }
        });
        tab.unload_timer.replace(Some(id));
    }

    /// Drop the tab's web view, keeping its history to restore on return.
    fn unload(self: &Rc<Self>, tab: &Rc<Tab>) {
        if self.is_current(tab) || !self.app.config.borrow().unload_tabs {
            return;
        }
        let Some(v) = tab.view.borrow().clone() else {
            return;
        };
        if v.is_playing_audio() || v.is_loading() {
            self.schedule_unload(tab); // busy: try again later
            return;
        }
        let Some(state) = v.session_state() else { return };
        tab.suspended.replace(Some(state));
        tab.pending.replace(v.uri());
        tab.view.replace(None);
        tab.parked.replace(None);
        while let Some(child) = tab.page.first_child() {
            tab.page.remove(&child);
        }
        // WebKit keeps a closed view's process cached for reuse; end it so
        // the memory is actually returned. The view is detached first, so
        // its final title/favicon changes no longer reach the tab.
        if !tab.shares_process.get() {
            v.terminate();
        }
        tab.root.add_css_class("unloaded");
    }

    pub fn loaded_count(&self) -> usize {
        self.tabs
            .borrow()
            .iter()
            .filter(|t| t.view.borrow().is_some())
            .count()
    }

    fn update_tabbar(&self) {
        let hide = self.app.config.borrow().hide_single_tab && self.tabs.borrow().len() <= 1;
        self.tabbar.set_visible(!hide);
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.borrow().len()
    }

    /// The network session if a page has created it already.
    pub fn live_session(&self) -> Option<engine::Session> {
        self.session.get().cloned()
    }

    fn confirm_close(self: &Rc<Self>, tabs: usize) {
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(format!("Close {tabs} tabs?"))
            .detail("You can turn this question off in Settings › General.")
            .buttons(["Cancel", "Close window"])
            .cancel_button(0)
            .default_button(1)
            .build();
        let w = Rc::downgrade(self);
        dialog.choose(Some(&self.win), None::<&gio::Cancellable>, move |r| {
            if let (Ok(1), Some(w)) = (r, w.upgrade()) {
                w.close_confirmed.set(true);
                w.win.close();
            }
        });
    }

    /// Focus the tab already showing `page`, reuse an empty new tab, or
    /// open a new one.
    pub fn open_internal(self: &Rc<Self>, page: Internal) {
        let existing = self
            .tabs
            .borrow()
            .iter()
            .find(|t| t.internal.get() == Some(page))
            .cloned();
        if let Some(t) = existing {
            self.select(&t);
            return;
        }
        let current = self.current.borrow().clone();
        let blank = current.filter(|t| {
            t.view.borrow().is_none() && t.pending.borrow().is_none() && t.internal.get().is_none()
        });
        let tab = blank.unwrap_or_else(|| self.add_tab());
        self.show_internal(&tab, page);
    }

    /// Turn `tab` into a built-in page. Only trusted callers reach this:
    /// the omnibox, menus, shortcuts and session restore.
    fn show_internal(self: &Rc<Self>, tab: &Rc<Tab>, page: Internal) {
        tab.view.replace(None);
        tab.parked.replace(None);
        tab.from_ntp.set(false);
        while let Some(child) = tab.page.first_child() {
            tab.page.remove(&child);
        }
        let content = match page {
            Internal::Settings => crate::settings::page(self),
            Internal::History => crate::history::page(self, tab),
        };
        tab.page.append(&content);
        tab.internal.set(Some(page));
        tab.set_title(page.title());
        tab.icon.set_icon_name(Some(page.icon()));
        self.select(tab);
    }

    fn cycle(self: &Rc<Self>, step: isize) {
        let next = {
            let tabs = self.tabs.borrow();
            let cur = self.current.borrow();
            let Some(i) = cur
                .as_ref()
                .and_then(|c| tabs.iter().position(|t| Rc::ptr_eq(t, c)))
            else {
                return;
            };
            let n = tabs.len() as isize;
            tabs[((i as isize + step).rem_euclid(n)) as usize].clone()
        };
        self.select(&next);
    }

    pub fn session(self: &Rc<Self>) -> &engine::Session {
        self.session
            .get_or_init(|| self.app.network_session(self.private.then_some(&self.downloads)))
    }

    pub fn is_current(&self, tab: &Tab) -> bool {
        self.current
            .borrow()
            .as_ref()
            .is_some_and(|c| std::ptr::eq(c.as_ref(), tab))
    }

    fn current_view(&self) -> Option<View> {
        self.current.borrow().as_ref()?.view.borrow().clone()
    }

    fn go_back(self: &Rc<Self>) {
        let Some(tab) = self.current.borrow().clone() else {
            return;
        };
        let Some(v) = tab.view.borrow().clone() else {
            return;
        };
        if v.can_go_back() {
            v.go_back();
        } else if tab.from_ntp.get() {
            v.stop();
            v.set_muted(true);
            tab.page.remove(&v.widget());
            tab.view.replace(None);
            tab.parked.replace(Some(v));
            tab.page.append(&self.new_tab_page(&tab));
            tab.set_title("");
            tab.set_icon(None);
            self.sync_ui();
            self.url.grab_focus();
        }
    }

    fn go_forward(self: &Rc<Self>) {
        let Some(tab) = self.current.borrow().clone() else {
            return;
        };
        if let Some(v) = tab.view.borrow().clone() {
            v.go_forward();
            return;
        }
        let Some(v) = tab.parked.take() else { return };
        while let Some(child) = tab.page.first_child() {
            tab.page.remove(&child);
        }
        v.set_muted(false);
        tab.page.append(&v.widget());
        tab.set_title(v.title().as_deref().unwrap_or(""));
        tab.set_icon(v.favicon().as_ref());
        tab.view.replace(Some(v.clone()));
        // Focus first: sync_ui leaves a focused address bar alone.
        v.grab_focus();
        self.sync_ui();
    }

    fn with_view(&self, f: impl FnOnce(&View)) {
        if let Some(v) = self.current_view() {
            f(&v);
        }
    }

    fn new_tab_page(self: &Rc<Self>, tab: &Rc<Tab>) -> gtk::Widget {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        col.add_css_class("ntp-column");
        col.set_valign(gtk::Align::Center);
        col.set_halign(gtk::Align::Center);
        col.set_vexpand(true);

        let logo = gtk::Image::from_icon_name(crate::app::APP_ID);
        logo.set_pixel_size(72);
        logo.set_accessible_role(gtk::AccessibleRole::Presentation);
        col.append(&logo);
        // Computed once per page; no clock timer keeps running.
        let hour = glib::DateTime::now_local().map_or(12, |d| d.hour());
        let greeting = match (self.private, hour) {
            (true, _) => "Private window",
            (_, 5..=11) => "Good morning",
            (_, 12..=17) => "Good afternoon",
            _ => "Good evening",
        };
        let title = gtk::Label::new(Some(greeting));
        title.add_css_class("ntp-title");
        col.append(&title);
        if self.private {
            let note = gtk::Label::new(Some(
                "History, cookies and site data from this window are discarded when it closes.\n\
                 Websites, your employer or your network provider can still see what you do.",
            ));
            note.set_justify(gtk::Justification::Center);
            note.set_wrap(true);
            note.add_css_class("ntp-note");
            col.append(&note);
        }

        let search = gtk::Entry::builder()
            .placeholder_text("Search the web or type a URL")
            .primary_icon_name("m-search-symbolic")
            .input_purpose(gtk::InputPurpose::Url)
            .width_chars(24)
            .max_width_chars(56)
            .build();
        search.add_css_class("ntp-search");
        crate::suggest::attach(&search, &self.app);
        search.update_property(&[gtk::accessible::Property::Label("Search the web or type a URL")]);
        let (w, t) = (Rc::downgrade(self), Rc::downgrade(tab));
        search.connect_activate(move |e| {
            let (Some(w), Some(t)) = (w.upgrade(), t.upgrade()) else {
                return;
            };
            let engine = w.app.config.borrow().search_url.clone();
            if let Some(uri) = util::resolve_input(&e.text(), &engine) {
                w.load(&t, &uri);
                w.sync_ui();
            }
        });
        col.append(&search);

        let bookmarks: Vec<(String, String)> = self
            .app
            .store()
            .map(|s| s.bookmarks.borrow().iter().take(8).cloned().collect())
            .unwrap_or_default();
        if !bookmarks.is_empty() {
            let flow = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .max_children_per_line(4)
                .min_children_per_line(2)
                .homogeneous(true)
                .halign(gtk::Align::Center)
                .build();
            flow.add_css_class("ntp-tiles");
            for (i, (url, name)) in bookmarks.into_iter().enumerate() {
                let host = util::host(&url).trim_start_matches("www.").to_string();
                let label = if name.is_empty() {
                    host.as_str()
                } else {
                    name.as_str()
                };
                let avatar = self.app.site_avatar(&url, label, i);
                avatar.set_halign(gtk::Align::Center);
                let caption = gtk::Label::builder()
                    .label(label)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(11)
                    .width_chars(11)
                    .build();
                caption.add_css_class("tile-caption");
                let inner = gtk::Box::new(gtk::Orientation::Vertical, 8);
                inner.append(&avatar);
                inner.append(&caption);
                let tile = gtk::Button::builder().child(&inner).tooltip_text(&url).build();
                tile.add_css_class("tile");
                let (w, t) = (Rc::downgrade(self), Rc::downgrade(tab));
                tile.connect_clicked(move |_| {
                    if let (Some(w), Some(t)) = (w.upgrade(), t.upgrade()) {
                        w.load(&t, &url);
                        w.sync_ui();
                    }
                });
                flow.append(&tile);
            }
            col.append(&flow);
        }

        let page = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&col)
            .build();
        page.add_css_class("ntp");
        page.upcast()
    }

    // ----- toolbar state -----

    /// Refresh toolbar and tab-strip state from the current tab.
    pub fn sync_ui(&self) {
        let Some(tab) = self.current.borrow().clone() else {
            return;
        };
        let view = tab.view.borrow().clone();
        let uri = tab.uri().unwrap_or_default();
        if !self.url.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN) {
            self.url.set_text(&uri);
        }
        let secure = uri.starts_with("https://")
            && view
                .as_ref()
                .and_then(|v| v.certificate())
                .is_some_and(|c| c.problems.is_none());
        let (icon, tip) = if let Some(page) = tab.internal.get() {
            (page.icon(), "Built-in Mallow page")
        } else if uri.is_empty() {
            ("m-search-symbolic", "Search or enter address")
        } else if secure {
            ("m-lock-symbolic", "Connection is secure")
        } else if uri.starts_with("http://") || uri.starts_with("https://") {
            ("m-warning-symbolic", "Connection is not secure")
        } else {
            ("m-public-symbolic", "Local or internal page")
        };
        self.url
            .set_icon_from_icon_name(gtk::EntryIconPosition::Primary, Some(icon));
        self.url
            .set_icon_tooltip_text(gtk::EntryIconPosition::Primary, Some(tip));
        if uri.is_empty() || tab.internal.get().is_some() {
            self.url
                .set_icon_from_icon_name(gtk::EntryIconPosition::Secondary, None);
        } else {
            let marked = self.app.store().is_some_and(|s| s.is_bookmarked(&uri));
            self.url.set_icon_from_icon_name(
                gtk::EntryIconPosition::Secondary,
                Some(if marked {
                    "m-star-filled-symbolic"
                } else {
                    "m-star-symbolic"
                }),
            );
            self.url.set_icon_tooltip_text(
                gtk::EntryIconPosition::Secondary,
                Some(if marked {
                    "Remove bookmark (Ctrl+D)"
                } else {
                    "Bookmark this page (Ctrl+D)"
                }),
            );
        }
        self.set_action_enabled(
            "back",
            view.as_ref()
                .is_some_and(|v| v.can_go_back() || tab.from_ntp.get()),
        );
        self.set_action_enabled(
            "forward",
            view.as_ref().is_some_and(|v| v.can_go_forward()) || tab.parked.borrow().is_some(),
        );
        self.set_action_enabled("reload", view.is_some());
        self.reload.set_sensitive(view.is_some());
        let loading = view.as_ref().is_some_and(|v| v.is_loading());
        self.reload.set_icon_name(if loading {
            "m-close-symbolic"
        } else {
            "m-refresh-symbolic"
        });
        self.reload
            .set_tooltip_text(Some(if loading { "Stop" } else { "Reload (Ctrl+R)" }));
        self.sync_progress();
        let base = if self.private {
            "Mallow — Private"
        } else {
            "Mallow"
        };
        let title = tab.title();
        self.win.set_title(Some(&if view.is_some() {
            format!("{title} — {base}")
        } else {
            base.to_string()
        }));
    }

    pub fn sync_progress(&self) {
        let Some(v) = self.current_view() else {
            self.progress.set_visible(false);
            return;
        };
        self.progress.set_visible(v.is_loading());
        self.progress.set_fraction(v.progress().max(0.05));
    }

    fn navigate_from_omnibox(self: &Rc<Self>) {
        let search = self.app.config.borrow().search_url.clone();
        let Some(uri) = util::resolve_input(&self.url.text(), &search) else {
            return;
        };
        let Some(tab) = self.current.borrow().clone() else {
            return;
        };
        self.load(&tab, &uri);
        if let Some(v) = tab.view.borrow().as_ref() {
            v.grab_focus();
        }
        self.sync_ui();
    }

    /// Popover attached to one of the omnibox icons; removed again on close.
    fn icon_popover(&self, pos: gtk::EntryIconPosition, child: &impl IsA<gtk::Widget>) -> gtk::Popover {
        let pop = gtk::Popover::builder()
            .child(child)
            .position(gtk::PositionType::Bottom)
            .build();
        pop.add_css_class("info-popover");
        // Parent to the toolbar, not the entry: as the entry's child, clicks
        // inside the popover reach the entry's text and dismiss the popover.
        let area = self.url.icon_area(pos);
        let anchor = self.url.parent().unwrap_or_else(|| self.url.clone().upcast());
        let origin = gtk::graphene::Point::new(area.x() as f32, area.y() as f32);
        let p = self.url.compute_point(&anchor, &origin).unwrap_or(origin);
        let (x, y) = (p.x(), p.y());
        pop.set_parent(&anchor);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(
            x as i32,
            y as i32,
            area.width(),
            area.height(),
        )));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop
    }

    /// Star / Ctrl+D: bookmark the page (if needed) and offer to rename or
    /// remove it, like Firefox's edit-bookmark panel.
    fn edit_bookmark(self: &Rc<Self>) {
        let Some(tab) = self.current.borrow().clone() else {
            return;
        };
        let (Some(uri), Some(store)) = (tab.uri(), self.app.store()) else {
            return;
        };
        let existing = store
            .bookmarks
            .borrow()
            .iter()
            .find(|(u, _)| *u == uri)
            .map(|(_, t)| t.clone());
        let is_new = existing.is_none();
        let title = existing.unwrap_or_else(|| tab.title());
        if is_new {
            store.bookmark(&uri, &title);
            self.sync_ui();
        }

        let heading = gtk::Label::builder()
            .label(if is_new { "Bookmark added" } else { "Edit bookmark" })
            .xalign(0.0)
            .build();
        heading.add_css_class("popover-title");
        let name_label = gtk::Label::builder().label("Name").xalign(0.0).build();
        name_label.add_css_class("field-label");
        let name = gtk::Entry::builder().text(&title).activates_default(true).build();
        name.add_css_class("field");
        name_label.set_mnemonic_widget(Some(&name));
        let remove = gtk::Button::with_label("Remove");
        remove.add_css_class("text-btn");
        let done = gtk::Button::with_label("Done");
        done.add_css_class("filled");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&remove);
        buttons.append(&done);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.add_css_class("bookmark-editor");
        for w in [
            heading.upcast_ref::<gtk::Widget>(),
            name_label.upcast_ref(),
            name.upcast_ref(),
            buttons.upcast_ref(),
        ] {
            content.append(w);
        }

        let pop = self.icon_popover(gtk::EntryIconPosition::Secondary, &content);
        pop.set_default_widget(Some(&done));
        let removed = Rc::new(std::cell::Cell::new(false));
        done.connect_clicked({
            let pop = pop.downgrade();
            move |_| {
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
            }
        });
        remove.connect_clicked({
            let (pop, removed, w, uri) = (pop.downgrade(), removed.clone(), Rc::downgrade(self), uri.clone());
            move |_| {
                removed.set(true);
                if let Some(s) = w.upgrade().as_ref().and_then(|w| w.app.store()) {
                    s.unbookmark(&uri);
                }
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
            }
        });
        // Closing by any means (Done, Enter, Escape, click outside) keeps the edit.
        pop.connect_closed({
            let w = Rc::downgrade(self);
            move |_| {
                let Some(w) = w.upgrade() else { return };
                let new_name = name.text().trim().to_string();
                if let Some(s) = w.app.store()
                    && !removed.get()
                    && !new_name.is_empty()
                    && new_name != title
                {
                    s.bookmark(&uri, &new_name);
                }
                w.sync_ui();
            }
        });
        pop.popup();
    }

    /// Lock icon: connection, privacy and this site's permissions, with the
    /// certificate on a second page.
    fn show_site_info(self: &Rc<Self>) {
        let Some(tab) = self.current.borrow().clone() else {
            return;
        };
        let Some(uri) = tab.uri() else { return };
        let host = util::host(&uri).to_string();
        let https = uri.starts_with("https://");
        let cert = self
            .current_view()
            .and_then(|v| v.certificate())
            .filter(|_| https);
        let (icon, tone, headline, detail) = match &cert {
            Some(c) if c.problems.is_none() => (
                "m-lock-symbolic",
                "good",
                "Connection is secure",
                "Information you send is encrypted in transit",
            ),
            Some(_) => (
                "m-warning-symbolic",
                "bad",
                "Certificate problem",
                "This site's identity couldn't be verified",
            ),
            None if uri.starts_with("http://") => (
                "m-warning-symbolic",
                "bad",
                "Connection is not secure",
                "Don't enter passwords or payment details",
            ),
            None => (
                "m-public-symbolic",
                "neutral",
                "Local page",
                "Loaded from this computer",
            ),
        };

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .transition_duration(220)
            .hhomogeneous(true)
            .vhomogeneous(false)
            .interpolate_size(true)
            .build();

        // --- main page
        let main = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let title = gtk::Label::builder()
            .label(if host.is_empty() {
                uri.as_str()
            } else {
                host.as_str()
            })
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        title.add_css_class("popover-title");
        title.set_margin_start(4);
        main.append(&title);

        let connection = info_row(icon, tone, headline, Some(detail));
        let conn_group = info_group();
        if cert.is_some() {
            let chevron = gtk::Image::from_icon_name("m-chevron_right-symbolic");
            chevron.add_css_class("row-chevron");
            connection.append(&chevron);
            let btn = gtk::Button::builder()
                .child(&connection)
                .tooltip_text("View certificate")
                .build();
            btn.add_css_class("info-row-btn");
            let stack = stack.downgrade();
            btn.connect_clicked(move |_| {
                if let Some(s) = stack.upgrade() {
                    s.set_visible_child_name("cert");
                }
            });
            conn_group.append(&btn);
        } else {
            conn_group.append(&connection);
        }
        main.append(&conn_group);

        let cfg = self.app.config.borrow();
        let privacy = info_group();
        privacy.append(&if cfg.block_trackers {
            info_row(
                "m-shield-symbolic",
                "good",
                "Trackers blocked",
                Some("Known third-party trackers can't load"),
            )
        } else {
            info_row(
                "m-shield-symbolic",
                "neutral",
                "Tracker blocking off",
                Some("Enable it in config.toml"),
            )
        });
        privacy.append(&match cfg.cookies {
            crate::config::Cookies::NoThirdParty => info_row(
                "m-cookie-symbolic",
                "good",
                "Third-party cookies blocked",
                Some("Other sites can't follow you here"),
            ),
            crate::config::Cookies::Never => info_row(
                "m-cookie-symbolic",
                "good",
                "All cookies blocked",
                Some("Some sites may not work"),
            ),
            crate::config::Cookies::Always => {
                info_row("m-cookie-symbolic", "neutral", "All cookies allowed", None)
            }
        });
        drop(cfg);
        if self.private {
            privacy.append(&info_row(
                "m-visibility_off-symbolic",
                "good",
                "Private window",
                Some("History and cookies are forgotten on close"),
            ));
        }
        main.append(&privacy);

        let origin = engine::origin(&uri);
        let mut perms: Vec<_> = self
            .permissions
            .borrow()
            .iter()
            .filter(|((site, _, _), _)| site == &origin)
            .map(|((_, current, requester), allowed)| (current.clone(), requester.clone(), *allowed))
            .collect();
        perms.sort();
        if !perms.is_empty() {
            let heading = gtk::Label::builder().label("Permissions").xalign(0.0).build();
            heading.add_css_class("section-title");
            heading.set_margin_start(4);
            main.append(&heading);
            let group = info_group();
            for (current, requester, allowed) in &perms {
                let label = format!("{requester} on {current}");
                let row = info_row("m-cookie-symbolic", "neutral", "Embedded cookies", Some(&label));
                let state = gtk::Label::new(Some(if *allowed { "Allowed" } else { "Blocked" }));
                state.add_css_class(if *allowed { "state-on" } else { "state-off" });
                row.append(&state);
                group.append(&row);
            }
            main.append(&group);
            let reset = gtk::Button::with_label("Reset permissions");
            reset.add_css_class("text-btn");
            reset.set_halign(gtk::Align::End);
            let (w, group) = (Rc::downgrade(self), group.downgrade());
            reset.connect_clicked(move |b| {
                if let Some(w) = w.upgrade() {
                    w.permissions
                        .borrow_mut()
                        .retain(|(site, _, _), _| site != &origin);
                }
                if let Some(g) = group.upgrade() {
                    g.set_sensitive(false);
                }
                b.set_label("Sites will ask again");
                b.set_sensitive(false);
            });
            main.append(&reset);
        }
        stack.add_named(&main, Some("main"));

        // --- certificate page
        if let Some(cert) = &cert {
            let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
            let back = gtk::Button::from_icon_name("m-arrow_back-symbolic");
            back.add_css_class("icon-btn");
            back.set_tooltip_text(Some("Back"));
            let stack_weak = stack.downgrade();
            back.connect_clicked(move |_| {
                if let Some(s) = stack_weak.upgrade() {
                    s.set_visible_child_name("main");
                }
            });
            let heading = gtk::Label::new(Some("Certificate"));
            heading.add_css_class("popover-title");
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            bar.append(&back);
            bar.append(&heading);
            page.append(&bar);

            let date = |d: Option<glib::DateTime>| {
                d.and_then(|d| d.format("%e %b %Y").ok())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            };
            let (subject, issuer) = (&cert.subject, &cert.issuer);
            let group = info_group();
            group.append(&field_row(
                "Issued to",
                util::dn_field(subject, "CN").unwrap_or(subject),
                false,
            ));
            group.append(&field_row(
                "Issued by",
                util::dn_field(issuer, "O")
                    .or(util::dn_field(issuer, "CN"))
                    .unwrap_or(issuer),
                false,
            ));
            group.append(&field_row(
                "Valid",
                &format!(
                    "{} – {}",
                    date(cert.not_before.clone()),
                    date(cert.not_after.clone())
                ),
                false,
            ));
            if let Some(problems) = &cert.problems {
                group.append(&field_row("Problems", problems, false));
            }
            if let Some(hex) = cert
                .der
                .as_ref()
                .and_then(|der| glib::compute_checksum_for_data(glib::ChecksumType::Sha256, der))
            {
                let pairs: Vec<String> = util::fingerprint(&hex).split(':').map(String::from).collect();
                let lines: Vec<String> = pairs.chunks(8).map(|c| c.join(":")).collect();
                group.append(&field_row("SHA-256 fingerprint", &lines.join("\n"), true));
            }
            page.append(&group);
            stack.add_named(&page, Some("cert"));
        }

        let pop = self.icon_popover(gtk::EntryIconPosition::Primary, &stack);
        pop.add_css_class("site-info-pop");
        pop.popup();
    }

    // ----- find -----

    fn open_find(&self) {
        if self.current_view().is_none() {
            return;
        }
        self.find_bar.set_reveal_child(true);
        self.find_entry.grab_focus();
        self.find(false);
    }

    fn close_find(&self) {
        if !self.find_bar.reveals_child() {
            return;
        }
        self.find_bar.set_reveal_child(false);
        self.find_label.set_text("");
        if let Some(v) = self.current_view() {
            v.find_finish();
            v.grab_focus();
        }
    }

    fn find(&self, _step: bool) {
        let Some(v) = self.current_view() else {
            return;
        };
        let text = self.find_entry.text();
        if text.is_empty() {
            self.find_label.set_text("");
        }
        v.find(&text);
    }

    fn find_step(&self, backwards: bool) {
        let Some(v) = self.current_view() else {
            return;
        };
        if v.find_text()
            .is_none_or(|t| t.as_str() != self.find_entry.text().as_str())
        {
            self.find(false);
        } else {
            v.find_next(backwards);
        }
    }

    // ----- prompts -----

    /// Ask once per host and kind for this window; the request stays pending
    /// until the user answers.
    pub fn ask_permission(self: &Rc<Self>, view: &View, req: PermissionRequest) {
        // Settings › Privacy can block a kind outright: deny without asking.
        let defaults = self.app.config.borrow().permissions;
        let block = |a: crate::config::Ask| a == crate::config::Ask::Block;
        let blocked = match req.kind {
            Permission::Media { video, audio } => {
                (video && block(defaults.camera)) || (audio && block(defaults.microphone))
            }
            Permission::Location => block(defaults.location),
            Permission::Notifications => block(defaults.notifications),
            _ => false,
        };
        if blocked {
            req.deny();
            return;
        }
        let kind = match &req.kind {
            Permission::Media { video, audio } => match (video, audio) {
                (true, true) => "use your camera and microphone",
                (true, false) => "use your camera",
                (false, true) => "use your microphone",
                _ => "capture your screen",
            },
            Permission::Location => "know your location",
            Permission::Notifications => "show notifications",
            Permission::Clipboard => "read your clipboard",
            Permission::PointerLock => "hide and lock your mouse pointer",
            Permission::DeviceInfo => "list your cameras and microphones",
            Permission::MediaKeys => "play protected (DRM) content",
            Permission::StorageAccess { .. } => "use cookies while embedded in another site",
            Permission::Other => {
                req.deny();
                return;
            }
        };
        let origin = engine::origin(&view.uri().unwrap_or_default());
        let (key, message) = if let Permission::StorageAccess { current, requester } = &req.kind {
            let message = format!("Allow {requester} to use cookies while embedded on {current}?");
            (Some((origin, current.clone(), requester.clone())), message)
        } else {
            // These requests expose no requesting-frame origin. Prompt for each request.
            (None, format!("Allow this page on {origin} to {kind}?"))
        };
        let known = key
            .as_ref()
            .and_then(|key| self.permissions.borrow().get(key).copied());
        if let Some(allowed) = known {
            if allowed {
                req.allow()
            } else {
                req.deny()
            }
            return;
        }
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(message)
            .detail(if key.is_some() {
                "Your choice is remembered until this window closes."
            } else {
                "Your choice applies to this request."
            })
            .buttons(["Block", "Allow"])
            .cancel_button(0)
            .default_button(0)
            .build();
        let map = Rc::downgrade(self);
        dialog.choose(Some(&self.win), None::<&gio::Cancellable>, move |r| {
            let allowed = r == Ok(1);
            if let (Some(w), Some(key)) = (map.upgrade(), key) {
                w.permissions.borrow_mut().insert(key, allowed);
            }
            if allowed { req.allow() } else { req.deny() }
        });
    }

    /// Hand a non-web URI (mailto:, magnet:, …) to the desktop after asking.
    pub fn ask_external(&self, uri: &str) {
        let scheme = util::scheme(uri).unwrap_or("unknown").to_string();
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(format!("Open this {scheme}: link in another application?"))
            .detail(uri.chars().take(300).collect::<String>())
            .buttons(["Cancel", "Open"])
            .cancel_button(0)
            .default_button(0)
            .build();
        let (uri, win) = (uri.to_string(), self.win.clone());
        dialog.choose(Some(&self.win), None::<&gio::Cancellable>, move |r| {
            if r == Ok(1) {
                gtk::UriLauncher::new(&uri).launch(Some(&win), None::<&gio::Cancellable>, |r| {
                    if let Err(e) = r {
                        eprintln!("mallow: no application for link: {e}");
                    }
                });
            }
        });
    }

    fn confirm_clear_data(self: &Rc<Self>) {
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message("Clear browsing data?")
            .detail("Deletes history, cookies, cache and site storage. You will be signed out of websites. Bookmarks are kept.")
            .buttons(["Cancel", "Clear"])
            .cancel_button(0)
            .default_button(0)
            .build();
        let app = Rc::downgrade(&self.app);
        dialog.choose(Some(&self.win), None::<&gio::Cancellable>, move |r| {
            if let (Ok(1), Some(app)) = (r, app.upgrade()) {
                app.clear_browsing_data();
            }
        });
    }

    fn open_in_new_tab(self: &Rc<Self>) -> Box<dyn Fn(&str)> {
        let w = Rc::downgrade(self);
        Box::new(move |url: &str| {
            if let Some(w) = w.upgrade() {
                w.new_tab(Some(url), true);
            }
        })
    }

    fn show_bookmarks(self: &Rc<Self>) {
        let Some(store) = self.app.store() else { return };
        let w = Rc::downgrade(self);
        lists::show(
            self.win.upcast_ref(),
            lists::ListSpec {
                title: "Bookmarks",
                rows: store.bookmarks.borrow().clone(),
                open: self.open_in_new_tab(),
                remove: Some(Box::new(move |url: &str| {
                    if let Some(w) = w.upgrade() {
                        if let Some(s) = w.app.store() {
                            s.unbookmark(url);
                        }
                        w.sync_ui();
                    }
                })),
                clear: None,
            },
        );
    }
}

fn info_group() -> gtk::Box {
    let g = gtk::Box::new(gtk::Orientation::Vertical, 2);
    g.add_css_class("info-group");
    g
}

/// Two-line list item with a round tonal icon (Material 3 list item).
fn info_row(icon: &str, tone: &str, title: &str, subtitle: Option<&str>) -> gtk::Box {
    let ic = gtk::Image::from_icon_name(icon);
    ic.add_css_class("info-icon");
    ic.add_css_class(tone);
    ic.set_valign(gtk::Align::Center);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let t = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    t.add_css_class("row-title");
    text.append(&t);
    if let Some(sub) = subtitle {
        let s = gtk::Label::builder()
            .label(sub)
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(30)
            .build();
        s.add_css_class("row-sub");
        text.append(&s);
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    row.add_css_class("info-row");
    row.append(&ic);
    row.append(&text);
    row
}

/// Label-over-value item for the certificate page.
fn field_row(label: &str, value: &str, mono: bool) -> gtk::Box {
    let l = gtk::Label::builder().label(label).xalign(0.0).build();
    l.add_css_class("row-overline");
    let v = gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(36)
        .build();
    v.add_css_class(if mono { "row-mono" } else { "row-title" });
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    b.add_css_class("info-row");
    b.append(&l);
    b.append(&v);
    b
}
