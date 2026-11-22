//! The browser engine boundary. Everything outside this module talks to web
//! content only through these types; each operating system gets a backend that
//! implements them on its native engine. Linux: WebKitGTK; macOS: WKWebView.
//!
//! Backends provide `Engine`, `Session`, `View`, `WeakView`, `Download`,
//! `PermissionRequest`, `SessionState`, `Handler` and `version()` with the
//! same methods. Data passed across the boundary uses the plain types below.

#[cfg(target_os = "linux")]
mod webkitgtk;
#[cfg(target_os = "linux")]
pub use webkitgtk::*;

#[cfg(target_os = "macos")]
mod wkwebview;
#[cfg(target_os = "macos")]
pub use wkwebview::*;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Mallow has engine backends for Linux (WebKitGTK) and macOS (WKWebView) only so far.");

/// Stages of a main-frame navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadEvent {
    Started,
    Redirected,
    Committed,
    Finished,
}

/// Why a page failed to load, for the error page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    NotFound,
    Tls,
    Refused,
    TimedOut,
    Offline,
    Other,
}

/// A navigation the page or the user started, before it happens.
#[derive(Clone, Debug)]
pub struct NavigationAction {
    pub uri: String,
    pub user_gesture: bool,
    pub redirect: bool,
    /// The page asked for a new window (`target=_blank`, `window.open`).
    pub new_window: bool,
    pub link_click: bool,
    /// Middle click or Ctrl+click: open in a background tab.
    pub background: bool,
}

/// A main-frame response, before the engine renders it.
#[derive(Clone, Copy, Debug)]
pub struct Response {
    pub main_resource: bool,
    /// The engine can display this MIME type.
    pub displayable: bool,
    /// The server marked it `Content-Disposition: attachment`.
    pub attachment: bool,
}

/// What a site asks to use.
#[derive(Clone, Debug, PartialEq, Eq)]
// WKWebView only raises camera and microphone requests.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub enum Permission {
    /// Camera and/or microphone; neither means screen capture.
    Media {
        video: bool,
        audio: bool,
    },
    Location,
    Notifications,
    Clipboard,
    PointerLock,
    DeviceInfo,
    MediaKeys,
    /// Third-party cookie access for `requester` while embedded on `current`.
    StorageAccess {
        current: String,
        requester: String,
    },
    Other,
}

/// The server certificate of the page shown.
#[derive(Clone, Debug)]
pub struct Certificate {
    /// Why it isn't trusted; `None` when it is.
    pub problems: Option<String>,
    pub subject: String,
    pub issuer: String,
    pub not_before: Option<gtk::glib::DateTime>,
    pub not_after: Option<gtk::glib::DateTime>,
    /// DER bytes, for the fingerprint.
    pub der: Option<Vec<u8>>,
}

/// Site data to clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataKinds {
    /// Cookies, storage, databases, service workers, permissions.
    pub site_data: bool,
    /// Memory and disk caches.
    pub cache: bool,
}

impl DataKinds {
    pub const NONE: Self = Self {
        site_data: false,
        cache: false,
    };
    pub const ALL: Self = Self {
        site_data: true,
        cache: true,
    };
}

/// How a download ended.
#[derive(Clone, Debug)]
pub enum DownloadEnd {
    Done,
    Cancelled,
    Failed(String),
}
