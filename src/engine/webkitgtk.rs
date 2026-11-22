//! Linux backend: WebKitGTK 6.0. The only file that talks to `webkit6`.

use super::{
    Certificate, DataKinds, DownloadEnd, LoadError, LoadEvent, NavigationAction, Permission, Response,
};
use crate::config::{Autoplay, Config, Cookies};
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};
use webkit6::{
    NavigationPolicyDecision, NavigationType, PolicyDecisionType, ResponsePolicyDecision, WebView, prelude::*,
};

/// Engine name shown in About.
pub const NAME: &str = "WebKitGTK";

pub fn version() -> String {
    format!(
        "{}.{}.{}",
        webkit6::functions::major_version(),
        webkit6::functions::minor_version(),
        webkit6::functions::micro_version()
    )
}

/// `scheme://host[:port]` of a URI, as the engine defines a site's origin.
pub fn origin(uri: &str) -> String {
    webkit6::SecurityOrigin::for_uri(uri).to_str().to_string()
}

/// A connected signal, for `disconnect`.
pub struct Handler(glib::SignalHandlerId);

// ------------------------------------------------------------------ engine

/// Process-wide engine state shared by every view: preferences and the
/// compiled tracker filter.
pub struct Engine {
    settings: webkit6::Settings,
    content: webkit6::UserContentManager,
}

impl Engine {
    pub fn new() -> Self {
        let settings = webkit6::Settings::new();
        settings.set_enable_developer_extras(true);
        Self {
            settings,
            content: webkit6::UserContentManager::new(),
        }
    }

    /// Engine-wide preferences from the config.
    pub fn apply(&self, cfg: &Config) {
        self.settings
            .set_hardware_acceleration_policy(if cfg.hardware_acceleration {
                webkit6::HardwareAccelerationPolicy::Always
            } else {
                webkit6::HardwareAccelerationPolicy::Never
            });
        self.settings.set_enable_smooth_scrolling(cfg.smooth_scrolling);
        self.settings.set_enable_webgl(cfg.webgl);
        self.settings.set_enable_page_cache(cfg.page_cache);
        if let Some(ctx) = webkit6::WebContext::default() {
            ctx.set_cache_model(if cfg.low_memory {
                webkit6::CacheModel::DocumentViewer
            } else {
                webkit6::CacheModel::WebBrowser
            });
            let langs = crate::config::parse_languages(&cfg.languages).unwrap_or_default();
            if !langs.is_empty() {
                ctx.set_preferred_languages(&langs.iter().map(String::as_str).collect::<Vec<_>>());
            }
            ctx.set_spell_checking_enabled(cfg.spellcheck);
            let dicts = crate::config::parse_dictionaries(&cfg.spell_languages).unwrap_or_default();
            if cfg.spellcheck && !dicts.is_empty() {
                ctx.set_spell_checking_languages(&dicts.iter().map(String::as_str).collect::<Vec<_>>());
            }
        }
    }

    /// Compile WebKit content-blocker JSON rules (cached in `cache_dir`) and
    /// apply them to every view. Asynchronous: pages loaded meanwhile aren't filtered.
    pub fn set_block_rules(&self, rules: String, cache_dir: &Path) {
        let store = webkit6::UserContentFilterStore::new(&cache_dir.to_string_lossy());
        let content = self.content.clone();
        store.save(
            "mallow-trackers",
            &glib::Bytes::from_owned(rules.into_bytes()),
            None::<&gio::Cancellable>,
            move |r| match r {
                Ok(filter) => content.add_filter(&filter),
                Err(e) => eprintln!("mallow: content filter failed to compile: {e}"),
            },
        );
    }

    pub fn clear_block_rules(&self) {
        self.content.remove_all_filters();
    }
}

// ----------------------------------------------------------------- session

/// Cookies, cache and site storage: one persistent session for normal
/// windows, an in-memory one per private window.
#[derive(Clone)]
pub struct Session(webkit6::NetworkSession);

impl Session {
    pub fn persistent(data: &Path, cache: &Path) -> Self {
        let s = webkit6::NetworkSession::new(Some(&data.to_string_lossy()), Some(&cache.to_string_lossy()));
        if let Some(cm) = s.cookie_manager() {
            cm.set_persistent_storage(
                &data.join("cookies.sqlite").to_string_lossy(),
                webkit6::CookiePersistentStorage::Sqlite,
            );
        }
        Self::init(s)
    }

    pub fn ephemeral() -> Self {
        Self::init(webkit6::NetworkSession::new_ephemeral())
    }

    fn init(s: webkit6::NetworkSession) -> Self {
        if let Some(dm) = s.website_data_manager() {
            dm.set_favicons_enabled(true);
        }
        Self(s)
    }

    /// Cookie policy and tracking prevention.
    pub fn apply(&self, cfg: &Config) {
        self.0.set_itp_enabled(cfg.itp);
        if let Some(cm) = self.0.cookie_manager() {
            cm.set_accept_policy(match cfg.cookies {
                Cookies::NoThirdParty => webkit6::CookieAcceptPolicy::NoThirdParty,
                Cookies::Always => webkit6::CookieAcceptPolicy::Always,
                Cookies::Never => webkit6::CookieAcceptPolicy::Never,
            });
        }
    }

    pub fn clear(&self, kinds: DataKinds) {
        let mut types = webkit6::WebsiteDataTypes::empty();
        if kinds.cache {
            types |= webkit6::WebsiteDataTypes::MEMORY_CACHE | webkit6::WebsiteDataTypes::DISK_CACHE;
        }
        if kinds.site_data {
            types |= webkit6::WebsiteDataTypes::all()
                - webkit6::WebsiteDataTypes::MEMORY_CACHE
                - webkit6::WebsiteDataTypes::DISK_CACHE;
        }
        let Some(dm) = self.0.website_data_manager() else {
            return;
        };
        if !types.is_empty() {
            dm.clear(
                types,
                glib::TimeSpan::from_seconds(0),
                None::<&gio::Cancellable>,
                |r| {
                    if let Err(e) = r {
                        eprintln!("mallow: clearing site data failed: {e}");
                    }
                },
            );
        }
        if kinds == DataKinds::ALL
            && let Some(fav) = dm.favicon_database()
        {
            fav.clear();
        }
    }

    /// The stored favicon of a page, if the engine has one.
    pub async fn favicon(&self, page: &str) -> Option<gdk::Texture> {
        let db = self.0.website_data_manager()?.favicon_database()?;
        db.favicon_future(page).await.ok()
    }

    pub fn connect_download(&self, f: impl Fn(Download) + 'static) {
        self.0
            .connect_download_started(move |_, dl| f(Download(dl.clone())));
    }
}

// -------------------------------------------------------------------- view

/// One page: a GTK widget showing web content plus its navigation state.
#[derive(Clone, PartialEq)]
pub struct View(WebView);

pub struct WeakView(glib::WeakRef<WebView>);

impl WeakView {
    pub fn upgrade(&self) -> Option<View> {
        self.0.upgrade().map(View)
    }
}

/// Back/forward history of a view, kept while the view is unloaded.
pub struct SessionState(webkit6::WebViewSessionState);

impl View {
    /// A new view in `session`, or sharing `related`'s process and session
    /// (for pages the opener can script, like `window.open`).
    pub fn new(engine: &Engine, session: &Session, related: Option<&View>, autoplay: Autoplay) -> Self {
        let autoplay = match autoplay {
            Autoplay::Allow => webkit6::AutoplayPolicy::Allow,
            Autoplay::NoSound => webkit6::AutoplayPolicy::AllowWithoutSound,
            Autoplay::Deny => webkit6::AutoplayPolicy::Deny,
        };
        let policies = webkit6::WebsitePolicies::builder().autoplay(autoplay).build();
        let builder = WebView::builder()
            .website_policies(&policies)
            .settings(&engine.settings)
            .user_content_manager(&engine.content);
        let view = match related {
            Some(r) => builder.related_view(&r.0).build(),
            None => builder.network_session(&session.0).build(),
        };
        view.connect_web_process_terminated(|v, reason| {
            eprintln!("mallow: web process for {:?} terminated: {reason:?}", v.uri());
        });
        Self(view)
    }

    /// A view on its own throwaway session, for tests.
    #[cfg(test)]
    pub fn standalone() -> Self {
        Self(
            WebView::builder()
                .network_session(&webkit6::NetworkSession::new_ephemeral())
                .build(),
        )
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.clone().upcast()
    }

    pub fn downgrade(&self) -> WeakView {
        WeakView(self.0.downgrade())
    }

    pub fn grab_focus(&self) {
        self.0.grab_focus();
    }

    // --- state
    pub fn uri(&self) -> Option<String> {
        self.0.uri().map(String::from)
    }
    pub fn title(&self) -> Option<String> {
        self.0.title().map(String::from)
    }
    pub fn favicon(&self) -> Option<gdk::Texture> {
        self.0.favicon()
    }
    pub fn is_loading(&self) -> bool {
        self.0.is_loading()
    }
    pub fn progress(&self) -> f64 {
        self.0.estimated_load_progress()
    }
    pub fn can_go_back(&self) -> bool {
        self.0.can_go_back()
    }
    pub fn can_go_forward(&self) -> bool {
        self.0.can_go_forward()
    }
    /// Whether the view has committed any page (a tab opened only for a
    /// download has none).
    pub fn has_history(&self) -> bool {
        self.0
            .back_forward_list()
            .and_then(|l| l.current_item())
            .is_some()
    }
    pub fn is_playing_audio(&self) -> bool {
        self.0.is_playing_audio()
    }
    pub fn zoom(&self) -> f64 {
        self.0.zoom_level()
    }
    pub fn certificate(&self) -> Option<Certificate> {
        let (cert, flags) = self.0.tls_info()?;
        Some(Certificate {
            problems: (!flags.is_empty()).then(|| format!("{flags:?}")),
            subject: cert.subject_name().map(String::from).unwrap_or_default(),
            issuer: cert.issuer_name().map(String::from).unwrap_or_default(),
            not_before: cert.not_valid_before(),
            not_after: cert.not_valid_after(),
            der: cert.certificate().map(|b| b.to_vec()),
        })
    }

    // --- actions
    pub fn load(&self, uri: &str) {
        self.0.load_uri(uri);
    }
    #[cfg(test)]
    pub fn load_html(&self, html: &str, base: Option<&str>) {
        self.0.load_html(html, base);
    }
    pub fn reload(&self) {
        self.0.reload();
    }
    pub fn stop(&self) {
        self.0.stop_loading();
    }
    pub fn go_back(&self) {
        self.0.go_back();
    }
    pub fn go_forward(&self) {
        self.0.go_forward();
    }
    pub fn set_zoom(&self, level: f64) {
        self.0.set_zoom_level(level);
    }
    pub fn set_muted(&self, muted: bool) {
        self.0.set_is_muted(muted);
    }
    /// End the view's content process so its memory is returned.
    pub fn terminate(&self) {
        self.0.terminate_web_process();
    }
    pub fn session_state(&self) -> Option<SessionState> {
        self.0.session_state().map(SessionState)
    }
    /// Restore history saved with `session_state` and load its current page.
    pub fn restore(&self, state: &SessionState) {
        self.0.restore_session_state(&state.0);
        if let Some(item) = self.0.back_forward_list().and_then(|l| l.current_item()) {
            self.0.go_to_back_forward_list_item(&item);
        }
    }

    // --- find in page
    pub fn find(&self, text: &str) {
        let Some(fc) = self.0.find_controller() else {
            return;
        };
        if text.is_empty() {
            fc.search_finish();
            return;
        }
        let opts = (webkit6::FindOptions::CASE_INSENSITIVE | webkit6::FindOptions::WRAP_AROUND).bits();
        fc.count_matches(text, opts, 1000);
        fc.search(text, opts, 1000);
    }
    /// The text last searched for, if a search is active.
    pub fn find_text(&self) -> Option<String> {
        self.0.find_controller()?.search_text().map(String::from)
    }
    pub fn find_next(&self, backwards: bool) {
        if let Some(fc) = self.0.find_controller() {
            if backwards {
                fc.search_previous()
            } else {
                fc.search_next()
            }
        }
    }
    pub fn find_finish(&self) {
        if let Some(fc) = self.0.find_controller() {
            fc.search_finish();
        }
    }

    // --- scripts in a named, page-invisible world
    /// Call `body` as an async function with `args` (a JSON object whose
    /// members become parameters). `world: None` runs in the page itself.
    pub async fn call_script(
        &self,
        body: &str,
        args: &serde_json::Value,
        world: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let dict = glib::VariantDict::new(None);
        for (k, v) in args.as_object().into_iter().flatten() {
            match v {
                serde_json::Value::String(s) => dict.insert(k, s.as_str()),
                serde_json::Value::Bool(b) => dict.insert(k, *b),
                serde_json::Value::Number(n) => match n.as_i64().and_then(|n| i32::try_from(n).ok()) {
                    Some(i) => dict.insert(k, i),
                    None => dict.insert(k, n.as_f64().unwrap_or_default()),
                },
                _ => return Err(format!("unsupported script argument {k}")),
            }
        }
        let value = self
            .0
            .call_async_javascript_function_future(body, Some(&dict.end()), world, None)
            .await
            .map_err(|e| e.to_string())?;
        Ok(value
            .to_json(0)
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or(serde_json::Value::Null))
    }

    /// `call_script` without waiting for, or caring about, the result.
    pub fn run_script(&self, body: &'static str, args: serde_json::Value, world: Option<&'static str>) {
        let v = self.clone();
        glib::spawn_future_local(async move {
            let _ = v.call_script(body, &args, world).await;
        });
    }

    /// Receive string messages the world posts to
    /// `window.webkit.messageHandlers[channel]`. `None` if the name is taken.
    pub fn add_script_handler(
        &self,
        world: &str,
        channel: &str,
        f: impl Fn(&str) + 'static,
    ) -> Option<Handler> {
        let manager = self.0.user_content_manager()?;
        if !manager.register_script_message_handler(channel, Some(world)) {
            return None;
        }
        Some(Handler(manager.connect_script_message_received(
            Some(channel),
            move |_, value| {
                if value.is_string() {
                    f(&value.to_str());
                }
            },
        )))
    }

    pub fn remove_script_handler(&self, world: &str, channel: &str, handler: Handler) {
        if let Some(manager) = self.0.user_content_manager() {
            manager.disconnect(handler.0);
            manager.unregister_script_message_handler(channel, Some(world));
        }
    }

    // --- events
    pub fn disconnect(&self, h: Handler) {
        self.0.disconnect(h.0);
    }
    pub fn connect_title(&self, f: impl Fn(&View) + 'static) -> Handler {
        Handler(self.0.connect_title_notify(move |v| f(&View(v.clone()))))
    }
    pub fn connect_favicon(&self, f: impl Fn(&View) + 'static) -> Handler {
        Handler(self.0.connect_favicon_notify(move |v| f(&View(v.clone()))))
    }
    pub fn connect_uri(&self, f: impl Fn(&View) + 'static) -> Handler {
        Handler(self.0.connect_uri_notify(move |v| f(&View(v.clone()))))
    }
    pub fn connect_loading(&self, f: impl Fn(&View) + 'static) -> Handler {
        Handler(self.0.connect_is_loading_notify(move |v| f(&View(v.clone()))))
    }
    pub fn connect_progress(&self, f: impl Fn(&View) + 'static) -> Handler {
        Handler(
            self.0
                .connect_estimated_load_progress_notify(move |v| f(&View(v.clone()))),
        )
    }
    pub fn connect_load(&self, f: impl Fn(&View, LoadEvent) + 'static) -> Handler {
        Handler(self.0.connect_load_changed(move |v, e| {
            let event = match e {
                webkit6::LoadEvent::Started => LoadEvent::Started,
                webkit6::LoadEvent::Redirected => LoadEvent::Redirected,
                webkit6::LoadEvent::Committed => LoadEvent::Committed,
                _ => LoadEvent::Finished,
            };
            f(&View(v.clone()), event)
        }))
    }
    /// `f` may return an HTML error page to show in place of the failed URI.
    /// Cancelled loads (Stop, a new navigation, a download) never reach it.
    pub fn connect_load_failed(&self, f: impl Fn(&str, LoadError, &str) -> Option<String> + 'static) {
        self.0.connect_load_failed(move |v, _, uri, err| {
            if err.matches(webkit6::NetworkError::Cancelled)
                || err.matches(webkit6::PolicyError::FrameLoadInterruptedByPolicyChange)
            {
                return false;
            }
            use gio::{IOErrorEnum as Io, ResolverError, TlsError};
            let kind = if err.kind::<ResolverError>().is_some() {
                LoadError::NotFound
            } else if err.kind::<TlsError>().is_some() {
                LoadError::Tls
            } else {
                match err.kind::<Io>() {
                    Some(Io::ConnectionRefused) => LoadError::Refused,
                    Some(Io::TimedOut) => LoadError::TimedOut,
                    Some(Io::NetworkUnreachable | Io::HostUnreachable) => LoadError::Offline,
                    _ => LoadError::Other,
                }
            };
            match f(uri, kind, err.message()) {
                Some(html) => {
                    v.load_alternate_html(&html, uri, None);
                    true
                }
                None => false,
            }
        });
    }
    /// The page opens a new window; return the view to load it in.
    pub fn connect_create(&self, f: impl Fn(&View) -> Option<View> + 'static) {
        self.0
            .connect_create(move |v, _| f(&View(v.clone())).map(|n| n.widget()));
    }
    /// The page called `window.close()`.
    pub fn connect_close(&self, f: impl Fn() + 'static) {
        self.0.connect_close(move |_| f());
    }
    /// Return true to cancel the navigation.
    pub fn connect_navigation(&self, f: impl Fn(&NavigationAction) -> bool + 'static) {
        self.0.connect_decide_policy(move |_, decision, kind| match kind {
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
                let mods = gdk::ModifierType::from_bits_truncate(action.modifiers());
                let a = NavigationAction {
                    uri: uri.to_string(),
                    user_gesture: action.is_user_gesture(),
                    redirect: action.is_redirect(),
                    new_window: kind == PolicyDecisionType::NewWindowAction,
                    link_click: action.navigation_type() == NavigationType::LinkClicked,
                    background: action.mouse_button() == gdk::BUTTON_MIDDLE
                        || mods.contains(gdk::ModifierType::CONTROL_MASK),
                };
                if f(&a) {
                    decision.ignore();
                    return true;
                }
                false
            }
            _ => false,
        });
    }
    /// Return true to save the response as a download instead of showing it.
    pub fn connect_response(&self, f: impl Fn(&Response) -> bool + 'static) {
        self.0.connect_decide_policy(move |_, decision, kind| {
            if kind != PolicyDecisionType::Response {
                return false;
            }
            let Some(d) = decision.downcast_ref::<ResponsePolicyDecision>() else {
                return false;
            };
            let r = Response {
                main_resource: d.is_main_frame_main_resource(),
                displayable: d.is_mime_type_supported(),
                attachment: d
                    .response()
                    .and_then(|r| r.http_headers())
                    .and_then(|h| h.one("Content-Disposition"))
                    .is_some_and(|v| v.trim_start().to_ascii_lowercase().starts_with("attachment")),
            };
            if f(&r) {
                decision.download();
                return true;
            }
            false
        });
    }
    pub fn connect_permission(&self, f: impl Fn(&View, PermissionRequest) + 'static) {
        self.0.connect_permission_request(move |v, req| {
            f(&View(v.clone()), PermissionRequest::new(req));
            true
        });
    }
    /// The link under the pointer changed (`None`: not over a link).
    pub fn connect_hover_link(&self, f: impl Fn(Option<String>) + 'static) {
        self.0.connect_mouse_target_changed(move |_, hit, _| {
            f(hit
                .context_is_link()
                .then(|| hit.link_uri())
                .flatten()
                .map(String::from))
        });
    }
    /// Find in page: match count (capped at 1000), or 0 when nothing matched.
    pub fn connect_find_result(&self, f: impl Fn(u32) + 'static) {
        let Some(fc) = self.0.find_controller() else {
            return;
        };
        let f = Rc::new(f);
        let g = f.clone();
        fc.connect_counted_matches(move |_, n| g(n));
        fc.connect_failed_to_find_text(move |_| f(0));
    }
    /// Run `f` once when the view is destroyed.
    pub fn on_destroy(&self, f: impl FnOnce() + 'static) {
        let f = Cell::new(Some(f));
        self.0.add_weak_ref_notify_local(move || {
            if let Some(f) = f.take() {
                f()
            }
        });
    }
}

// -------------------------------------------------------------- permission

/// A pending permission prompt; answer it with `allow` or `deny`.
#[derive(Clone)]
pub struct PermissionRequest {
    req: webkit6::PermissionRequest,
    pub kind: Permission,
}

impl PermissionRequest {
    fn new(req: &webkit6::PermissionRequest) -> Self {
        let kind = if let Some(m) = req.downcast_ref::<webkit6::UserMediaPermissionRequest>() {
            Permission::Media {
                video: m.is_for_video_device(),
                audio: m.is_for_audio_device(),
            }
        } else if req.is::<webkit6::GeolocationPermissionRequest>() {
            Permission::Location
        } else if req.is::<webkit6::NotificationPermissionRequest>() {
            Permission::Notifications
        } else if req.is::<webkit6::ClipboardPermissionRequest>() {
            Permission::Clipboard
        } else if req.is::<webkit6::PointerLockPermissionRequest>() {
            Permission::PointerLock
        } else if req.is::<webkit6::DeviceInfoPermissionRequest>() {
            Permission::DeviceInfo
        } else if req.is::<webkit6::MediaKeySystemPermissionRequest>() {
            Permission::MediaKeys
        } else if let Some(a) = req.downcast_ref::<webkit6::WebsiteDataAccessPermissionRequest>() {
            match (a.current_domain(), a.requesting_domain()) {
                (Some(c), Some(r)) => Permission::StorageAccess {
                    current: c.to_string(),
                    requester: r.to_string(),
                },
                _ => Permission::Other,
            }
        } else {
            Permission::Other
        };
        Self {
            req: req.clone(),
            kind,
        }
    }
    pub fn allow(&self) {
        self.req.allow();
    }
    pub fn deny(&self) {
        self.req.deny();
    }
}

// ---------------------------------------------------------------- download

#[derive(Clone)]
pub struct Download(webkit6::Download);

impl Download {
    pub fn source(&self) -> Option<String> {
        self.0.request().and_then(|r| r.uri()).map(String::from)
    }
    /// The view that started it, if any.
    pub fn view(&self) -> Option<View> {
        self.0.web_view().map(View)
    }
    pub fn destination(&self) -> Option<PathBuf> {
        self.0.destination().map(|d| PathBuf::from(d.as_str()))
    }
    pub fn received_bytes(&self) -> u64 {
        self.0.received_data_length()
    }
    pub fn cancel(&self) {
        self.0.cancel();
    }
    /// Choose where to save, given the server's suggested file name. The
    /// file is never overwritten.
    pub fn connect_destination(&self, f: impl Fn(&str) -> PathBuf + 'static) {
        self.0.connect_decide_destination(move |dl, suggested| {
            dl.set_allow_overwrite(false);
            dl.set_destination(&f(suggested).to_string_lossy());
            true
        });
    }
    /// The file was created; its path may differ from the chosen one.
    pub fn connect_created(&self, f: impl Fn(&Path) + 'static) {
        self.0
            .connect_created_destination(move |_, dest| f(Path::new(dest)));
    }
    /// Progress as (fraction, bytes received).
    pub fn connect_progress(&self, f: impl Fn(f64, u64) + 'static) {
        self.0
            .connect_received_data(move |dl, _| f(dl.estimated_progress(), dl.received_data_length()));
    }
    pub fn connect_end(&self, f: impl Fn(&Download, DownloadEnd) + 'static) {
        // WebKit emits `failed` and then `finished`; report once, with the failure.
        let failure: Rc<RefCell<Option<DownloadEnd>>> = Rc::default();
        let fail = failure.clone();
        self.0.connect_failed(move |_, err| {
            fail.replace(Some(if err.matches(webkit6::DownloadError::CancelledByUser) {
                DownloadEnd::Cancelled
            } else {
                DownloadEnd::Failed(err.message().to_string())
            }));
        });
        self.0.connect_finished(move |dl| {
            f(&Download(dl.clone()), failure.take().unwrap_or(DownloadEnd::Done))
        });
    }
}
