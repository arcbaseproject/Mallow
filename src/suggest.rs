//! Address suggestions under a URL entry: the search engine's suggestions,
//! then bookmarks and history. Typed searches go to the chosen preset engine;
//! typed addresses and custom engines stay on the machine.

use crate::{app::App, config, util};
use gtk::{gdk, glib, prelude::*};
use soup::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const LIMIT: usize = 8;
/// At most this many of the rows are search-engine suggestions.
const SEARCH_LIMIT: usize = 4;

/// Show suggestions while `entry` is focused and has text. Picking one puts its
/// URL in the entry and activates it, so the entry's own handler navigates.
pub fn attach(entry: &gtk::Entry, app: &Rc<App>) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .focusable(false)
        .build();
    list.add_css_class("suggest-list");
    let pop = gtk::Popover::builder()
        .child(&list)
        .position(gtk::PositionType::Bottom)
        .has_arrow(false)
        .autohide(false)
        .can_focus(false)
        .build();
    pop.add_css_class("suggest");

    let hide = {
        let pop = pop.downgrade();
        move || {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
        }
    };

    // URL of each row, by index.
    let urls: Rc<RefCell<Vec<String>>> = Rc::default();
    // Set while a pick writes the URL into the entry, so that edit isn't searched.
    let picking: Rc<Cell<bool>> = Rc::default();
    let pick = {
        let (entry, urls, picking) = (entry.downgrade(), urls.clone(), picking.clone());
        let hide = hide.clone();
        move |row: &gtk::ListBoxRow| {
            let Some(e) = entry.upgrade() else { return };
            let Some(url) = urls.borrow().get(row.index() as usize).cloned() else {
                return;
            };
            hide();
            picking.set(true);
            e.set_text(&url);
            picking.set(false);
            e.emit_activate();
        }
    };
    let p = pick.clone();
    list.connect_row_activated(move |_, row| p(row));

    let weak_app = Rc::downgrade(app);
    let (e, l, pp) = (entry.downgrade(), list.downgrade(), pop.downgrade());
    entry.connect_changed(move |entry| {
        let (Some(app), Some(list), Some(pop)) = (weak_app.upgrade(), l.upgrade(), pp.upgrade()) else {
            return;
        };
        if picking.get() {
            return;
        }
        let query = entry.text().trim().to_lowercase();
        if query.is_empty() || !entry.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN) {
            pop.popdown();
            return;
        }
        // Bookmarks are in memory; history comes from the database thread.
        let marks: Vec<(String, String)> = app.store().map_or_else(Vec::new, |s| {
            s.bookmarks
                .borrow()
                .iter()
                .filter(|(u, t)| u.to_lowercase().contains(&query) || t.to_lowercase().contains(&query))
                .take(LIMIT)
                .cloned()
                .collect()
        });
        let history = app.store().map(|s| s.suggest(&query, LIMIT as u32));
        let searches = glib::spawn_future_local(search_suggestions(
            app.config.borrow().search_url.clone(),
            query.clone(),
        ));
        let (e, urls) = (e.clone(), urls.clone());
        glib::spawn_future_local(async move {
            let visits = match history {
                Some(rx) => rx.await.unwrap_or_default(),
                None => Vec::new(),
            };
            let searches = searches.await.unwrap_or_default();
            let Some(entry) = e.upgrade() else { return };
            // A newer keystroke has its own query in flight; a removed entry
            // (new-tab page replaced) has no window to show a popover in.
            if entry.text().trim().to_lowercase() != query
                || !entry.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN)
                || entry.root().is_none()
            {
                return;
            }
            // A search row's "URL" is its text: the entry turns it into a search.
            let mut rows: Vec<(String, String)> = searches
                .into_iter()
                .take(SEARCH_LIMIT)
                .map(|s| (s, String::new()))
                .collect();
            let n_search = rows.len();
            for (url, title) in marks
                .into_iter()
                .chain(visits.into_iter().map(|v| (v.url, v.title)))
            {
                if !rows.iter().any(|(u, _)| *u == url) {
                    rows.push((url, title));
                }
            }
            rows.truncate(LIMIT);
            list.remove_all();
            if rows.is_empty() {
                pop.popdown();
                return;
            }
            for (i, (url, title)) in rows.iter().enumerate() {
                list.append(&suggestion_row(&app, url, title, i < n_search));
            }
            urls.replace(rows.into_iter().map(|(u, _)| u).collect());
            if pop.parent().is_none() {
                // Toolbar parent, like the omnibox icon popovers: as the
                // entry's child, clicks inside would reach the entry text.
                let anchor = entry.parent().unwrap_or_else(|| entry.clone().upcast());
                pop.set_parent(&anchor);
            }
            if let Some(anchor) = pop.parent()
                && let Some(b) = entry.compute_bounds(&anchor)
            {
                pop.set_pointing_to(Some(&gdk::Rectangle::new(
                    b.x() as i32,
                    b.y() as i32,
                    b.width() as i32,
                    b.height() as i32,
                )));
                list.set_size_request(b.width() as i32, -1);
            }
            pop.popup();
        });
    });

    // Up/Down move through suggestions while typing continues in the entry.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (l, pp) = (list.downgrade(), pop.downgrade());
    keys.connect_key_pressed(move |_, key, _, _| {
        let (Some(list), Some(pop)) = (l.upgrade(), pp.upgrade()) else {
            return glib::Propagation::Proceed;
        };
        if !pop.is_visible() {
            return glib::Propagation::Proceed;
        }
        let selected = list.selected_row().map(|r| r.index());
        let target = match key {
            gdk::Key::Down => Some(selected.map_or(0, |i| i + 1)),
            gdk::Key::Up => match selected {
                Some(0) | None => {
                    list.unselect_all();
                    return glib::Propagation::Stop;
                }
                Some(i) => Some(i - 1),
            },
            gdk::Key::Return | gdk::Key::KP_Enter => {
                if let Some(row) = list.selected_row() {
                    pick(&row);
                    return glib::Propagation::Stop;
                }
                pop.popdown();
                return glib::Propagation::Proceed;
            }
            // First Escape closes the list; the next one restores the URL.
            gdk::Key::Escape => {
                pop.popdown();
                return glib::Propagation::Stop;
            }
            _ => None,
        };
        if let Some(row) = target.and_then(|i| list.row_at_index(i)) {
            list.select_row(Some(&row));
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(keys);

    // Deferred, so a click on a row lands before the list goes away.
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| {
        let hide = hide.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), hide);
    });
    entry.add_controller(focus);
    let pp = pop.downgrade();
    entry.connect_activate(move |_| {
        if let Some(p) = pp.upgrade() {
            p.popdown();
        }
    });
    entry.connect_destroy(move |_| {
        if pop.parent().is_some() {
            pop.unparent();
        }
    });
}

/// The preset engine's suggestions for `query`. None for a custom engine, or
/// when `query` is an address rather than a search, so neither is sent anywhere.
async fn search_suggestions(search_url: String, query: String) -> Vec<String> {
    let Some((_, _, template)) = config::SEARCH_ENGINES.iter().find(|e| e.1 == search_url) else {
        return Vec::new();
    };
    // With an empty template a search resolves to "", and an address never does.
    if util::resolve_input(&query, "").as_deref() != Some("") {
        return Vec::new();
    }
    let Ok(msg) = soup::Message::new("GET", &template.replace("{}", &util::encode_query(&query))) else {
        return Vec::new();
    };
    let session = soup::Session::new();
    session.set_user_agent(concat!("Mallow/", env!("CARGO_PKG_VERSION")));
    session.set_timeout(5);
    match session.send_and_read_future(&msg, glib::Priority::DEFAULT).await {
        Ok(body) if msg.status() == soup::Status::Ok => parse_suggestions(&body),
        _ => Vec::new(),
    }
}

/// OpenSearch suggestions JSON: `["query", ["one", "two"], ...]`.
fn parse_suggestions(body: &[u8]) -> Vec<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let list = v.get(1)?.as_array()?;
            Some(list.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        })
        .unwrap_or_default()
}

fn suggestion_row(app: &Rc<App>, url: &str, title: &str, search: bool) -> gtk::ListBoxRow {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    b.set_hexpand(true);
    let label = |text: &str, class: &str| {
        let l = gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            // Natural width would be the whole URL; the list's size request sets the real width.
            .max_width_chars(1)
            .build();
        l.add_css_class(class);
        l
    };
    b.append(&label(
        if title.is_empty() { url } else { title },
        "suggest-title",
    ));
    if !title.is_empty() {
        b.append(&label(url, "suggest-url"));
    }
    let icon = gtk::Image::from_icon_name(if search {
        "m-search-symbolic"
    } else {
        "m-public-symbolic"
    });
    icon.add_css_class("suggest-icon");
    if !search {
        let session = app.network_session(None);
        let (weak, page) = (icon.downgrade(), url.to_string());
        glib::spawn_future_local(async move {
            if let (Some(texture), Some(icon)) = (session.favicon(&page).await, weak.upgrade()) {
                icon.set_paintable(Some(&texture));
            }
        });
    }
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    line.append(&icon);
    line.append(&b);
    let row = gtk::ListBoxRow::builder().child(&line).focusable(false).build();
    let label = if search {
        format!("Search for {url}")
    } else {
        url.to_string()
    };
    row.update_property(&[gtk::accessible::Property::Label(&label)]);
    row
}

#[cfg(test)]
mod tests {
    use super::parse_suggestions;

    #[test]
    fn parses_opensearch_suggestions() {
        let google = br#"["rust",["rustdesk","rust game"],[],{"google:suggesttype":[]}]"#;
        assert_eq!(parse_suggestions(google), ["rustdesk", "rust game"]);
        assert!(parse_suggestions(b"<html>").is_empty());
        assert!(parse_suggestions(br#"{"q":"rust"}"#).is_empty());
    }
}
