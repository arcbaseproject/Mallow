//! Windows backend: Microsoft Edge WebView2, embedded in GTK's HWND.
//!
//! Each `View` is a GTK placeholder widget. WebView2 creates its controller
//! asynchronously, so a view starts in a hidden parking window and queues
//! calls until it is ready; while the placeholder is mapped, the controller
//! is parented to the GTK window and kept over the placeholder's allocation
//! every frame. GTK widgets drawn inside the same window (status bar,
//! overlays) appear underneath it; popovers and dialogs are separate windows.
//!
//! Differences from the other backends:
//! - WebView2 has no isolated script worlds, so scripts that ask for one fail
//!   and `add_script_handler` returns `None`: the password manager's form
//!   bridge is not offered on Windows.
//! - No content-blocker API: tracker blocking turns on WebView2's Strict
//!   tracking prevention instead of applying Mallow's list.
//! - No cookie policy API: the cookie setting is not applied.
//! - No serializable history: a suspended tab restores only its address.
//! - Not wired yet: favicons, link-hover URLs, response policy (WebView2
//!   downloads undisplayable responses itself).
//! - Untrusted certificates never load: the certificate-error event cancels.

use super::{
    Certificate, DataKinds, DownloadEnd, LoadError, LoadEvent, NavigationAction, Permission, Response,
};
use crate::config::{Autoplay, Config};
use gtk::{gdk, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};
use webview2_com::{
    BytesReceivedChangedEventHandler, CallDevToolsProtocolMethodCompletedHandler,
    ClearBrowsingDataCompletedHandler, ContentLoadingEventHandler, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DocumentTitleChangedEventHandler, DownloadStartingEventHandler, FindMatchCountChangedEventHandler,
    FindStartCompletedHandler, Microsoft::Web::WebView2::Win32::*, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler, PermissionRequestedEventHandler,
    ProcessFailedEventHandler, ServerCertificateErrorDetectedEventHandler, SourceChangedEventHandler,
    StateChangedEventHandler, WindowCloseRequestedEventHandler, take_pwstr,
};
use windows::{
    Win32::{
        Foundation::{HWND, RECT},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
        UI::WindowsAndMessaging::{CreateWindowExW, WINDOW_EX_STYLE, WS_POPUP},
    },
    core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR, w},
};

pub const NAME: &str = "Microsoft Edge WebView2";

pub fn version() -> String {
    let v = get_str(|p| unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), p) });
    match v {
        Some(v) if !v.is_empty() => format!("WebView2 {v}"),
        _ => "WebView2 (runtime not found)".into(),
    }
}

pub fn origin(uri: &str) -> String {
    match glib::Uri::parse(uri, glib::UriFlags::NONE) {
        Ok(u) => {
            let scheme = u.scheme();
            let host = u.host().unwrap_or_default();
            let default = matches!(
                (scheme.as_str(), u.port()),
                ("https", 443) | ("http", 80) | (_, -1)
            );
            if default {
                format!("{scheme}://{host}")
            } else {
                format!("{scheme}://{host}:{}", u.port())
            }
        }
        Err(_) => "null".into(),
    }
}

fn hs(s: &str) -> HSTRING {
    HSTRING::from(s)
}

/// Reads a COM string out-parameter.
fn get_str(f: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> Option<String> {
    let mut p = PWSTR::null();
    f(&mut p).ok()?;
    Some(take_pwstr(p))
}

/// Reads a COM BOOL out-parameter; false on error.
fn get_bool(f: impl FnOnce(*mut BOOL) -> windows::core::Result<()>) -> bool {
    let mut b = BOOL::default();
    f(&mut b).is_ok() && b.as_bool()
}

/// A connected callback, for `disconnect`.
pub struct Handler(u64);

/// Callbacks for one event, called in connection order.
struct Slot<F: ?Sized>(RefCell<Vec<(u64, Rc<F>)>>);

impl<F: ?Sized> Default for Slot<F> {
    fn default() -> Self {
        Self(RefCell::default())
    }
}

impl<F: ?Sized> Slot<F> {
    fn add(&self, id: u64, f: Rc<F>) {
        self.0.borrow_mut().push((id, f));
    }
    fn remove(&self, id: u64) {
        self.0.borrow_mut().retain(|(i, _)| *i != id);
    }
    /// A snapshot, so callbacks may connect or disconnect while running.
    fn all(&self) -> Vec<Rc<F>> {
        self.0.borrow().iter().map(|(_, f)| f.clone()).collect()
    }
}

type EnvCallback = Box<dyn FnOnce(&ICoreWebView2Environment)>;

enum Env {
    Idle,
    Pending(Vec<EnvCallback>),
    Ready(ICoreWebView2Environment),
    Failed,
}

thread_local! {
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static ENV: RefCell<Env> = const { RefCell::new(Env::Idle) };
    static AUTOPLAY: Cell<Autoplay> = const { Cell::new(Autoplay::NoSound) };
    static TRACKING: Cell<COREWEBVIEW2_TRACKING_PREVENTION_LEVEL> =
        const { Cell::new(COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BALANCED) };
    static SESSIONS: RefCell<Vec<Weak<SessionInner>>> = const { RefCell::new(Vec::new()) };
    static PARKING: Cell<usize> = const { Cell::new(0) };
}

fn next_id() -> u64 {
    NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    })
}

fn env() -> Option<ICoreWebView2Environment> {
    ENV.with(|e| match &*e.borrow() {
        Env::Ready(env) => Some(env.clone()),
        _ => None,
    })
}

/// Runs `f` once the shared WebView2 environment exists; never if the
/// runtime is missing.
fn with_env(f: impl FnOnce(&ICoreWebView2Environment) + 'static) {
    if let Some(env) = env() {
        return f(&env);
    }
    let start = ENV.with(|e| {
        let mut e = e.borrow_mut();
        if let Env::Pending(q) = &mut *e {
            q.push(Box::new(f));
            false
        } else if matches!(*e, Env::Idle) {
            *e = Env::Pending(vec![Box::new(f)]);
            true
        } else {
            false
        }
    });
    if start {
        create_env();
    }
}

fn create_env() {
    // WebView2 needs a single-threaded apartment; GTK may have set one up already.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let options = CoreWebView2EnvironmentOptions::default();
    // Autoplay is a browser-wide switch, read once at startup.
    if AUTOPLAY.get() == Autoplay::Deny {
        unsafe { options.set_additional_browser_arguments("--autoplay-policy=user-gesture-required".into()) };
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let folder = hs(&crate::config::data_dir().join("webview2").to_string_lossy());
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|hr, created| {
        let next = match (hr, created) {
            (Ok(()), Some(created)) => Env::Ready(created),
            (r, _) => {
                eprintln!("mallow: WebView2 runtime unavailable: {r:?}");
                Env::Failed
            }
        };
        let queued = ENV.with(|e| match std::mem::replace(&mut *e.borrow_mut(), next) {
            Env::Pending(q) => q,
            _ => Vec::new(),
        });
        if let Some(env) = env() {
            for f in queued {
                f(&env);
            }
        }
        Ok(())
    }));
    if let Err(e) =
        unsafe { CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &folder, &options, &handler) }
    {
        eprintln!("mallow: WebView2 runtime unavailable: {e}");
        ENV.with(|e| *e.borrow_mut() = Env::Failed);
    }
}

/// A hidden window that holds views before their placeholder is realized.
fn parking() -> HWND {
    let p = PARKING.get();
    if p != 0 {
        return HWND(p as *mut c_void);
    }
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("mallow"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        )
    }
    .unwrap_or_default();
    PARKING.set(hwnd.0 as usize);
    hwnd
}

// ------------------------------------------------------------------ engine

pub struct Engine;

impl Engine {
    pub fn new() -> Self {
        Engine
    }

    pub fn apply(&self, cfg: &Config) {
        AUTOPLAY.set(cfg.autoplay);
    }

    /// No content-blocker API: block trackers with Strict tracking prevention.
    pub fn set_block_rules(&self, _rules: String, _cache_dir: &Path) {
        set_tracking(COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT);
    }

    pub fn clear_block_rules(&self) {
        set_tracking(COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BALANCED);
    }
}

fn set_tracking(level: COREWEBVIEW2_TRACKING_PREVENTION_LEVEL) {
    TRACKING.set(level);
    SESSIONS.with(|all| {
        all.borrow_mut().retain(|s| s.strong_count() > 0);
        for s in all.borrow().iter().filter_map(Weak::upgrade) {
            if let Some(p) = s.profile.borrow().as_ref() {
                apply_tracking(p);
            }
        }
    });
}

fn apply_tracking(profile: &ICoreWebView2Profile) {
    if let Ok(p) = profile.cast::<ICoreWebView2Profile3>() {
        let _ = unsafe { p.SetPreferredTrackingPreventionLevel(TRACKING.get()) };
    }
}

// ----------------------------------------------------------------- session

#[derive(Clone)]
pub struct Session(Rc<SessionInner>);

struct SessionInner {
    name: String,
    private: bool,
    /// Known once the first view of this session is ready.
    profile: RefCell<Option<ICoreWebView2Profile>>,
    pending_clear: Cell<DataKinds>,
    downloads: Slot<dyn Fn(Download)>,
}

impl Session {
    /// WebView2 keeps all profiles under one user data folder, set when the
    /// environment is created.
    pub fn persistent(_data: &Path, _cache: &Path) -> Self {
        Self::with("default".into(), false)
    }

    pub fn ephemeral() -> Self {
        Self::with(format!("private-{}", next_id()), true)
    }

    fn with(name: String, private: bool) -> Self {
        let inner = Rc::new(SessionInner {
            name,
            private,
            profile: RefCell::default(),
            pending_clear: Cell::new(DataKinds::NONE),
            downloads: Slot::default(),
        });
        SESSIONS.with(|all| all.borrow_mut().push(Rc::downgrade(&inner)));
        Self(inner)
    }

    fn register(&self, profile: ICoreWebView2Profile) {
        if self.0.profile.borrow().is_some() {
            return;
        }
        apply_tracking(&profile);
        self.0.profile.replace(Some(profile));
        let pending = self.0.pending_clear.replace(DataKinds::NONE);
        self.clear(pending);
    }

    /// WebView2 has no cookie policy setting.
    pub fn apply(&self, _cfg: &Config) {}

    pub fn clear(&self, kinds: DataKinds) {
        let mut flags = 0;
        if kinds.site_data {
            flags |= COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_SITE.0;
        }
        if kinds.cache {
            flags |= COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0;
        }
        if flags == 0 {
            return;
        }
        let Some(p) = self.0.profile.borrow().clone() else {
            // No view has opened the profile yet: clear when one does.
            let prev = self.0.pending_clear.get();
            self.0.pending_clear.set(DataKinds {
                site_data: prev.site_data || kinds.site_data,
                cache: prev.cache || kinds.cache,
            });
            return;
        };
        if let Ok(p) = p.cast::<ICoreWebView2Profile2>() {
            let done = ClearBrowsingDataCompletedHandler::create(Box::new(|hr| {
                if let Err(e) = hr {
                    eprintln!("mallow: clearing site data failed: {e}");
                }
                Ok(())
            }));
            let _ = unsafe { p.ClearBrowsingData(COREWEBVIEW2_BROWSING_DATA_KINDS(flags), &done) };
        }
    }

    pub async fn favicon(&self, _page: &str) -> Option<gdk::Texture> {
        None
    }

    pub fn connect_download(&self, f: impl Fn(Download) + 'static) {
        self.0.downloads.add(next_id(), Rc::new(f));
    }
}

// -------------------------------------------------------------------- view

#[derive(Clone)]
struct Ready {
    controller: ICoreWebView2Controller,
    core: ICoreWebView2,
}

type Queued = Box<dyn FnOnce(&Ready)>;

pub struct Inner {
    placeholder: gtk::DrawingArea,
    session: Session,
    ready: RefCell<Option<Ready>>,
    queue: RefCell<Vec<Queued>>,
    parent: Cell<usize>,
    bounds: Cell<(i32, i32, i32, i32)>,
    visible: Cell<bool>,
    loading: Cell<bool>,
    progress: Cell<f64>,
    /// The address being loaded, for `uri()` before the view is ready and for failures.
    pending_uri: RefCell<Option<String>>,
    /// While the error page shows, the address that failed.
    error_uri: RefCell<Option<String>>,
    /// The next navigation is our error page.
    error_nav: Cell<bool>,
    title: Slot<dyn Fn(&View)>,
    uri: Slot<dyn Fn(&View)>,
    loading_changed: Slot<dyn Fn(&View)>,
    progress_changed: Slot<dyn Fn(&View)>,
    load: Slot<dyn Fn(&View, LoadEvent)>,
    load_failed: Slot<dyn Fn(&str, LoadError, &str) -> Option<String>>,
    create: Slot<dyn Fn(&View) -> Option<View>>,
    close: Slot<dyn Fn()>,
    navigation: Slot<dyn Fn(&NavigationAction) -> bool>,
    response: Slot<dyn Fn(&Response) -> bool>,
    permission: Slot<dyn Fn(&View, PermissionRequest)>,
    find_result: Slot<dyn Fn(u32)>,
    find_text: RefCell<Option<String>>,
    destroyed: RefCell<Vec<Box<dyn FnOnce()>>>,
}

impl Inner {
    /// Runs `f` now if the controller is ready, otherwise once it is.
    fn with(&self, f: impl FnOnce(&Ready) + 'static) {
        let ready = self.ready.borrow().clone();
        match ready {
            Some(r) => f(&r),
            None => self.queue.borrow_mut().push(Box::new(f)),
        }
    }

    /// Like `with`, but ahead of anything already queued.
    fn with_first(&self, f: impl FnOnce(&Ready) + 'static) {
        let ready = self.ready.borrow().clone();
        match ready {
            Some(r) => f(&r),
            None => self.queue.borrow_mut().insert(0, Box::new(f)),
        }
    }

    fn core(&self) -> Option<ICoreWebView2> {
        self.ready.borrow().as_ref().map(|r| r.core.clone())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(r) = self.ready.take() {
            let _ = unsafe { r.controller.Close() };
        }
        for f in self.destroyed.take() {
            f();
        }
    }
}

#[derive(Clone)]
pub struct View(Rc<Inner>);

impl PartialEq for View {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

pub struct WeakView(Weak<Inner>);

impl WeakView {
    pub fn upgrade(&self) -> Option<View> {
        self.0.upgrade().map(View)
    }
}

/// What a suspended tab keeps: WebView2 can't serialize history, only the address.
pub struct SessionState(String);

impl View {
    pub fn new(_engine: &Engine, session: &Session, related: Option<&View>, _autoplay: Autoplay) -> Self {
        // A page-opened window must share its opener's profile.
        let session = related.map_or_else(|| session.clone(), |r| r.0.session.clone());
        let placeholder = gtk::DrawingArea::builder().focusable(true).build();
        let inner = Rc::new(Inner {
            placeholder,
            session,
            ready: RefCell::default(),
            queue: RefCell::default(),
            parent: Cell::new(0),
            bounds: Cell::new((0, 0, 0, 0)),
            visible: Cell::new(false),
            loading: Cell::new(false),
            progress: Cell::new(0.0),
            pending_uri: RefCell::default(),
            error_uri: RefCell::default(),
            error_nav: Cell::new(false),
            title: Slot::default(),
            uri: Slot::default(),
            loading_changed: Slot::default(),
            progress_changed: Slot::default(),
            load: Slot::default(),
            load_failed: Slot::default(),
            create: Slot::default(),
            close: Slot::default(),
            navigation: Slot::default(),
            response: Slot::default(),
            permission: Slot::default(),
            find_result: Slot::default(),
            find_text: RefCell::default(),
            destroyed: RefCell::default(),
        });
        create_controller(&inner);
        attach_native(&inner);
        View(inner)
    }

    #[cfg(test)]
    pub fn standalone() -> Self {
        Self::new(&Engine::new(), &Session::ephemeral(), None, Autoplay::Allow)
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.placeholder.clone().upcast()
    }

    pub fn downgrade(&self) -> WeakView {
        WeakView(Rc::downgrade(&self.0))
    }

    pub fn grab_focus(&self) {
        self.0.placeholder.grab_focus();
        self.0.with(|r| {
            let _ = unsafe {
                r.controller
                    .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
            };
        });
    }

    // --- state
    pub fn uri(&self) -> Option<String> {
        if let Some(u) = self.0.error_uri.borrow().clone() {
            return Some(u);
        }
        match self.0.core() {
            Some(core) => get_str(|p| unsafe { core.Source(p) }).filter(|s| !s.is_empty()),
            None => self.0.pending_uri.borrow().clone(),
        }
    }
    pub fn title(&self) -> Option<String> {
        let core = self.0.core()?;
        get_str(|p| unsafe { core.DocumentTitle(p) }).filter(|s| !s.is_empty())
    }
    pub fn favicon(&self) -> Option<gdk::Texture> {
        None
    }
    pub fn is_loading(&self) -> bool {
        self.0.loading.get()
    }
    pub fn progress(&self) -> f64 {
        self.0.progress.get()
    }
    pub fn can_go_back(&self) -> bool {
        self.0
            .core()
            .is_some_and(|c| get_bool(|b| unsafe { c.CanGoBack(b) }))
    }
    pub fn can_go_forward(&self) -> bool {
        self.0
            .core()
            .is_some_and(|c| get_bool(|b| unsafe { c.CanGoForward(b) }))
    }
    pub fn has_history(&self) -> bool {
        self.uri().is_some_and(|u| u != "about:blank")
    }
    pub fn is_playing_audio(&self) -> bool {
        self.0
            .core()
            .and_then(|c| c.cast::<ICoreWebView2_8>().ok())
            .is_some_and(|c| get_bool(|b| unsafe { c.IsDocumentPlayingAudio(b) }))
    }
    pub fn zoom(&self) -> f64 {
        let mut z = 1.0;
        if let Some(r) = self.0.ready.borrow().as_ref() {
            let _ = unsafe { r.controller.ZoomFactor(&mut z) };
        }
        z
    }
    /// Untrusted certificates are cancelled before the page commits, so a
    /// committed HTTPS page has a trusted certificate.
    pub fn certificate(&self) -> Option<Certificate> {
        let uri = self.uri()?;
        if !uri.starts_with("https://")
            || self.0.error_uri.borrow().is_some()
            || self.is_loading() && !self.has_history()
        {
            return None;
        }
        let host = glib::Uri::parse(&uri, glib::UriFlags::NONE)
            .ok()
            .and_then(|u| u.host())
            .map(String::from)
            .unwrap_or_default();
        Some(Certificate {
            problems: None,
            subject: format!("CN={host}"),
            issuer: String::new(),
            not_before: None,
            not_after: None,
            der: None,
        })
    }

    // --- actions
    pub fn load(&self, uri: &str) {
        self.0.pending_uri.replace(Some(uri.to_string()));
        let uri = hs(uri);
        self.0.with(move |r| {
            let _ = unsafe { r.core.Navigate(&uri) };
        });
    }
    #[cfg(test)]
    pub fn load_html(&self, html: &str, _base: Option<&str>) {
        let html = hs(html);
        self.0.with(move |r| {
            let _ = unsafe { r.core.NavigateToString(&html) };
        });
    }
    pub fn reload(&self) {
        // The error page was loaded from a string; reload the address that failed.
        if let Some(u) = self.0.error_uri.borrow().clone() {
            return self.load(&u);
        }
        self.0.with(|r| {
            let _ = unsafe { r.core.Reload() };
        });
    }
    pub fn stop(&self) {
        self.0.with(|r| {
            let _ = unsafe { r.core.Stop() };
        });
    }
    pub fn go_back(&self) {
        self.0.with(|r| {
            let _ = unsafe { r.core.GoBack() };
        });
    }
    pub fn go_forward(&self) {
        self.0.with(|r| {
            let _ = unsafe { r.core.GoForward() };
        });
    }
    pub fn set_zoom(&self, level: f64) {
        self.0.with(move |r| {
            let _ = unsafe { r.controller.SetZoomFactor(level) };
        });
    }
    pub fn set_muted(&self, muted: bool) {
        self.0.with(move |r| {
            if let Ok(c) = r.core.cast::<ICoreWebView2_8>() {
                let _ = unsafe { c.SetIsMuted(muted) };
            }
        });
    }
    /// WebView2 ends the renderer when the controller closes, on drop.
    pub fn terminate(&self) {}
    pub fn session_state(&self) -> Option<SessionState> {
        self.uri().map(SessionState)
    }
    pub fn restore(&self, state: &SessionState) {
        self.load(&state.0);
    }

    // --- find in page
    pub fn find(&self, text: &str) {
        self.0
            .find_text
            .replace((!text.is_empty()).then(|| text.to_string()));
        if text.is_empty() {
            return;
        }
        let term = hs(text);
        self.0.with(move |r| {
            let (Some(find), Some(env)) = (
                find_of(&r.core),
                env().and_then(|e| e.cast::<ICoreWebView2Environment15>().ok()),
            ) else {
                return;
            };
            unsafe {
                let Ok(opts) = env.CreateFindOptions() else { return };
                let _ = opts.SetFindTerm(&term);
                let _ = find.Start(&opts, &FindStartCompletedHandler::create(Box::new(|_| Ok(()))));
            }
        });
    }
    pub fn find_text(&self) -> Option<String> {
        self.0.find_text.borrow().clone()
    }
    pub fn find_next(&self, backwards: bool) {
        self.0.with(move |r| {
            if let Some(find) = find_of(&r.core) {
                let _ = unsafe {
                    if backwards {
                        find.FindPrevious()
                    } else {
                        find.FindNext()
                    }
                };
            }
        });
    }
    pub fn find_finish(&self) {
        self.0.find_text.replace(None);
        self.0.with(|r| {
            if let Some(find) = find_of(&r.core) {
                let _ = unsafe { find.Stop() };
            }
        });
    }

    // --- scripts
    /// Runs `body` as an async function in the page's main frame, with
    /// `args`' keys as variables. WebView2 has no isolated worlds, so a call
    /// that names one fails rather than running where the page can see it.
    pub async fn call_script(
        &self,
        body: &str,
        args: &serde_json::Value,
        world: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        if world.is_some() {
            return Err("WebView2 has no isolated script worlds".into());
        }
        let mut names = Vec::new();
        for k in args.as_object().into_iter().flatten().map(|(k, _)| k) {
            if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("unsupported script argument {k}"));
            }
            names.push(k.as_str());
        }
        let args = if args.is_object() {
            args.to_string()
        } else {
            "{}".into()
        };
        // The result comes back as JSON text, so any value converts the same way.
        let expression = format!(
            "(async ({{{}}}) => JSON.stringify(await (async () => {{ {body} }})()))({args})",
            names.join(",")
        );
        // ExecuteScript doesn't await promises; the DevTools protocol does.
        let params = hs(&serde_json::json!({
            "expression": expression,
            "awaitPromise": true,
            "returnByValue": true,
        })
        .to_string());
        let (tx, rx) = futures_channel::oneshot::channel::<Result<serde_json::Value, String>>();
        self.0.with(move |r| {
            let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |hr, json| {
                let _ = tx.send(hr.map_err(|e| e.to_string()).and_then(|()| script_result(&json)));
                Ok(())
            }));
            let _ = unsafe {
                r.core
                    .CallDevToolsProtocolMethod(&hs("Runtime.evaluate"), &params, &done)
            };
        });
        rx.await.map_err(|_| "script cancelled".to_string())?
    }

    pub fn run_script(&self, body: &'static str, args: serde_json::Value, world: Option<&'static str>) {
        let v = self.clone();
        glib::spawn_future_local(async move {
            let _ = v.call_script(body, &args, world).await;
        });
    }

    /// Script handlers live in named worlds, which WebView2 lacks.
    pub fn add_script_handler(
        &self,
        _world: &str,
        _channel: &str,
        _f: impl Fn(&str) + 'static,
    ) -> Option<Handler> {
        None
    }

    pub fn remove_script_handler(&self, _world: &str, _channel: &str, _handler: Handler) {}

    // --- events
    pub fn disconnect(&self, h: Handler) {
        let i = &self.0;
        i.title.remove(h.0);
        i.uri.remove(h.0);
        i.loading_changed.remove(h.0);
        i.progress_changed.remove(h.0);
        i.load.remove(h.0);
    }
    fn on(&self, slot: &Slot<dyn Fn(&View)>, f: impl Fn(&View) + 'static) -> Handler {
        let id = next_id();
        slot.add(id, Rc::new(f));
        Handler(id)
    }
    pub fn connect_title(&self, f: impl Fn(&View) + 'static) -> Handler {
        self.on(&self.0.title, f)
    }
    /// Favicons aren't wired on this engine: never fires.
    pub fn connect_favicon(&self, _f: impl Fn(&View) + 'static) -> Handler {
        Handler(next_id())
    }
    pub fn connect_uri(&self, f: impl Fn(&View) + 'static) -> Handler {
        self.on(&self.0.uri, f)
    }
    pub fn connect_loading(&self, f: impl Fn(&View) + 'static) -> Handler {
        self.on(&self.0.loading_changed, f)
    }
    pub fn connect_progress(&self, f: impl Fn(&View) + 'static) -> Handler {
        self.on(&self.0.progress_changed, f)
    }
    pub fn connect_load(&self, f: impl Fn(&View, LoadEvent) + 'static) -> Handler {
        let id = next_id();
        self.0.load.add(id, Rc::new(f));
        Handler(id)
    }
    pub fn connect_load_failed(&self, f: impl Fn(&str, LoadError, &str) -> Option<String> + 'static) {
        self.0.load_failed.add(next_id(), Rc::new(f));
    }
    pub fn connect_create(&self, f: impl Fn(&View) -> Option<View> + 'static) {
        self.0.create.add(next_id(), Rc::new(f));
    }
    pub fn connect_close(&self, f: impl Fn() + 'static) {
        self.0.close.add(next_id(), Rc::new(f));
    }
    pub fn connect_navigation(&self, f: impl Fn(&NavigationAction) -> bool + 'static) {
        self.0.navigation.add(next_id(), Rc::new(f));
    }
    /// WebView2 has no response policy: never fires.
    pub fn connect_response(&self, f: impl Fn(&Response) -> bool + 'static) {
        self.0.response.add(next_id(), Rc::new(f));
    }
    pub fn connect_permission(&self, f: impl Fn(&View, PermissionRequest) + 'static) {
        self.0.permission.add(next_id(), Rc::new(f));
    }
    /// Not wired yet: never fires.
    pub fn connect_hover_link(&self, _f: impl Fn(Option<String>) + 'static) {}
    pub fn connect_find_result(&self, f: impl Fn(u32) + 'static) {
        self.0.find_result.add(next_id(), Rc::new(f));
    }
    pub fn on_destroy(&self, f: impl FnOnce() + 'static) {
        self.0.destroyed.borrow_mut().push(Box::new(f));
    }
}

fn find_of(core: &ICoreWebView2) -> Option<ICoreWebView2Find> {
    core.cast::<ICoreWebView2_28>()
        .ok()
        .and_then(|c| unsafe { c.Find() }.ok())
}

/// The value of a `Runtime.evaluate` reply whose expression returned JSON text.
fn script_result(reply: &str) -> Result<serde_json::Value, String> {
    let v: serde_json::Value = serde_json::from_str(reply).map_err(|e| e.to_string())?;
    if let Some(ex) = v.get("exceptionDetails") {
        let msg = ex
            .pointer("/exception/description")
            .or_else(|| ex.get("text"))
            .and_then(|m| m.as_str())
            .unwrap_or("script failed");
        return Err(msg.to_string());
    }
    Ok(v.pointer("/result/value")
        .and_then(|s| s.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null))
}

// ------------------------------------------------------------- controller

fn create_controller(inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    let (name, private) = (inner.session.0.name.clone(), inner.session.0.private);
    with_env(move |env| {
        let made = (|| -> windows::core::Result<()> {
            let env10 = env.cast::<ICoreWebView2Environment10>()?;
            let opts = unsafe { env10.CreateCoreWebView2ControllerOptions()? };
            unsafe {
                opts.SetProfileName(&hs(&name))?;
                opts.SetIsInPrivateModeEnabled(private)?;
            }
            let done =
                CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, controller| {
                    let controller = match (hr, controller) {
                        (Ok(()), Some(c)) => c,
                        (r, _) => {
                            eprintln!("mallow: WebView2 view failed: {r:?}");
                            return Ok(());
                        }
                    };
                    match weak.upgrade() {
                        Some(inner) => {
                            if let Err(e) = setup(&inner, controller) {
                                eprintln!("mallow: WebView2 view setup failed: {e}");
                            }
                        }
                        None => {
                            let _ = unsafe { controller.Close() };
                        }
                    }
                    Ok(())
                }));
            unsafe { env10.CreateCoreWebView2ControllerWithOptions(parking(), &opts, &done) }
        })();
        if let Err(e) = made {
            eprintln!("mallow: WebView2 view failed: {e}");
        }
    });
}

fn fire(inner: &Rc<Inner>, slot: &Slot<dyn Fn(&View)>) {
    let view = View(inner.clone());
    for f in slot.all() {
        f(&view);
    }
}

fn load_event(inner: &Rc<Inner>, event: LoadEvent) {
    let view = View(inner.clone());
    for f in inner.load.all() {
        f(&view, event);
    }
}

fn set_progress(inner: &Rc<Inner>, loading: bool, progress: f64) {
    if inner.loading.replace(loading) != loading {
        fire(inner, &inner.loading_changed);
    }
    inner.progress.set(progress);
    fire(inner, &inner.progress_changed);
}

/// Wraps an event closure so it runs only while the view is alive.
macro_rules! on {
    ($weak:expr, |$inner:ident, $args:ident| $body:expr) => {{
        let weak = $weak.clone();
        Box::new(move |_, $args| {
            if let Some($inner) = weak.upgrade() {
                $body;
            }
            Ok(())
        })
    }};
}

fn setup(inner: &Rc<Inner>, controller: ICoreWebView2Controller) -> windows::core::Result<()> {
    let core = unsafe { controller.CoreWebView2()? };
    let weak = Rc::downgrade(inner);
    let mut t = 0i64;
    unsafe {
        let settings = core.Settings()?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetAreDevToolsEnabled(true)?;

        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(on!(weak, |inner, args| if let Some(a) = args {
                navigation_starting(&inner, &a)
            })),
            &mut t,
        )?;
        core.add_ContentLoading(
            &ContentLoadingEventHandler::create(on!(weak, |inner, _a| {
                set_progress(&inner, true, 0.5);
                load_event(&inner, LoadEvent::Committed)
            })),
            &mut t,
        )?;
        core.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(on!(weak, |inner, args| if let Some(a) = args {
                navigation_completed(&inner, &a)
            })),
            &mut t,
        )?;
        core.add_SourceChanged(
            &SourceChangedEventHandler::create(on!(weak, |inner, _a| fire(&inner, &inner.uri))),
            &mut t,
        )?;
        core.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(on!(weak, |inner, _a| fire(&inner, &inner.title))),
            &mut t,
        )?;
        core.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(on!(weak, |inner, args| if let Some(a) = args {
                new_window(&inner, a)
            })),
            &mut t,
        )?;
        core.add_WindowCloseRequested(
            &WindowCloseRequestedEventHandler::create(on!(weak, |inner, _a| for f in inner.close.all() {
                f()
            })),
            &mut t,
        )?;
        core.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(on!(weak, |inner, args| if let Some(a) = args {
                permission(&inner, a)
            })),
            &mut t,
        )?;
        if let Ok(c4) = core.cast::<ICoreWebView2_4>() {
            c4.add_DownloadStarting(
                &DownloadStartingEventHandler::create(on!(weak, |inner, args| if let Some(a) = args {
                    download_starting(&inner, &a)
                })),
                &mut t,
            )?;
        }
        if let Ok(c14) = core.cast::<ICoreWebView2_14>() {
            c14.add_ServerCertificateErrorDetected(
                &ServerCertificateErrorDetectedEventHandler::create(Box::new(|_, args| {
                    if let Some(a) = args {
                        a.SetAction(COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_CANCEL)?;
                    }
                    Ok(())
                })),
                &mut t,
            )?;
        }
        if let Some(find) = find_of(&core) {
            let w = weak.clone();
            find.add_MatchCountChanged(
                &FindMatchCountChangedEventHandler::create(Box::new(move |find, _| {
                    let (Some(inner), Some(find)) = (w.upgrade(), find) else {
                        return Ok(());
                    };
                    let mut n = 0;
                    find.MatchCount(&mut n)?;
                    for f in inner.find_result.all() {
                        f(n.max(0) as u32);
                    }
                    Ok(())
                })),
                &mut t,
            )?;
        }
        core.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(|_, _| {
                eprintln!("mallow: WebView2 process failed");
                Ok(())
            })),
            &mut t,
        )?;
    }
    if let Ok(p) = core
        .cast::<ICoreWebView2_13>()
        .and_then(|c| unsafe { c.Profile() })
    {
        inner.session.register(p);
    }
    let ready = Ready { controller, core };
    inner.ready.replace(Some(ready.clone()));
    for f in inner.queue.take() {
        f(&ready);
    }
    Ok(())
}

fn navigation_starting(inner: &Rc<Inner>, args: &ICoreWebView2NavigationStartingEventArgs) {
    if inner.error_nav.replace(false) {
        return;
    }
    let uri = get_str(|p| unsafe { args.Uri(p) }).unwrap_or_default();
    let user = get_bool(|b| unsafe { args.IsUserInitiated(b) });
    let redirect = get_bool(|b| unsafe { args.IsRedirected(b) });
    let action = NavigationAction {
        uri: uri.clone(),
        user_gesture: user,
        redirect,
        new_window: false,
        link_click: user && !redirect,
        background: false,
    };
    if inner.navigation.all().iter().any(|f| f(&action)) {
        let _ = unsafe { args.SetCancel(true) };
        return;
    }
    inner.error_uri.replace(None);
    inner.pending_uri.replace(Some(uri));
    set_progress(inner, true, 0.1);
    load_event(
        inner,
        if redirect {
            LoadEvent::Redirected
        } else {
            LoadEvent::Started
        },
    );
}

fn navigation_completed(inner: &Rc<Inner>, args: &ICoreWebView2NavigationCompletedEventArgs) {
    // A server that answered (even 404) shows its own page.
    let answered = args
        .cast::<ICoreWebView2NavigationCompletedEventArgs2>()
        .is_ok_and(|a| {
            let mut code = 0;
            unsafe { a.HttpStatusCode(&mut code) }.is_ok() && code > 0
        });
    if get_bool(|b| unsafe { args.IsSuccess(b) }) || answered {
        set_progress(inner, false, 1.0);
        load_event(inner, LoadEvent::Finished);
        return;
    }
    set_progress(inner, false, 1.0);
    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN;
    let _ = unsafe { args.WebErrorStatus(&mut status) };
    let (kind, message) = match status {
        COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED => return,
        COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED => {
            (LoadError::NotFound, "The host name could not be resolved.")
        }
        COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID
        | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS => {
            (LoadError::Tls, "The server's certificate is not trusted.")
        }
        COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT | COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE => {
            (LoadError::Refused, "Could not connect to the server.")
        }
        COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => (LoadError::TimedOut, "The connection timed out."),
        COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET => (LoadError::Offline, "The connection was lost."),
        _ => (LoadError::Other, "The page could not be loaded."),
    };
    let failing = inner.pending_uri.borrow().clone().unwrap_or_default();
    let page = inner
        .load_failed
        .all()
        .iter()
        .find_map(|f| f(&failing, kind, message));
    if let (Some(html), Some(core)) = (page, inner.core()) {
        inner.error_uri.replace(Some(failing));
        inner.error_nav.set(true);
        let _ = unsafe { core.NavigateToString(&hs(&html)) };
        fire(inner, &inner.uri);
    }
}

fn new_window(inner: &Rc<Inner>, args: ICoreWebView2NewWindowRequestedEventArgs) {
    let uri = get_str(|p| unsafe { args.Uri(p) }).unwrap_or_default();
    let user = get_bool(|b| unsafe { args.IsUserInitiated(b) });
    let action = NavigationAction {
        uri,
        user_gesture: user,
        redirect: false,
        new_window: true,
        link_click: user,
        background: false,
    };
    let opener = View(inner.clone());
    let blocked = inner.navigation.all().iter().any(|f| f(&action));
    let created = if blocked {
        None
    } else {
        inner.create.all().iter().find_map(|f| f(&opener))
    };
    let (Some(view), Ok(deferral)) = (created, unsafe { args.GetDeferral() }) else {
        let _ = unsafe { args.SetHandled(true) };
        return;
    };
    // The page's window must be the new view's first navigation.
    view.0.with_first(move |r| unsafe {
        let _ = args.SetNewWindow(&r.core);
        let _ = deferral.Complete();
    });
}

fn permission(inner: &Rc<Inner>, args: ICoreWebView2PermissionRequestedEventArgs) {
    let mut kind = COREWEBVIEW2_PERMISSION_KIND_UNKNOWN_PERMISSION;
    let _ = unsafe { args.PermissionKind(&mut kind) };
    let kind = match kind {
        COREWEBVIEW2_PERMISSION_KIND_CAMERA => Permission::Media {
            video: true,
            audio: false,
        },
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE => Permission::Media {
            video: false,
            audio: true,
        },
        COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION => Permission::Location,
        COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS => Permission::Notifications,
        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ => Permission::Clipboard,
        _ => Permission::Other,
    };
    let Ok(deferral) = (unsafe { args.GetDeferral() }) else {
        let _ = unsafe { args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY) };
        return;
    };
    let req = PermissionRequest {
        kind,
        answer: Rc::new(Cell::new(Some(Box::new(move |ok: bool| unsafe {
            let _ = args.SetState(if ok {
                COREWEBVIEW2_PERMISSION_STATE_ALLOW
            } else {
                COREWEBVIEW2_PERMISSION_STATE_DENY
            });
            let _ = deferral.Complete();
        })))),
    };
    let view = View(inner.clone());
    match inner.permission.all().first() {
        Some(f) => f(&view, req),
        None => req.deny(),
    }
}

fn download_starting(inner: &Rc<Inner>, args: &ICoreWebView2DownloadStartingEventArgs) {
    let Ok(op) = (unsafe { args.DownloadOperation() }) else {
        return;
    };
    let dl = Download::new(op, inner);
    for f in inner.session.0.downloads.all() {
        f(dl.clone());
    }
    let suggested = get_str(|p| unsafe { args.ResultFilePath(p) }).unwrap_or_default();
    let name = Path::new(&suggested)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dest = dl.0.destination_fn.borrow().as_ref().map(|f| f(&name));
    let dest = dest.unwrap_or_else(|| PathBuf::from(&suggested));
    unsafe {
        let _ = args.SetResultFilePath(&hs(&dest.to_string_lossy()));
        // Mallow shows its own download list.
        let _ = args.SetHandled(true);
    }
    dl.0.destination.replace(Some(dest.clone()));
    for f in dl.0.created.all() {
        f(&dest);
    }
}

/// Keep the native view over the placeholder while it is mapped.
fn attach_native(inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    inner.placeholder.add_tick_callback(move |p, _| {
        let Some(inner) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        place(&inner, p);
        glib::ControlFlow::Continue
    });
    let weak = Rc::downgrade(inner);
    inner.placeholder.connect_unmap(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.visible.set(false);
            inner.with(|r| {
                let _ = unsafe { r.controller.SetIsVisible(false) };
            });
        }
    });
    let weak = Rc::downgrade(inner);
    inner.placeholder.connect_unrealize(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.parent.set(0);
            inner.visible.set(false);
            inner.with(|r| unsafe {
                let _ = r.controller.SetIsVisible(false);
                let _ = r.controller.SetParentWindow(parking());
            });
        }
    });
}

/// Position the controller over the placeholder, in the window it is in.
fn place(inner: &Inner, p: &gtk::DrawingArea) {
    let Some(r) = inner.ready.borrow().clone() else {
        return;
    };
    let Some(root) = p.root() else { return };
    let Some(native) = root.native() else { return };
    let Some(surface) = native.surface() else { return };
    let scale = surface.scale();
    let Ok(surface) = surface.downcast::<gdk4_win32::Win32Surface>() else {
        return;
    };
    let hwnd = HWND(surface.handle().0);
    let Some(b) = p.compute_bounds(&root) else { return };
    // GTK lays out in the surface's coordinates including its CSD shadow;
    // WebView2 wants physical pixels in the window's client area.
    let (ox, oy) = native.surface_transform();
    let px = |v: f64| (v * scale).round() as i32;
    let x = b.x() as f64 + ox;
    let y = b.y() as f64 + oy;
    let bounds = (px(x), px(y), px(x + b.width() as f64), px(y + b.height() as f64));
    unsafe {
        if inner.parent.get() != hwnd.0 as usize {
            let _ = r.controller.SetParentWindow(hwnd);
            inner.parent.set(hwnd.0 as usize);
        }
        if inner.bounds.get() != bounds {
            let (left, top, right, bottom) = bounds;
            let _ = r.controller.SetBounds(RECT {
                left,
                top,
                right,
                bottom,
            });
            inner.bounds.set(bounds);
        }
        if !inner.visible.get() && p.is_mapped() {
            let _ = r.controller.SetIsVisible(true);
            inner.visible.set(true);
        }
    }
}

// -------------------------------------------------------------- permission

#[derive(Clone)]
pub struct PermissionRequest {
    pub kind: Permission,
    answer: Rc<Cell<Option<Box<dyn FnOnce(bool)>>>>,
}

impl PermissionRequest {
    pub fn allow(&self) {
        if let Some(f) = self.answer.take() {
            f(true)
        }
    }
    pub fn deny(&self) {
        if let Some(f) = self.answer.take() {
            f(false)
        }
    }
}

// ---------------------------------------------------------------- download

#[derive(Clone)]
pub struct Download(Rc<DownloadInner>);

/// Picks the save path from the server's suggested file name.
type DestinationFn = Box<dyn Fn(&str) -> PathBuf>;

struct DownloadInner {
    op: ICoreWebView2DownloadOperation,
    view: Weak<Inner>,
    destination_fn: RefCell<Option<DestinationFn>>,
    destination: RefCell<Option<PathBuf>>,
    created: Slot<dyn Fn(&Path)>,
    progress: Slot<dyn Fn(f64, u64)>,
    end: Slot<dyn Fn(&Download, DownloadEnd)>,
}

thread_local! {
    /// Live downloads; WebView2's callbacks hold only weak references.
    static DOWNLOADS: RefCell<Vec<Download>> = const { RefCell::new(Vec::new()) };
}

impl Download {
    fn new(op: ICoreWebView2DownloadOperation, view: &Rc<Inner>) -> Self {
        let d = Download(Rc::new(DownloadInner {
            op,
            view: Rc::downgrade(view),
            destination_fn: RefCell::default(),
            destination: RefCell::default(),
            created: Slot::default(),
            progress: Slot::default(),
            end: Slot::default(),
        }));
        let mut t = 0i64;
        let weak = Rc::downgrade(&d.0);
        let _ = unsafe {
            d.0.op.add_BytesReceivedChanged(
                &BytesReceivedChangedEventHandler::create(Box::new(move |_, _| {
                    if let Some(inner) = weak.upgrade() {
                        let (mut done, mut total) = (0i64, 0i64);
                        inner.op.BytesReceived(&mut done)?;
                        inner.op.TotalBytesToReceive(&mut total)?;
                        let done = done.max(0) as u64;
                        let fraction = if total > 0 {
                            done as f64 / total as f64
                        } else {
                            0.0
                        };
                        for f in inner.progress.all() {
                            f(fraction, done);
                        }
                    }
                    Ok(())
                })),
                &mut t,
            )
        };
        let weak = Rc::downgrade(&d.0);
        let _ = unsafe {
            d.0.op.add_StateChanged(
                &StateChangedEventHandler::create(Box::new(move |_, _| {
                    let Some(inner) = weak.upgrade() else {
                        return Ok(());
                    };
                    let mut state = COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS;
                    inner.op.State(&mut state)?;
                    let end = match state {
                        COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED => DownloadEnd::Done,
                        COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED => {
                            let mut reason = COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_NONE;
                            inner.op.InterruptReason(&mut reason)?;
                            if reason == COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_CANCELED {
                                DownloadEnd::Cancelled
                            } else {
                                DownloadEnd::Failed(format!("Download interrupted (reason {})", reason.0))
                            }
                        }
                        _ => return Ok(()),
                    };
                    Download(inner).end(end);
                    Ok(())
                })),
                &mut t,
            )
        };
        DOWNLOADS.with(|all| all.borrow_mut().push(d.clone()));
        d
    }

    fn end(&self, end: DownloadEnd) {
        for f in self.0.end.all() {
            f(self, end.clone());
        }
        DOWNLOADS.with(|all| all.borrow_mut().retain(|d| !Rc::ptr_eq(&d.0, &self.0)));
    }

    pub fn source(&self) -> Option<String> {
        get_str(|p| unsafe { self.0.op.Uri(p) })
    }
    pub fn view(&self) -> Option<View> {
        self.0.view.upgrade().map(View)
    }
    pub fn destination(&self) -> Option<PathBuf> {
        self.0.destination.borrow().clone()
    }
    pub fn received_bytes(&self) -> u64 {
        let mut n = 0i64;
        let _ = unsafe { self.0.op.BytesReceived(&mut n) };
        n.max(0) as u64
    }
    pub fn cancel(&self) {
        let _ = unsafe { self.0.op.Cancel() };
    }
    pub fn connect_destination(&self, f: impl Fn(&str) -> PathBuf + 'static) {
        self.0.destination_fn.replace(Some(Box::new(f)));
    }
    pub fn connect_created(&self, f: impl Fn(&Path) + 'static) {
        self.0.created.add(next_id(), Rc::new(f));
    }
    pub fn connect_progress(&self, f: impl Fn(f64, u64) + 'static) {
        self.0.progress.add(next_id(), Rc::new(f));
    }
    pub fn connect_end(&self, f: impl Fn(&Download, DownloadEnd) + 'static) {
        self.0.end.add(next_id(), Rc::new(f));
    }
}

#[cfg(test)]
mod tests {
    use super::script_result;
    use serde_json::json;

    #[test]
    fn script_results() {
        let ok = r#"{"result":{"type":"string","value":"{\"a\":1}"}}"#;
        assert_eq!(script_result(ok).unwrap(), json!({"a": 1}));
        let undefined = r#"{"result":{"type":"undefined"}}"#;
        assert_eq!(script_result(undefined).unwrap(), serde_json::Value::Null);
        let thrown = r#"{"result":{},"exceptionDetails":{"text":"Uncaught","exception":{"description":"Error: boom"}}}"#;
        assert_eq!(script_result(thrown).unwrap_err(), "Error: boom");
    }
}
