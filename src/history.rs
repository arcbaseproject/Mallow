//! Native history page shown at `mallow://history`.
//!
//! The list reloads each time the tab is shown, so it is never stale and
//! nothing runs while it is hidden.

use crate::{
    store::Visit,
    util,
    window::{Tab, Window},
};
use gtk::{gio, glib, prelude::*};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
};

const LIMIT: u32 = 1000;
const COLUMN: i32 = 760;

struct Page {
    win: Weak<Window>,
    tab: Weak<Tab>,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
    empty: gtk::Label,
    clear: gtk::Button,
    /// Lower-cased "title url" per row, for search.
    hay: RefCell<HashMap<gtk::ListBoxRow, String>>,
}

pub fn page(w: &Rc<Window>, tab: &Rc<Tab>) -> gtk::Widget {
    let title = gtk::Label::builder()
        .label("History")
        .xalign(0.0)
        .hexpand(true)
        .build();
    title.add_css_class("settings-title");
    title.set_accessible_role(gtk::AccessibleRole::Heading);
    let clear = gtk::Button::with_label("Clear history");
    clear.add_css_class("tonal-btn");
    clear.set_valign(gtk::Align::Center);
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    head.append(&title);
    head.append(&clear);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search history")
        .hexpand(true)
        .build();
    search.add_css_class("ntp-search");
    search.add_css_class("history-search");

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("history-list");
    let empty = gtk::Label::new(Some("Loading…"));
    empty.add_css_class("settings-row-sub");
    empty.set_margin_top(32);
    list.set_placeholder(Some(&empty));

    let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
    column.add_css_class("settings-column");
    column.append(&head);
    column.append(&search);
    column.append(&list);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&column)
        .build();
    // Width probe, as on the settings page: centre a column of COLUMN px.
    let probe = gtk::DrawingArea::builder()
        .hexpand(true)
        .content_height(0)
        .build();
    probe.connect_resize(move |_, width, _| {
        let margin = ((width - COLUMN) / 2).max(16);
        column.set_margin_start(margin);
        column.set_margin_end(margin);
    });
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("settings");
    root.append(&probe);
    root.append(&scroller);

    let page = Rc::new(Page {
        win: Rc::downgrade(w),
        tab: Rc::downgrade(tab),
        list: list.clone(),
        search: search.clone(),
        empty,
        clear: clear.clone(),
        hay: RefCell::default(),
    });

    list.set_header_func(|row, before| {
        let day = row.widget_name();
        if before.is_some_and(|b| b.widget_name() == day) {
            row.set_header(None::<&gtk::Widget>);
        } else {
            let h = gtk::Label::builder().label(day.as_str()).xalign(0.0).build();
            h.add_css_class("settings-section");
            h.add_css_class("history-day");
            row.set_header(Some(&h));
        }
    });
    {
        let p = Rc::downgrade(&page);
        list.set_filter_func(move |row| {
            let Some(p) = p.upgrade() else { return true };
            let q = p.search.text().to_lowercase();
            q.is_empty() || p.hay.borrow().get(row).is_some_and(|h| h.contains(&q))
        });
    }
    {
        let p = Rc::downgrade(&page);
        search.connect_search_changed(move |_| {
            if let Some(p) = p.upgrade() {
                p.list.invalidate_filter();
                p.list.invalidate_headers();
                p.update_empty();
            }
        });
    }
    // Click or Enter opens the entry in this tab; the row tooltip holds its URL.
    {
        let p = Rc::downgrade(&page);
        list.connect_row_activated(move |_, row| {
            let Some(p) = p.upgrade() else { return };
            if let (Some(w), Some(t), Some(url)) = (p.win.upgrade(), p.tab.upgrade(), row.tooltip_text()) {
                w.load(&t, &url);
            }
        });
    }
    {
        let p = Rc::downgrade(&page);
        clear.connect_clicked(move |b| {
            let Some(p) = p.upgrade() else { return };
            let dialog = gtk::AlertDialog::builder()
                .modal(true)
                .message("Clear all history?")
                .detail("Bookmarks, cookies and site data stay. This can't be undone.")
                .buttons(["Cancel", "Clear"])
                .cancel_button(0)
                .default_button(0)
                .build();
            let weak = Rc::downgrade(&p);
            let parent = b.root().and_downcast::<gtk::Window>();
            dialog.choose(parent.as_ref(), None::<&gio::Cancellable>, move |r| {
                let Some(p) = weak.upgrade() else { return };
                if r != Ok(1) {
                    return;
                }
                if let Some(s) = p.win.upgrade().as_ref().and_then(|w| w.app.store()) {
                    s.clear_history();
                }
                p.list.remove_all();
                p.hay.borrow_mut().clear();
                p.update_empty();
            });
        });
    }
    // Reload whenever the tab becomes visible.
    let p = Rc::downgrade(&page);
    root.connect_map(move |_| {
        if let Some(p) = p.upgrade() {
            p.reload();
        }
    });
    // Keep `page` alive as long as its widgets.
    root.connect_destroy(move |_| {
        let _ = &page;
    });
    root.upcast()
}

/// "Today", "Yesterday", or e.g. "Monday, 17 November".
fn day_label(when: &glib::DateTime, now: &glib::DateTime) -> String {
    let same = |a: &glib::DateTime, b: &glib::DateTime| a.ymd() == b.ymd();
    if same(when, now) {
        "Today".into()
    } else if now.add_days(-1).is_ok_and(|y| same(when, &y)) {
        "Yesterday".into()
    } else if when.year() == now.year() {
        when.format("%A, %-d %B").map(String::from).unwrap_or_default()
    } else {
        when.format("%-d %B %Y").map(String::from).unwrap_or_default()
    }
}

impl Page {
    fn reload(self: &Rc<Self>) {
        let Some(store) = self
            .win
            .upgrade()
            .and_then(|w| w.app.store().map(|s| s.history(LIMIT)))
        else {
            self.empty.set_text("History is unavailable");
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(visits) = store.await else { return };
            if let Some(p) = weak.upgrade() {
                p.fill(visits);
            }
        });
    }

    fn fill(self: &Rc<Self>, visits: Vec<Visit>) {
        self.list.remove_all();
        let mut hay = self.hay.borrow_mut();
        hay.clear();
        let now = glib::DateTime::now_local().ok();
        for (i, v) in visits.iter().enumerate() {
            let when = glib::DateTime::from_unix_local(v.when).ok();
            let day = match (&when, &now) {
                (Some(w), Some(n)) => day_label(w, n),
                _ => String::new(),
            };
            let row = self.row(v, i, when.as_ref());
            row.set_widget_name(&day);
            hay.insert(row.clone(), format!("{} {}", v.title, v.url).to_lowercase());
            self.list.append(&row);
        }
        drop(hay);
        self.clear.set_sensitive(!visits.is_empty());
        self.update_empty();
    }

    fn update_empty(&self) {
        let searching = !self.search.text().is_empty();
        self.empty.set_text(if self.hay.borrow().is_empty() {
            "Pages you visit appear here"
        } else if searching {
            "No pages match your search"
        } else {
            ""
        });
    }

    fn row(self: &Rc<Self>, v: &Visit, i: usize, when: Option<&glib::DateTime>) -> gtk::ListBoxRow {
        let host = util::host(&v.url).trim_start_matches("www.").to_string();
        let name = if v.title.is_empty() {
            host.clone()
        } else {
            v.title.clone()
        };
        let avatar = match self.win.upgrade() {
            Some(w) => w.app.site_avatar(&v.url, &name, i),
            None => gtk::Box::new(gtk::Orientation::Horizontal, 0),
        };
        avatar.add_css_class("history-avatar");
        avatar.set_valign(gtk::Align::Center);
        let title = gtk::Label::builder()
            .label(&name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        title.add_css_class("settings-row-title");
        let time = when
            .and_then(|w| w.format("%H:%M").ok())
            .map(String::from)
            .unwrap_or_default();
        let sub = gtk::Label::builder()
            .label(if time.is_empty() {
                host.clone()
            } else {
                format!("{host} · {time}")
            })
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        sub.add_css_class("settings-row-sub");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        text.append(&title);
        text.append(&sub);
        let remove = gtk::Button::from_icon_name("m-close-symbolic");
        remove.add_css_class("icon-btn");
        remove.add_css_class("history-remove");
        remove.set_tooltip_text(Some("Remove from history"));
        remove.set_valign(gtk::Align::Center);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        content.add_css_class("history-entry");
        content.append(&avatar);
        content.append(&text);
        content.append(&remove);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(true)
            .tooltip_text(&v.url)
            .build();
        row.update_property(&[gtk::accessible::Property::Label(&name)]);

        let url = v.url.clone();
        let (p, r) = (Rc::downgrade(self), row.downgrade());
        remove.connect_clicked(move |_| {
            let (Some(p), Some(r)) = (p.upgrade(), r.upgrade()) else {
                return;
            };
            if let Some(s) = p.win.upgrade().as_ref().and_then(|w| w.app.store()) {
                s.forget(&url);
            }
            p.hay.borrow_mut().remove(&r);
            p.list.remove(&r);
            p.clear.set_sensitive(!p.hay.borrow().is_empty());
            p.update_empty();
        });
        // Middle-click opens a new tab; click or Enter is handled by the list.
        let url = v.url.clone();
        let p = Rc::downgrade(self);
        let middle = gtk::GestureClick::builder()
            .button(gtk::gdk::BUTTON_MIDDLE)
            .build();
        middle.connect_released(move |_, _, _, _| {
            if let Some(w) = p.upgrade().and_then(|p| p.win.upgrade()) {
                w.new_tab(Some(&url), false);
            }
        });
        content.add_controller(middle);
        row
    }
}
