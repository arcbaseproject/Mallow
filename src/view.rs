//! WebView creation and the policy hooks that keep web content contained.

use crate::{
    engine::{LoadError, LoadEvent, NavigationAction, Response, View},
    util,
    window::{Tab, Window},
};
use gtk::prelude::*;
use std::rc::{Rc, Weak};

/// Clone two weak refs into a closure that upgrades both or does nothing.
fn pair(w: &Rc<Window>, t: &Rc<Tab>) -> impl Fn() -> Option<(Rc<Window>, Rc<Tab>)> + Clone + 'static {
    let (w, t): (Weak<Window>, Weak<Tab>) = (Rc::downgrade(w), Rc::downgrade(t));
    move || Some((w.upgrade()?, t.upgrade()?))
}

pub fn create(win: &Rc<Window>, tab: &Rc<Tab>, related: Option<&View>) -> View {
    let autoplay = win.app.config.borrow().autoplay;
    let view = View::new(&win.app.engine, win.session(), related, autoplay);
    crate::passwords::browser::attach(win, tab, &view);
    let widget = view.widget();
    widget.set_vexpand(true);
    widget.set_hexpand(true);
    view.set_zoom(win.app.config.borrow().default_zoom);

    let get = pair(win, tab);
    let g = get.clone();
    view.connect_title(move |v| {
        let Some((w, t)) = g() else { return };
        if t.view.borrow().as_ref() != Some(v) {
            return; // detached (unloaded or parked) view
        }
        t.set_title(v.title().as_deref().unwrap_or(""));
        if w.is_current(&t) {
            w.sync_ui();
        }
    });
    let g = get.clone();
    view.connect_favicon(move |v| {
        if let Some((_, t)) = g()
            && t.view.borrow().as_ref() == Some(v)
        {
            t.set_icon(v.favicon().as_ref());
        }
    });
    let sync = {
        let g = get.clone();
        move |_: &View| {
            if let Some((w, t)) = g()
                && w.is_current(&t)
            {
                w.sync_ui();
            }
        }
    };
    view.connect_uri(sync.clone());
    view.connect_loading(sync);
    let g = get.clone();
    view.connect_progress(move |_| {
        if let Some((w, t)) = g()
            && w.is_current(&t)
        {
            w.sync_progress();
        }
    });
    let g = get.clone();
    view.connect_load(move |v, event| {
        let Some((w, t)) = g() else { return };
        if event == LoadEvent::Committed && w.is_current(&t) {
            w.sync_ui(); // TLS state and back/forward are known now
        }
        if event == LoadEvent::Finished {
            w.queue_session_save();
        }
        if event == LoadEvent::Finished && !w.private {
            let uri = v.uri().unwrap_or_default();
            if (uri.starts_with("https://") || uri.starts_with("http://"))
                && let Some(store) = w.app.store()
            {
                store.visit(&uri, v.title().as_deref().unwrap_or(""));
            }
        }
    });
    // The engine often learns the title after load-finished; fill it in then.
    let g = get.clone();
    view.connect_title(move |v| {
        let Some((w, _)) = g() else { return };
        let (Some(uri), Some(title)) = (v.uri(), v.title()) else {
            return;
        };
        if !w.private
            && !title.is_empty()
            && (uri.starts_with("https://") || uri.starts_with("http://"))
            && let Some(store) = w.app.store()
        {
            store.title(&uri, &title);
        }
    });
    let g = get.clone();
    view.connect_load_failed(move |uri, kind, message| {
        let (w, _) = g()?;
        let accent = w.app.config.borrow().accent.clone();
        Some(error_page(uri, kind, message, &accent))
    });
    let g = get.clone();
    view.connect_create(move |v| {
        let (w, _) = g()?;
        Some(w.new_related_tab(v))
    });
    let g = get.clone();
    view.connect_close(move || {
        if let Some((w, t)) = g() {
            w.close_tab(&t);
        }
    });
    let g = get.clone();
    view.connect_navigation(move |action| g().is_some_and(|(w, _)| intercept(&w, action)));
    view.connect_response(download_instead);
    let g = get.clone();
    view.connect_permission(move |v, req| match g() {
        Some((w, _)) => w.ask_permission(v, req),
        None => req.deny(),
    });
    let g = get.clone();
    view.connect_hover_link(move |link| {
        let Some((w, t)) = g() else { return };
        if !w.is_current(&t) {
            return;
        }
        w.status.set_visible(link.is_some());
        if let Some(l) = link {
            w.status.set_text(&l);
        }
    });
    view.connect_find_result(move |n| {
        if let Some((w, t)) = get()
            && w.is_current(&t)
        {
            if n == 0 {
                w.find_label.add_css_class("error");
                w.find_label.set_text("No matches");
                return;
            }
            w.find_label.remove_css_class("error");
            w.find_label.set_text(&match n {
                1 => "1 match".to_string(),
                n if n >= 1000 => "1000+ matches".to_string(),
                n => format!("{n} matches"),
            });
        }
    });
    view
}

/// Navigations the browser handles itself. Returns true to cancel the page's.
fn intercept(w: &Rc<Window>, action: &NavigationAction) -> bool {
    let scheme = util::scheme(&action.uri)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !util::INTERNAL_SCHEMES.contains(&scheme.as_str()) {
        // Only a click may launch another app, never a script or
        // redirect. `mallow:` is browser UI: pages can't open it.
        if action.user_gesture && !action.redirect && scheme != "mallow" {
            w.ask_external(&action.uri);
        }
        return true;
    }
    if !action.new_window && action.link_click && action.background {
        w.new_tab(Some(&action.uri), false);
        return true;
    }
    false
}

/// Anything the engine cannot display (or the server marks as an
/// attachment) becomes a download instead of being rendered.
fn download_instead(r: &Response) -> bool {
    r.main_resource && (!r.displayable || r.attachment)
}

/// Friendly page for a failed load, themed with the browser's colour tokens.
fn error_page(uri: &str, kind: LoadError, message: &str, accent: &str) -> String {
    let host = gtk::glib::Uri::parse(uri, gtk::glib::UriFlags::NONE)
        .ok()
        .and_then(|u| u.host())
        .map_or_else(|| uri.to_string(), String::from);
    let (title, hint, code) = match kind {
        LoadError::NotFound => (
            "This site can't be found",
            format!("Check the spelling of {host}, or your connection."),
            "NAME_NOT_RESOLVED",
        ),
        LoadError::Tls => (
            "Connection not secure",
            format!("{host} sent a certificate that can't be trusted."),
            "CERT_INVALID",
        ),
        LoadError::Refused => (
            "Connection refused",
            format!("{host} refused to connect."),
            "CONNECTION_REFUSED",
        ),
        LoadError::TimedOut => (
            "Connection timed out",
            format!("{host} took too long to respond."),
            "TIMED_OUT",
        ),
        LoadError::Offline => (
            "You're offline",
            "Check your network connection.".into(),
            "ADDRESS_UNREACHABLE",
        ),
        LoadError::Other => (
            "This page couldn't load",
            format!("Something went wrong loading {host}."),
            "LOAD_FAILED",
        ),
    };
    let dark = gtk::Settings::default()
        .is_some_and(|s| s.gtk_interface_color_scheme() == gtk::InterfaceColorScheme::Dark);
    include_str!("../assets/error.html")
        .replace("{{tokens}}", &crate::theme::css(accent, dark))
        .replace("{{title}}", title)
        .replace("{{hint}}", &escape(&hint))
        .replace("{{message}}", &escape(message))
        .replace("{{code}}", &format!("{code} · {}", escape(uri)))
        .replace("{{uri}}", &escape(uri))
}

/// Error text and URLs come from the network; never let them become markup.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_markup() {
        assert_eq!(
            super::escape("<a href=\"x\">&"),
            "&lt;a href=&quot;x&quot;&gt;&amp;"
        );
    }
}
