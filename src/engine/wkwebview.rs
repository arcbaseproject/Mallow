//! macOS backend: WKWebView, embedded in GTK's NSWindow.
//!
//! Each `View` is a GTK placeholder widget. While it is mapped, a native
//! WKWebView sits in the window's content view, kept over the placeholder's
//! allocation every frame. GTK widgets drawn inside the same window (status
//! bar, overlays) appear underneath it; popovers and dialogs are separate
//! windows and are unaffected.
//!
//! Not available through public WKWebView API, so absent here: favicons,
//! link-hover URLs, playing-audio state, geolocation prompts, certificate
//! issuer and dates. Untrusted certificates never load (WebKit rejects them
//! when the navigation delegate leaves authentication to the default).

use super::{
    Certificate, DataKinds, DownloadEnd, LoadError, LoadEvent, NavigationAction, Permission, Response,
};
use crate::config::{Autoplay, Config, Cookies};
use block2::RcBlock;
use gtk::{gdk, glib, prelude::*};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{NSEventModifierFlags, NSView, NSWindow};
use objc2_foundation::{
    NSArray, NSDate, NSDictionary, NSError, NSKeyValueChangeKey, NSKeyValueObservingOptions, NSNumber,
    NSObject, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSPoint, NSRect, NSSet, NSSize,
    NSProgressReporting, NSString, NSURL, NSURLRequest, NSURLResponse, NSHTTPURLResponse,
};
use objc2_web_kit::*;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};

pub const NAME: &str = "WebKit (WKWebView)";

pub fn version() -> String {
    // WebKit ships with macOS; its version is the system's.
    let v = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
    format!("macOS {}.{}.{}", v.majorVersion, v.minorVersion, v.patchVersion)
}

pub fn origin(uri: &str) -> String {
    match glib::Uri::parse(uri, glib::UriFlags::NONE) {
        Ok(u) => {
            let scheme = u.scheme();
            let host = u.host().unwrap_or_default();
            let default = matches!((scheme.as_str(), u.port()), ("https", 443) | ("http", 80) | (_, -1));
            if default {
                format!("{scheme}://{host}")
            } else {
                format!("{scheme}://{host}:{}", u.port())
            }
        }
        Err(_) => "null".into(),
    }
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("engine used off the main thread")
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn url(s: &str) -> Option<Retained<NSURL>> {
    NSURL::URLWithString(&ns(s))
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

thread_local! {
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    /// Configuration WebKit hands us for a page-opened window, consumed by
    /// the next `View::new` with a related view.
    static PENDING_CONFIG: RefCell<Option<Retained<WKWebViewConfiguration>>> = const { RefCell::new(None) };
}

fn next_id() -> u64 {
    NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    })
}

// ------------------------------------------------------------------ engine

pub struct Engine {
    content: Retained<WKUserContentController>,
    rules: RefCell<Option<Retained<WKContentRuleList>>>,
    autoplay_default: Cell<Autoplay>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            content: unsafe { WKUserContentController::new(mtm()) },
            rules: RefCell::default(),
            autoplay_default: Cell::new(Autoplay::NoSound),
        }
    }

    /// Most engine-wide preferences have no WKWebView equivalent; per-view
    /// settings are applied when each view is created.
    pub fn apply(&self, cfg: &Config) {
        self.autoplay_default.set(cfg.autoplay);
    }

    pub fn set_block_rules(&self, rules: String, _cache_dir: &Path) {
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm()) }) else {
            return;
        };
        let content = self.content.clone();
        let block = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| {
            match unsafe { list.as_ref() } {
                Some(list) => unsafe { content.addContentRuleList(list) },
                None => eprintln!(
                    "mallow: content filter failed to compile: {}",
                    unsafe { err.as_ref() }.map_or_else(String::new, |e| e.localizedDescription().to_string())
                ),
            }
        });
        unsafe {
            store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                Some(&ns("mallow-trackers")),
                Some(&ns(&rules)),
                Some(&block),
            )
        };
        let _ = &self.rules;
    }

    pub fn clear_block_rules(&self) {
        unsafe { self.content.removeAllContentRuleLists() };
    }
}

// ----------------------------------------------------------------- session

#[derive(Clone)]
pub struct Session(Rc<SessionInner>);

struct SessionInner {
    store: Retained<WKWebsiteDataStore>,
    downloads: Slot<dyn Fn(Download)>,
}

impl Session {
    /// WKWebView keeps persistent data in the app's own container; the
    /// paths are chosen by the system.
    pub fn persistent(_data: &Path, _cache: &Path) -> Self {
        Self::with(unsafe { WKWebsiteDataStore::defaultDataStore(mtm()) })
    }

    pub fn ephemeral() -> Self {
        Self::with(unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm()) })
    }

    fn with(store: Retained<WKWebsiteDataStore>) -> Self {
        Self(Rc::new(SessionInner {
            store,
            downloads: Slot::default(),
        }))
    }

    pub fn apply(&self, cfg: &Config) {
        // WebKit blocks third-party cookies itself; only "never" is extra.
        let policy = if cfg.cookies == Cookies::Never {
            WKCookiePolicy::Disallow
        } else {
            WKCookiePolicy::Allow
        };
        unsafe {
            self.0
                .store
                .httpCookieStore()
                .setCookiePolicy_completionHandler(policy, None)
        };
    }

    pub fn clear(&self, kinds: DataKinds) {
        let types: Retained<NSSet<NSString>> = if kinds.site_data {
            let all = unsafe { WKWebsiteDataStore::allWebsiteDataTypes(mtm()) };
            if kinds.cache {
                all
            } else {
                let keep: Vec<Retained<NSString>> = all
                    .iter()
                    .filter(|t| {
                        let s = t.to_string();
                        !s.contains("Cache")
                    })
                    .collect();
                NSSet::from_retained_slice(&keep)
            }
        } else if kinds.cache {
            NSSet::from_retained_slice(&[
                ns("WKWebsiteDataTypeDiskCache"),
                ns("WKWebsiteDataTypeMemoryCache"),
            ])
        } else {
            return;
        };
        let done = RcBlock::new(|| {});
        unsafe {
            self.0
                .store
                .removeDataOfTypes_modifiedSince_completionHandler(&types, &NSDate::distantPast(), &done)
        };
    }

    /// WKWebView has no favicon database.
    pub async fn favicon(&self, _page: &str) -> Option<gdk::Texture> {
        None
    }

    pub fn connect_download(&self, f: impl Fn(Download) + 'static) {
        self.0.downloads.add(next_id(), Rc::new(f));
    }
}

// -------------------------------------------------------------- delegates

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MallowWebDelegate"]
    #[ivars = Weak<Inner>]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    impl Delegate {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(
            &self,
            key: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            let (Some(inner), Some(key)) = (self.ivars().upgrade(), key) else { return };
            let view = View(inner.clone());
            let slot = match key.to_string().as_str() {
                "title" => &inner.title,
                "URL" => &inner.uri,
                "loading" => &inner.loading,
                "estimatedProgress" => &inner.progress,
                _ => return,
            };
            for f in slot.all() {
                f(&view);
            }
        }
    }

    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_action(
            &self,
            _web_view: &WKWebView,
            action: &WKNavigationAction,
            handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let cancel = self.ivars().upgrade().is_some_and(|inner| {
                let request = unsafe { action.request() };
                let Some(uri) = request.URL().and_then(|u| u.absoluteString()) else {
                    return false;
                };
                let kind = unsafe { action.navigationType() };
                let mods = unsafe { action.modifierFlags() };
                let a = NavigationAction {
                    uri: uri.to_string(),
                    // No public user-gesture flag: a link click or form
                    // submission is a user action.
                    user_gesture: kind == WKNavigationType::LinkActivated
                        || kind == WKNavigationType::FormSubmitted,
                    redirect: false,
                    new_window: unsafe { action.targetFrame() }.is_none(),
                    link_click: kind == WKNavigationType::LinkActivated,
                    background: unsafe { action.buttonNumber() } == 2
                        || mods.contains(NSEventModifierFlags::Command),
                };
                inner.navigation.all().iter().any(|f| f(&a))
            });
            handler.call((if cancel {
                WKNavigationActionPolicy::Cancel
            } else {
                WKNavigationActionPolicy::Allow
            },));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            _web_view: &WKWebView,
            response: &WKNavigationResponse,
            handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            let download = self.ivars().upgrade().is_some_and(|inner| {
                let resp = unsafe { response.response() };
                let attachment = resp
                    .downcast_ref::<NSHTTPURLResponse>()
                    .and_then(|h| h.valueForHTTPHeaderField(&ns("Content-Disposition")))
                    .is_some_and(|v| v.to_string().trim_start().to_ascii_lowercase().starts_with("attachment"));
                let r = Response {
                    main_resource: unsafe { response.isForMainFrame() },
                    displayable: unsafe { response.canShowMIMEType() },
                    attachment,
                };
                inner.response.all().iter().any(|f| f(&r))
            });
            handler.call((if download {
                WKNavigationResponsePolicy::Download
            } else {
                WKNavigationResponsePolicy::Allow
            },));
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn started(&self, _w: &WKWebView, _n: Option<&WKNavigation>) {
            self.load(LoadEvent::Started);
        }

        #[unsafe(method(webView:didReceiveServerRedirectForProvisionalNavigation:))]
        fn redirected(&self, _w: &WKWebView, _n: Option<&WKNavigation>) {
            self.load(LoadEvent::Redirected);
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn committed(&self, _w: &WKWebView, _n: Option<&WKNavigation>) {
            self.load(LoadEvent::Committed);
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, _w: &WKWebView, _n: Option<&WKNavigation>) {
            self.load(LoadEvent::Finished);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_early(&self, w: &WKWebView, _n: Option<&WKNavigation>, error: &NSError) {
            self.failed(w, error);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed_late(&self, w: &WKWebView, _n: Option<&WKNavigation>, error: &NSError) {
            self.failed(w, error);
        }

        #[unsafe(method(webView:navigationResponse:didBecomeDownload:))]
        fn response_download(&self, _w: &WKWebView, _r: &WKNavigationResponse, download: &WKDownload) {
            self.download(download);
        }

        #[unsafe(method(webView:navigationAction:didBecomeDownload:))]
        fn action_download(&self, _w: &WKWebView, _a: &WKNavigationAction, download: &WKDownload) {
            self.download(download);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn crashed(&self, w: &WKWebView) {
            let uri = unsafe { w.URL() }.and_then(|u| u.absoluteString());
            eprintln!("mallow: web process for {uri:?} terminated");
        }
    }

    unsafe impl WKUIDelegate for Delegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create(
            &self,
            _w: &WKWebView,
            config: &WKWebViewConfiguration,
            _action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            let inner = self.ivars().upgrade();
            inner.and_then(|inner| {
                PENDING_CONFIG.with(|c| c.replace(Some(config.retain())));
                let opener = View(inner.clone());
                let created = inner.create.all().iter().find_map(|f| f(&opener));
                PENDING_CONFIG.with(|c| c.replace(None));
                created.map(|v| v.0.wk.clone())
            })
        }

        #[unsafe(method(webViewDidClose:))]
        fn closed(&self, _w: &WKWebView) {
            if let Some(inner) = self.ivars().upgrade() {
                for f in inner.close.all() {
                    f();
                }
            }
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media(
            &self,
            _w: &WKWebView,
            _origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            kind: WKMediaCaptureType,
            handler: &block2::DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            let Some(inner) = self.ivars().upgrade() else {
                handler.call((WKPermissionDecision::Deny,));
                return;
            };
            let (video, audio) = match kind {
                WKMediaCaptureType::Camera => (true, false),
                WKMediaCaptureType::Microphone => (false, true),
                _ => (true, true),
            };
            let ptr = handler as *const block2::DynBlock<dyn Fn(WKPermissionDecision)>
                as *mut block2::DynBlock<dyn Fn(WKPermissionDecision)>;
            let Some(decide) = (unsafe { RcBlock::copy(ptr) }) else {
                return;
            };
            let req = PermissionRequest {
                kind: Permission::Media { video, audio },
                answer: Rc::new(Cell::new(Some(Box::new(move |ok: bool| {
                    decide.call((if ok {
                        WKPermissionDecision::Grant
                    } else {
                        WKPermissionDecision::Deny
                    },));
                })))),
            };
            let view = View(inner.clone());
            match inner.permission.all().first() {
                Some(f) => f(&view, req),
                None => req.deny(),
            }
        }
    }

    unsafe impl WKDownloadDelegate for Delegate {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn destination(
            &self,
            download: &WKDownload,
            _response: &NSURLResponse,
            suggested: &NSString,
            handler: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            let dl = Download::of(download);
            let path = dl.0.destination_fn.borrow().as_ref().map(|f| f(&suggested.to_string()));
            match path.and_then(|p| {
                dl.0.destination.replace(Some(p.clone()));
                NSURL::fileURLWithPath(&ns(&p.to_string_lossy())).into()
            }) {
                Some(u) => {
                    for f in dl.0.created.all() {
                        f(dl.0.destination.borrow().as_deref().unwrap_or(Path::new("")));
                    }
                    handler.call((Retained::as_ptr(&u) as *mut NSURL,))
                }
                None => handler.call((std::ptr::null_mut(),)),
            }
        }

        #[unsafe(method(downloadDidFinish:))]
        fn download_done(&self, download: &WKDownload) {
            Download::of(download).end(DownloadEnd::Done);
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn download_failed(&self, download: &WKDownload, error: &NSError, _resume: Option<&objc2_foundation::NSData>) {
            // NSURLErrorCancelled
            let end = if error.code() == -999 {
                DownloadEnd::Cancelled
            } else {
                DownloadEnd::Failed(error.localizedDescription().to_string())
            };
            Download::of(download).end(end);
        }
    }
);

impl Delegate {
    fn new(inner: Weak<Inner>) -> Retained<Self> {
        let this = mtm().alloc::<Self>().set_ivars(inner);
        unsafe { msg_send![super(this), init] }
    }

    fn load(&self, event: LoadEvent) {
        if let Some(inner) = self.ivars().upgrade() {
            let view = View(inner.clone());
            for f in inner.load.all() {
                f(&view, event);
            }
        }
    }

    fn failed(&self, w: &WKWebView, error: &NSError) {
        let Some(inner) = self.ivars().upgrade() else { return };
        let domain = error.domain().to_string();
        let code = error.code();
        // Cancelled (Stop, new navigation) and "frame load interrupted" (a
        // response turned into a download) keep the current page.
        if (domain == "NSURLErrorDomain" && code == -999) || (domain == "WebKitErrorDomain" && code == 102) {
            return;
        }
        let kind = match code {
            -1003 | -1006 => LoadError::NotFound,
            -1206..=-1200 => LoadError::Tls,
            -1004 => LoadError::Refused,
            -1001 => LoadError::TimedOut,
            -1009 | -1005 => LoadError::Offline,
            _ => LoadError::Other,
        };
        let failing = error
            .userInfo()
            .objectForKey(&ns("NSErrorFailingURLStringKey"))
            .and_then(|o| o.downcast::<NSString>().ok())
            .map(|s| s.to_string())
            .or_else(|| unsafe { w.URL() }.and_then(|u| u.absoluteString()).map(|s| s.to_string()))
            .unwrap_or_default();
        let message = error.localizedDescription().to_string();
        let page = inner
            .load_failed
            .all()
            .iter()
            .find_map(|f| f(&failing, kind, &message));
        if let Some(html) = page {
            unsafe { w.loadHTMLString_baseURL(&ns(&html), url(&failing).as_deref()) };
        }
    }

    fn download(&self, download: &WKDownload) {
        let Some(inner) = self.ivars().upgrade() else { return };
        unsafe { download.setDelegate(Some(ProtocolObject::from_ref(self))) };
        let dl = Download::of(download);
        dl.0.view.replace(Rc::downgrade(&inner));
        for f in inner.session.0.downloads.all() {
            f(dl.clone());
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MallowScriptHandler"]
    #[ivars = Box<dyn Fn(&str)>]
    struct ScriptHandler;

    unsafe impl NSObjectProtocol for ScriptHandler {}

    unsafe impl WKScriptMessageHandler for ScriptHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn received(&self, _c: &WKUserContentController, message: &WKScriptMessage) {
            if let Ok(s) = unsafe { message.body() }.downcast::<NSString>() {
                (self.ivars())(&s.to_string());
            }
        }
    }
);

// -------------------------------------------------------------------- view

const OBSERVED: [&str; 4] = ["title", "URL", "loading", "estimatedProgress"];

pub struct Inner {
    placeholder: gtk::DrawingArea,
    wk: Retained<WKWebView>,
    delegate: RefCell<Option<Retained<Delegate>>>,
    session: Session,
    title: Slot<dyn Fn(&View)>,
    uri: Slot<dyn Fn(&View)>,
    loading: Slot<dyn Fn(&View)>,
    progress: Slot<dyn Fn(&View)>,
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

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(d) = self.delegate.take() {
            for key in OBSERVED {
                unsafe { self.wk.removeObserver_forKeyPath(&d, &ns(key)) };
            }
            unsafe {
                self.wk.setNavigationDelegate(None);
                self.wk.setUIDelegate(None);
            }
        }
        self.wk.removeFromSuperview();
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

/// Back/forward history of a view (WebKit's opaque interaction state).
pub struct SessionState(Retained<AnyObject>);

impl View {
    pub fn new(engine: &Engine, session: &Session, related: Option<&View>, autoplay: Autoplay) -> Self {
        let mtm = mtm();
        // A page-opened window must use the configuration WebKit provides.
        let pending = related.and_then(|_| PENDING_CONFIG.with(|c| c.borrow_mut().take()));
        let (config, session) = match (pending, related) {
            (Some(c), Some(r)) => (c, r.0.session.clone()),
            _ => {
                let c = unsafe { WKWebViewConfiguration::new(mtm) };
                unsafe {
                    c.setWebsiteDataStore(&session.0.store);
                    c.setUserContentController(&engine.content);
                    c.setMediaTypesRequiringUserActionForPlayback(match autoplay {
                        Autoplay::Allow => WKAudiovisualMediaTypes::None,
                        Autoplay::NoSound => WKAudiovisualMediaTypes::Audio,
                        Autoplay::Deny => WKAudiovisualMediaTypes::All,
                    });
                    c.preferences().setJavaScriptCanOpenWindowsAutomatically(false);
                }
                (c, session.clone())
            }
        };
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
        let wk = unsafe { WKWebView::initWithFrame_configuration(mtm.alloc(), frame, &config) };
        unsafe { wk.setInspectable(true) };
        let placeholder = gtk::DrawingArea::builder().focusable(true).build();
        let inner = Rc::new(Inner {
            placeholder,
            wk,
            delegate: RefCell::default(),
            session,
            title: Slot::default(),
            uri: Slot::default(),
            loading: Slot::default(),
            progress: Slot::default(),
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
        let delegate = Delegate::new(Rc::downgrade(&inner));
        unsafe {
            inner.wk.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            inner.wk.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            for key in OBSERVED {
                inner.wk.addObserver_forKeyPath_options_context(
                    &delegate,
                    &ns(key),
                    NSKeyValueObservingOptions::New,
                    std::ptr::null_mut(),
                );
            }
        }
        inner.delegate.replace(Some(delegate));
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
        if let Some(win) = self.0.wk.window() {
            win.makeFirstResponder(Some(&self.0.wk));
        }
    }

    // --- state
    pub fn uri(&self) -> Option<String> {
        unsafe { self.0.wk.URL() }
            .and_then(|u| u.absoluteString())
            .map(|s| s.to_string())
    }
    pub fn title(&self) -> Option<String> {
        unsafe { self.0.wk.title() }.map(|s| s.to_string())
    }
    pub fn favicon(&self) -> Option<gdk::Texture> {
        None
    }
    pub fn is_loading(&self) -> bool {
        unsafe { self.0.wk.isLoading() }
    }
    pub fn progress(&self) -> f64 {
        unsafe { self.0.wk.estimatedProgress() }
    }
    pub fn can_go_back(&self) -> bool {
        unsafe { self.0.wk.canGoBack() }
    }
    pub fn can_go_forward(&self) -> bool {
        unsafe { self.0.wk.canGoForward() }
    }
    pub fn has_history(&self) -> bool {
        unsafe { self.0.wk.backForwardList().currentItem() }.is_some()
    }
    /// Not exposed by WKWebView.
    pub fn is_playing_audio(&self) -> bool {
        false
    }
    pub fn zoom(&self) -> f64 {
        unsafe { self.0.wk.pageZoom() }
    }
    /// WebKit only commits HTTPS pages whose certificate it trusts, so a
    /// committed HTTPS page has a trusted certificate.
    pub fn certificate(&self) -> Option<Certificate> {
        let uri = self.uri()?;
        if !uri.starts_with("https://") || self.is_loading() && !self.has_history() {
            return None;
        }
        let host = glib::Uri::parse(&uri, glib::UriFlags::NONE)
            .ok()
            .and_then(|u| u.host())
            .map(String::from)
            .unwrap_or_default();
        Some(Certificate {
            problems: (!unsafe { self.0.wk.hasOnlySecureContent() })
                .then(|| "Page includes insecure content".to_string()),
            subject: format!("CN={host}"),
            issuer: String::new(),
            not_before: None,
            not_after: None,
            der: None,
        })
    }

    // --- actions
    pub fn load(&self, uri: &str) {
        if let Some(u) = url(uri) {
            unsafe { self.0.wk.loadRequest(&NSURLRequest::requestWithURL(&u)) };
        }
    }
    #[cfg(test)]
    pub fn load_html(&self, html: &str, base: Option<&str>) {
        unsafe { self.0.wk.loadHTMLString_baseURL(&ns(html), base.and_then(url).as_deref()) };
    }
    pub fn reload(&self) {
        unsafe { self.0.wk.reload() };
    }
    pub fn stop(&self) {
        unsafe { self.0.wk.stopLoading() };
    }
    pub fn go_back(&self) {
        unsafe { self.0.wk.goBack() };
    }
    pub fn go_forward(&self) {
        unsafe { self.0.wk.goForward() };
    }
    pub fn set_zoom(&self, level: f64) {
        unsafe { self.0.wk.setPageZoom(level) };
    }
    /// No mute API; a parked view suspends its media instead.
    pub fn set_muted(&self, muted: bool) {
        unsafe { self.0.wk.setAllMediaPlaybackSuspended_completionHandler(muted, None) };
    }
    /// WKWebView ends its process when the view is released.
    pub fn terminate(&self) {}
    pub fn session_state(&self) -> Option<SessionState> {
        unsafe { self.0.wk.interactionState() }.map(SessionState)
    }
    pub fn restore(&self, state: &SessionState) {
        unsafe { self.0.wk.setInteractionState(Some(&state.0)) };
    }

    // --- find in page
    pub fn find(&self, text: &str) {
        self.0.find_text.replace((!text.is_empty()).then(|| text.to_string()));
        if text.is_empty() {
            return;
        }
        // WebKit highlights but doesn't count; count with a page script.
        let (view, needle) = (self.clone(), text.to_string());
        glib::spawn_future_local(async move {
            let count = view
                .call_script(
                    "const t=(document.body?.innerText||'').toLowerCase(), n=needle.toLowerCase(); let c=0,i=0; while(n&&c<1000&&(i=t.indexOf(n,i))!==-1){c++;i+=n.length;} return c;",
                    &serde_json::json!({ "needle": needle }),
                    None,
                )
                .await
                .ok()
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            for f in view.0.find_result.all() {
                f(count);
            }
        });
        self.find_next(false);
    }
    pub fn find_text(&self) -> Option<String> {
        self.0.find_text.borrow().clone()
    }
    pub fn find_next(&self, backwards: bool) {
        let Some(text) = self.find_text() else { return };
        let config = unsafe { WKFindConfiguration::new(mtm()) };
        unsafe {
            config.setCaseSensitive(false);
            config.setWraps(true);
            config.setBackwards(backwards);
        }
        let done = RcBlock::new(|_: std::ptr::NonNull<WKFindResult>| {});
        unsafe {
            self.0
                .wk
                .findString_withConfiguration_completionHandler(&ns(&text), Some(&config), &done)
        };
    }
    pub fn find_finish(&self) {
        self.0.find_text.replace(None);
    }

    // --- scripts
    pub async fn call_script(
        &self,
        body: &str,
        args: &serde_json::Value,
        world: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut keys: Vec<Retained<NSString>> = Vec::new();
        let mut values: Vec<Retained<AnyObject>> = Vec::new();
        for (k, v) in args.as_object().into_iter().flatten() {
            let value: Retained<AnyObject> = match v {
                serde_json::Value::String(s) => ns(s).into(),
                serde_json::Value::Bool(b) => NSNumber::new_bool(*b).into(),
                serde_json::Value::Number(n) => NSNumber::new_f64(n.as_f64().unwrap_or_default()).into(),
                _ => return Err(format!("unsupported script argument {k}")),
            };
            keys.push(ns(k));
            values.push(value);
        }
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let dict = NSDictionary::from_retained_objects(&key_refs, &values);
        // The result comes back as JSON text, so any value converts the same way.
        let wrapped = format!("return JSON.stringify(await (async () => {{ {body} }})());");
        let content_world = match world {
            Some(name) => unsafe { WKContentWorld::worldWithName(&ns(name), mtm()) },
            None => unsafe { WKContentWorld::pageWorld(mtm()) },
        };
        let (tx, rx) = futures_channel::oneshot::channel::<Result<serde_json::Value, String>>();
        let tx = Cell::new(Some(tx));
        let block = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
            let Some(tx) = tx.take() else { return };
            let out = match unsafe { error.as_ref() } {
                Some(e) => Err(e.localizedDescription().to_string()),
                None => Ok(unsafe { result.as_ref() }
                    .and_then(|r| r.downcast_ref::<NSString>())
                    .and_then(|s| serde_json::from_str(&s.to_string()).ok())
                    .unwrap_or(serde_json::Value::Null)),
            };
            let _ = tx.send(out);
        });
        unsafe {
            self.0.wk.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                &ns(&wrapped),
                Some(&dict),
                None,
                &content_world,
                Some(&block),
            )
        };
        rx.await.map_err(|_| "script cancelled".to_string())?
    }

    pub fn run_script(&self, body: &'static str, args: serde_json::Value, world: Option<&'static str>) {
        let v = self.clone();
        glib::spawn_future_local(async move {
            let _ = v.call_script(body, &args, world).await;
        });
    }

    pub fn add_script_handler(
        &self,
        world: &str,
        channel: &str,
        f: impl Fn(&str) + 'static,
    ) -> Option<Handler> {
        let handler = mtm()
            .alloc::<ScriptHandler>()
            .set_ivars(Box::new(f) as Box<dyn Fn(&str)>);
        let handler: Retained<ScriptHandler> = unsafe { msg_send![super(handler), init] };
        let controller = unsafe { self.0.wk.configuration().userContentController() };
        unsafe {
            controller.addScriptMessageHandler_contentWorld_name(
                ProtocolObject::from_ref(&*handler),
                &WKContentWorld::worldWithName(&ns(world), mtm()),
                &ns(channel),
            )
        };
        Some(Handler(next_id()))
    }

    pub fn remove_script_handler(&self, world: &str, channel: &str, _handler: Handler) {
        let controller = unsafe { self.0.wk.configuration().userContentController() };
        unsafe {
            controller.removeScriptMessageHandlerForName_contentWorld(
                &ns(channel),
                &WKContentWorld::worldWithName(&ns(world), mtm()),
            )
        };
    }

    // --- events
    pub fn disconnect(&self, h: Handler) {
        let i = &self.0;
        i.title.remove(h.0);
        i.uri.remove(h.0);
        i.loading.remove(h.0);
        i.progress.remove(h.0);
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
    /// No favicons on this engine: never fires.
    pub fn connect_favicon(&self, _f: impl Fn(&View) + 'static) -> Handler {
        Handler(next_id())
    }
    pub fn connect_uri(&self, f: impl Fn(&View) + 'static) -> Handler {
        self.on(&self.0.uri, f)
    }
    pub fn connect_loading(&self, f: impl Fn(&View) + 'static) -> Handler {
        self.on(&self.0.loading, f)
    }
    pub fn connect_progress(&self, f: impl Fn(&View) + 'static) -> Handler {
        self.on(&self.0.progress, f)
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
    pub fn connect_response(&self, f: impl Fn(&Response) -> bool + 'static) {
        self.0.response.add(next_id(), Rc::new(f));
    }
    pub fn connect_permission(&self, f: impl Fn(&View, PermissionRequest) + 'static) {
        self.0.permission.add(next_id(), Rc::new(f));
    }
    /// No public hover API: never fires.
    pub fn connect_hover_link(&self, _f: impl Fn(Option<String>) + 'static) {}
    pub fn connect_find_result(&self, f: impl Fn(u32) + 'static) {
        self.0.find_result.add(next_id(), Rc::new(f));
    }
    pub fn on_destroy(&self, f: impl FnOnce() + 'static) {
        self.0.destroyed.borrow_mut().push(Box::new(f));
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
            inner.wk.setHidden(true);
        }
    });
    let weak = Rc::downgrade(inner);
    inner.placeholder.connect_unrealize(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.wk.removeFromSuperview();
        }
    });
}

/// Position the WKWebView over the placeholder, in the window it is in.
fn place(inner: &Inner, p: &gtk::DrawingArea) {
    let wk = &inner.wk;
    let Some(root) = p.root() else { return };
    let Some(native) = root.native() else { return };
    let Some(surface) = native.surface() else { return };
    let Ok(surface) = surface.downcast::<gdk4_macos::MacosSurface>() else { return };
    let ptr = surface.native() as *mut NSWindow;
    let Some(window) = (unsafe { ptr.as_ref() }) else { return };
    let Some(content) = window.contentView() else { return };
    let Some(bounds) = p.compute_bounds(&root) else { return };
    // GTK lays out in the window's coordinates including its CSD shadow;
    // translate by the surface offset of the native widget.
    let (ox, oy) = native.surface_transform();
    let (x, y, w, h) = (
        bounds.x() as f64 + ox,
        bounds.y() as f64 + oy,
        bounds.width() as f64,
        bounds.height() as f64,
    );
    let ch = content.frame().size.height;
    let origin_y = if content.isFlipped() { y } else { ch - y - h };
    let frame = NSRect::new(NSPoint::new(x, origin_y), NSSize::new(w, h));
    let attached = unsafe { wk.superview() }.is_some_and(|s| &*s == &*content);
    if !attached {
        wk.removeFromSuperview();
        content.addSubview(wk);
    }
    if wk.frame() != frame {
        wk.setFrame(frame);
    }
    if wk.isHidden() && p.is_mapped() {
        wk.setHidden(false);
    }
    let _: &NSView = wk;
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

struct DownloadInner {
    wk: Retained<WKDownload>,
    view: RefCell<Weak<Inner>>,
    destination_fn: RefCell<Option<Box<dyn Fn(&str) -> PathBuf>>>,
    destination: RefCell<Option<PathBuf>>,
    created: Slot<dyn Fn(&Path)>,
    progress: Slot<dyn Fn(f64, u64)>,
    end: Slot<dyn Fn(&Download, DownloadEnd)>,
    timer: RefCell<Option<glib::SourceId>>,
}

thread_local! {
    /// Live downloads by WKDownload pointer, so delegate callbacks find their wrapper.
    static DOWNLOADS: RefCell<Vec<Download>> = const { RefCell::new(Vec::new()) };
}

impl Download {
    fn of(wk: &WKDownload) -> Download {
        DOWNLOADS.with(|all| {
            let mut all = all.borrow_mut();
            if let Some(d) = all.iter().find(|d| std::ptr::eq(&*d.0.wk, wk)) {
                return d.clone();
            }
            let d = Download(Rc::new(DownloadInner {
                wk: wk.retain(),
                view: RefCell::new(Weak::new()),
                destination_fn: RefCell::default(),
                destination: RefCell::default(),
                created: Slot::default(),
                progress: Slot::default(),
                end: Slot::default(),
                timer: RefCell::default(),
            }));
            // WKDownload's progress is an NSProgress; sample it.
            let weak = Rc::downgrade(&d.0);
            let id = glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                let Some(inner) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let p = inner.wk.progress();
                let (done, total) = (p.completedUnitCount().max(0) as u64, p.totalUnitCount());
                let fraction = if total > 0 { done as f64 / total as f64 } else { 0.0 };
                for f in inner.progress.all() {
                    f(fraction, done);
                }
                glib::ControlFlow::Continue
            });
            d.0.timer.replace(Some(id));
            all.push(d.clone());
            d
        })
    }

    fn end(&self, end: DownloadEnd) {
        if let Some(id) = self.0.timer.take() {
            id.remove();
        }
        for f in self.0.end.all() {
            f(self, end.clone());
        }
        DOWNLOADS.with(|all| all.borrow_mut().retain(|d| !Rc::ptr_eq(&d.0, &self.0)));
    }

    pub fn source(&self) -> Option<String> {
        unsafe { self.0.wk.originalRequest() }
            .and_then(|r| r.URL())
            .and_then(|u| u.absoluteString())
            .map(|s| s.to_string())
    }
    pub fn view(&self) -> Option<View> {
        self.0.view.borrow().upgrade().map(View)
    }
    pub fn destination(&self) -> Option<PathBuf> {
        self.0.destination.borrow().clone()
    }
    pub fn received_bytes(&self) -> u64 {
        self.0.wk.progress().completedUnitCount().max(0) as u64
    }
    pub fn cancel(&self) {
        unsafe { self.0.wk.cancel(None) };
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

#[allow(dead_code)]
fn _unused(_: &NSArray<NSString>) {}
