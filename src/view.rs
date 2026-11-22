//! WebView creation and the policy hooks that keep web content contained.

use crate::{
    util,
    window::{Tab, Window},
};
use gtk::{gdk, prelude::*};
use std::rc::{Rc, Weak};
use webkit6::{
    LoadEvent, NavigationPolicyDecision, NavigationType, PolicyDecisionType, ResponsePolicyDecision, WebView,
    prelude::*,
};

/// Clone two weak refs into a closure that upgrades both or does nothing.
fn pair(w: &Rc<Window>, t: &Rc<Tab>) -> impl Fn() -> Option<(Rc<Window>, Rc<Tab>)> + Clone + 'static {
    let (w, t): (Weak<Window>, Weak<Tab>) = (Rc::downgrade(w), Rc::downgrade(t));
    move || Some((w.upgrade()?, t.upgrade()?))
}

pub fn create(win: &Rc<Window>, tab: &Rc<Tab>, related: Option<&WebView>) -> WebView {
    let autoplay = match win.app.config.borrow().autoplay {
        crate::config::Autoplay::Allow => webkit6::AutoplayPolicy::Allow,
        crate::config::Autoplay::NoSound => webkit6::AutoplayPolicy::AllowWithoutSound,
        crate::config::Autoplay::Deny => webkit6::AutoplayPolicy::Deny,
    };
    let policies = webkit6::WebsitePolicies::builder().autoplay(autoplay).build();
    let builder = WebView::builder()
        .website_policies(&policies)
        .settings(&win.app.settings)
        .user_content_manager(&win.app.content);
    let view = match related {
        Some(r) => builder.related_view(r).build(),
        None => builder.network_session(win.session()).build(),
    };
    crate::passwords::browser::attach(win, tab, &view);
    view.set_vexpand(true);
    view.set_hexpand(true);
    view.set_zoom_level(win.app.config.borrow().default_zoom);

    let get = pair(win, tab);
    let g = get.clone();
    view.connect_title_notify(move |v| {
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
    view.connect_favicon_notify(move |v| {
        if let Some((_, t)) = g()
            && t.view.borrow().as_ref() == Some(v)
        {
            t.set_icon(v.favicon().as_ref());
        }
    });
    let sync = {
        let g = get.clone();
        move |_: &WebView| {
            if let Some((w, t)) = g()
                && w.is_current(&t)
            {
                w.sync_ui();
            }
        }
    };
    view.connect_uri_notify(sync.clone());
    view.connect_is_loading_notify(sync);
    let g = get.clone();
    view.connect_estimated_load_progress_notify(move |_| {
        if let Some((w, t)) = g()
            && w.is_current(&t)
        {
            w.sync_progress();
        }
    });
    let g = get.clone();
    view.connect_load_changed(move |v, event| {
        let Some((w, t)) = g() else { return };
        if event == LoadEvent::Committed && w.is_current(&t) {
            w.sync_ui(); // TLS state and back/forward are known now
        }
        if event == LoadEvent::Finished && !w.private {
            let uri = v.uri().map(String::from).unwrap_or_default();
            if (uri.starts_with("https://") || uri.starts_with("http://"))
                && let Some(store) = w.app.store()
            {
                store.visit(&uri, v.title().as_deref().unwrap_or(""));
            }
        }
    });
    // WebKit often learns the title after load-finished; fill it in then.
    let g = get.clone();
    view.connect_title_notify(move |v| {
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
    view.connect_load_failed(move |v, _, uri, err| {
        // Cancelled loads (Stop, new navigation, downloads) keep WebKit's default: nothing.
        if err.matches(webkit6::NetworkError::Cancelled)
            || err.matches(webkit6::PolicyError::FrameLoadInterruptedByPolicyChange)
        {
            return false;
        }
        let Some((w, _)) = g() else { return false };
        let accent = w.app.config.borrow().accent.clone();
        v.load_alternate_html(&error_page(uri, err, &accent), uri, None);
        true
    });
    let g = get.clone();
    view.connect_create(move |v, _action| {
        let (w, _) = g()?;
        Some(w.new_related_tab(v).upcast())
    });
    let g = get.clone();
    view.connect_close(move |_| {
        if let Some((w, t)) = g() {
            w.close_tab(&t);
        }
    });
    let g = get.clone();
    view.connect_decide_policy(move |_, decision, kind| {
        let Some((w, _)) = g() else { return false };
        decide_policy(&w, decision, kind)
    });
    let g = get.clone();
    view.connect_permission_request(move |v, req| {
        match g() {
            Some((w, _)) => w.ask_permission(v, req),
            None => req.deny(),
        }
        true
    });
    let g = get.clone();
    view.connect_mouse_target_changed(move |_, hit, _| {
        let Some((w, t)) = g() else { return };
        if !w.is_current(&t) {
            return;
        }
        let link = hit.context_is_link().then(|| hit.link_uri()).flatten();
        w.status.set_visible(link.is_some());
        if let Some(l) = link {
            w.status.set_text(&l);
        }
    });
    view.connect_web_process_terminated(|v, reason| {
        eprintln!("mallow: web process for {:?} terminated: {reason:?}", v.uri());
    });

    if let Some(fc) = view.find_controller() {
        let g = get.clone();
        fc.connect_counted_matches(move |_, n| {
            if let Some((w, t)) = g()
                && w.is_current(&t)
            {
                w.find_label.remove_css_class("error");
                w.find_label.set_text(&match n {
                    1 => "1 match".to_string(),
                    n if n >= 1000 => "1000+ matches".to_string(),
                    n => format!("{n} matches"),
                });
            }
        });
        fc.connect_failed_to_find_text(move |_| {
            if let Some((w, t)) = get()
                && w.is_current(&t)
            {
                w.find_label.add_css_class("error");
                w.find_label.set_text("No matches");
            }
        });
    }
    view
}

/// Returns true when the decision was taken here.
fn decide_policy(w: &Rc<Window>, decision: &webkit6::PolicyDecision, kind: PolicyDecisionType) -> bool {
    match kind {
        PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
            let Some(action) = decision
                .downcast_ref::<NavigationPolicyDecision>()
                .and_then(|d| d.navigation_action())
            else {
                return false;
            };
            let Some(uri) = action.request().and_then(|r| r.uri()) else {
                return false;
            };
            let scheme = util::scheme(&uri)
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if !util::INTERNAL_SCHEMES.contains(&scheme.as_str()) {
                decision.ignore();
                // Only a click may launch another app, never a script or
                // redirect. `mallow:` is browser UI: pages can't open it.
                if action.is_user_gesture() && !action.is_redirect() && scheme != "mallow" {
                    w.ask_external(&uri);
                }
                return true;
            }
            let mods = gdk::ModifierType::from_bits_truncate(action.modifiers());
            let background =
                action.mouse_button() == gdk::BUTTON_MIDDLE || mods.contains(gdk::ModifierType::CONTROL_MASK);
            if kind == PolicyDecisionType::NavigationAction
                && action.navigation_type() == NavigationType::LinkClicked
                && background
            {
                decision.ignore();
                w.new_tab(Some(&uri), false);
                return true;
            }
            false
        }
        PolicyDecisionType::Response => {
            let Some(d) = decision.downcast_ref::<ResponsePolicyDecision>() else {
                return false;
            };
            // Anything WebKit cannot display (or the server marks as an
            // attachment) becomes a download instead of being rendered.
            let attachment = d
                .response()
                .and_then(|r| r.http_headers())
                .and_then(|h| h.one("Content-Disposition"))
                .is_some_and(|v| v.trim_start().to_ascii_lowercase().starts_with("attachment"));
            if d.is_main_frame_main_resource() && (!d.is_mime_type_supported() || attachment) {
                decision.download();
                return true;
            }
            false
        }
        _ => false,
    }
}

/// Friendly page for a failed load, themed with the browser's colour tokens.
fn error_page(uri: &str, err: &gtk::glib::Error, accent: &str) -> String {
    use gtk::gio::{IOErrorEnum as Io, ResolverError, TlsError};
    let host = gtk::glib::Uri::parse(uri, gtk::glib::UriFlags::NONE)
        .ok()
        .and_then(|u| u.host())
        .map_or_else(|| uri.to_string(), String::from);
    let (title, hint, code) = if err.kind::<ResolverError>().is_some() {
        (
            "This site can't be found",
            format!("Check the spelling of {host}, or your connection."),
            "NAME_NOT_RESOLVED",
        )
    } else if err.kind::<TlsError>().is_some() {
        (
            "Connection not secure",
            format!("{host} sent a certificate that can't be trusted."),
            "CERT_INVALID",
        )
    } else {
        match err.kind::<Io>() {
            Some(Io::ConnectionRefused) => (
                "Connection refused",
                format!("{host} refused to connect."),
                "CONNECTION_REFUSED",
            ),
            Some(Io::TimedOut) => (
                "Connection timed out",
                format!("{host} took too long to respond."),
                "TIMED_OUT",
            ),
            Some(Io::NetworkUnreachable | Io::HostUnreachable) => (
                "You're offline",
                "Check your network connection.".into(),
                "ADDRESS_UNREACHABLE",
            ),
            _ => (
                "This page couldn't load",
                format!("Something went wrong loading {host}."),
                "LOAD_FAILED",
            ),
        }
    };
    let dark = gtk::Settings::default()
        .is_some_and(|s| s.gtk_interface_color_scheme() == gtk::InterfaceColorScheme::Dark);
    include_str!("../assets/error.html")
        .replace("{{tokens}}", &crate::theme::css(accent, dark))
        .replace("{{title}}", title)
        .replace("{{hint}}", &escape(&hint))
        .replace("{{message}}", &escape(err.message()))
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
