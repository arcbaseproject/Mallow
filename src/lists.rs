//! One searchable list window, used for both history and bookmarks.

use gtk::prelude::*;
use std::rc::Rc;

pub type UrlFn = Box<dyn Fn(&str)>;

pub struct ListSpec {
    pub title: &'static str,
    pub rows: Vec<(String, String)>,
    pub open: UrlFn,
    /// Per-row delete button (bookmarks).
    pub remove: Option<UrlFn>,
    /// Footer button with confirmation text (history: clear all).
    pub clear: Option<(&'static str, Box<dyn Fn()>)>,
}

pub fn show(parent: &gtk::Window, spec: ListSpec) {
    let ListSpec {
        title,
        rows,
        open,
        remove,
        clear,
    } = spec;
    let win = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .default_width(560)
        .default_height(620)
        .build();
    win.add_css_class("mallow");

    let search = gtk::SearchEntry::builder()
        .placeholder_text(format!("Search {}", title.to_lowercase()))
        .build();
    search.add_css_class("omnibox");
    let list = gtk::ListBox::new();
    list.add_css_class("item-list");
    let placeholder = gtk::Label::new(Some("Nothing here yet"));
    placeholder.add_css_class("dim-label");
    placeholder.set_margin_top(24);
    list.set_placeholder(Some(&placeholder));

    let haystack: Rc<Vec<String>> = Rc::new(
        rows.iter()
            .map(|(u, t)| format!("{t} {u}").to_lowercase())
            .collect(),
    );
    let urls: Rc<Vec<String>> = Rc::new(rows.iter().map(|(u, _)| u.clone()).collect());
    let remove = remove.map(Rc::new);

    for (url, title) in &rows {
        let t = gtk::Label::builder()
            .label(if title.is_empty() { url } else { title })
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let u = gtk::Label::builder()
            .label(url)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        u.add_css_class("dim-label");
        u.add_css_class("caption");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.append(&t);
        text.append(&u);
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row_box.append(&text);
        let row = gtk::ListBoxRow::builder().child(&row_box).build();
        if let Some(remove) = &remove {
            let del = gtk::Button::from_icon_name("m-close-symbolic");
            del.add_css_class("icon-btn");
            del.set_tooltip_text(Some("Remove"));
            del.set_valign(gtk::Align::Center);
            let (remove, url, row) = (remove.clone(), url.clone(), row.downgrade());
            del.connect_clicked(move |_| {
                remove(&url);
                if let Some(row) = row.upgrade() {
                    row.set_visible(false);
                }
            });
            row_box.append(&del);
        }
        list.append(&row);
    }

    {
        let (haystack, search) = (haystack.clone(), search.clone());
        list.set_filter_func(move |row| {
            let q = search.text().to_lowercase();
            q.is_empty() || haystack.get(row.index() as usize).is_some_and(|h| h.contains(&q))
        });
    }
    search.connect_search_changed({
        let list = list.clone();
        move |_| list.invalidate_filter()
    });
    list.connect_row_activated({
        let win = win.downgrade();
        move |_, row| {
            if let Some(url) = urls.get(row.index() as usize) {
                open(url);
            }
            if let Some(w) = win.upgrade() {
                w.close();
            }
        }
    });
    search.connect_activate({
        let list = list.clone();
        move |_| {
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                if row.is_child_visible() && row.is_visible() {
                    row.activate();
                    break;
                }
                i += 1;
            }
        }
    });

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.add_css_class("list-window");
    content.append(&search);
    content.append(&gtk::ScrolledWindow::builder().vexpand(true).child(&list).build());
    if let Some((label, action)) = clear {
        let btn = gtk::Button::with_label(label);
        btn.add_css_class("tonal");
        btn.set_halign(gtk::Align::End);
        let (list, win_weak) = (list.clone(), win.downgrade());
        let action = Rc::new(action);
        btn.connect_clicked(move |_| {
            let dialog = gtk::AlertDialog::builder()
                .modal(true)
                .message(format!("{label}?"))
                .detail("This cannot be undone.")
                .buttons(["Cancel", label])
                .cancel_button(0)
                .default_button(0)
                .build();
            let (list, action) = (list.clone(), action.clone());
            dialog.choose(
                win_weak.upgrade().as_ref(),
                None::<&gtk::gio::Cancellable>,
                move |r| {
                    if r == Ok(1) {
                        action();
                        list.remove_all();
                    }
                },
            );
        });
        content.append(&btn);
    }
    win.set_child(Some(&content));
    win.present();
    search.grab_focus();
}
