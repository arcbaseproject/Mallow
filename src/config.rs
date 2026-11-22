use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    gtk::glib::user_config_dir().join("mallow")
}
pub fn data_dir() -> PathBuf {
    gtk::glib::user_data_dir().join("mallow")
}
pub fn cache_dir() -> PathBuf {
    gtk::glib::user_cache_dir().join("mallow")
}

/// Address of the built-in settings page. Only typed or trusted UI can open
/// it; web content can never navigate to the `mallow:` scheme.
pub const SETTINGS_URI: &str = "mallow://settings";
pub const HISTORY_URI: &str = "mallow://history";

/// (name, template, suggestions template) presets offered in Settings.
/// Suggestions answer in the OpenSearch JSON form: `["query", ["one", "two"]]`.
pub const SEARCH_ENGINES: &[(&str, &str, &str)] = &[
    ("DuckDuckGo", "https://duckduckgo.com/?q={}", "https://duckduckgo.com/ac/?q={}&type=list"),
    (
        "Google",
        "https://www.google.com/search?q={}",
        "https://suggestqueries.google.com/complete/search?client=firefox&q={}",
    ),
    ("Bing", "https://www.bing.com/search?q={}", "https://api.bing.com/osjson.aspx?query={}"),
    ("Brave Search", "https://search.brave.com/search?q={}", "https://search.brave.com/api/suggest?q={}"),
    ("Startpage", "https://www.startpage.com/do/search?q={}", "https://www.startpage.com/osuggestions?q={}"),
    ("Ecosia", "https://www.ecosia.org/search?q={}", "https://ac.ecosia.org/autocomplete?q={}&type=list"),
];

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Cookies {
    NoThirdParty,
    Always,
    Never,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Startup {
    NewTab,
    Homepage,
    Restore,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum NewTab {
    Page,
    Homepage,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Autoplay {
    Allow,
    NoSound,
    Deny,
}

/// Minutes offered for "unload inactive tabs after".
pub const UNLOAD_CHOICES: &[u32] = &[1, 5, 15, 30, 60, 120];

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Ask {
    Ask,
    Block,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(default)]
pub struct Permissions {
    pub camera: Ask,
    pub microphone: Ask,
    pub location: Ask,
    pub notifications: Ask,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            camera: Ask::Ask,
            microphone: Ask::Ask,
            location: Ask::Ask,
            notifications: Ask::Ask,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub passwords: crate::passwords::Preferences,
    /// Empty: the built-in new-tab page.
    pub homepage: String,
    /// `{}` is replaced by the encoded query.
    pub search_url: String,
    pub theme: Theme,
    /// Seed colour for the palette, `#rrggbb`.
    pub accent: String,
    pub animations: bool,
    pub compact: bool,
    pub hide_single_tab: bool,
    /// What the first window opens. `None` falls back to the older
    /// `restore_session` flag so existing config files keep working.
    pub startup: Option<Startup>,
    #[serde(skip_serializing)]
    pub restore_session: Option<bool>,
    pub new_tab: NewTab,
    pub confirm_close: bool,
    pub default_zoom: f64,
    /// Empty: the XDG Downloads folder.
    pub download_dir: String,
    pub block_trackers: bool,
    /// WebKit's Intelligent Tracking Prevention.
    pub itp: bool,
    pub cookies: Cookies,
    pub permissions: Permissions,
    pub hardware_acceleration: bool,
    pub smooth_scrolling: bool,
    pub webgl: bool,
    /// WebKit's back/forward page cache.
    pub page_cache: bool,
    /// WebKit's document-viewer cache model: smaller caches, no spare
    /// web processes.
    pub low_memory: bool,
    pub autoplay: Autoplay,
    /// Drop the web process of tabs left in the background this long.
    pub unload_tabs: bool,
    /// Minutes; one of `UNLOAD_CHOICES`.
    pub unload_after: u32,
    /// Accept-Language for websites, e.g. "en-US, fr". Empty: system locale.
    pub languages: String,
    pub spellcheck: bool,
    /// Dictionaries, e.g. "en_US". Empty: system locale.
    pub spell_languages: String,
    /// Ask GitHub for a newer release each time Mallow starts.
    pub check_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            passwords: Default::default(),
            homepage: String::new(),
            search_url: SEARCH_ENGINES[0].1.into(),
            theme: Theme::System,
            accent: "#6750A4".into(),
            animations: true,
            compact: false,
            hide_single_tab: false,
            startup: None,
            restore_session: None,
            new_tab: NewTab::Page,
            confirm_close: true,
            default_zoom: 1.0,
            download_dir: String::new(),
            block_trackers: true,
            itp: true,
            cookies: Cookies::NoThirdParty,
            permissions: Permissions::default(),
            hardware_acceleration: true,
            smooth_scrolling: true,
            webgl: true,
            page_cache: true,
            low_memory: false,
            autoplay: Autoplay::NoSound,
            unload_tabs: false,
            unload_after: 30,
            languages: String::new(),
            spellcheck: false,
            spell_languages: String::new(),
            check_updates: true,
        }
    }
}

pub const ZOOM_RANGE: (f64, f64) = (0.3, 3.0);

/// Search templates must be web URLs containing `{}`.
pub fn check_search_url(s: &str) -> Result<(), &'static str> {
    let s = s.trim();
    if !(s.starts_with("https://") || s.starts_with("http://")) {
        return Err("Must start with https://");
    }
    if !s.contains("{}") {
        return Err("Must contain {} where the search terms go");
    }
    if s.contains(char::is_whitespace) {
        return Err("Must not contain spaces");
    }
    if crate::util::host(s).is_empty() {
        return Err("Missing the site address");
    }
    Ok(())
}

pub fn check_homepage(s: &str) -> Result<(), &'static str> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(());
    }
    let web = s.starts_with("https://") || s.starts_with("http://");
    if !(web || s.starts_with("file:///")) || s.contains(char::is_whitespace) {
        return Err("Enter a full address starting with https://");
    }
    if web && crate::util::host(s).is_empty() {
        return Err("Missing the site address");
    }
    Ok(())
}

/// Comma-separated BCP 47 tags such as "en-US, fr".
pub fn parse_languages(s: &str) -> Result<Vec<String>, &'static str> {
    let tag = |t: &str| {
        let mut parts = t.split('-');
        parts
            .next()
            .is_some_and(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphabetic()))
            && parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
    };
    split_list(s, tag, "Use language codes like en-US, fr")
}

/// Comma-separated dictionary names such as "en_US, de_DE".
pub fn parse_dictionaries(s: &str) -> Result<Vec<String>, &'static str> {
    let dict = |t: &str| {
        let mut parts = t.split('_');
        parts
            .next()
            .is_some_and(|p| (2..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_lowercase()))
            && parts.all(|p| (2..=4).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
    };
    split_list(s, dict, "Use dictionary names like en_US")
}

fn split_list(s: &str, ok: impl Fn(&str) -> bool, err: &'static str) -> Result<Vec<String>, &'static str> {
    let items: Vec<String> = s
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect();
    if items.iter().all(|t| ok(t)) {
        Ok(items)
    } else {
        Err(err)
    }
}

fn read<T: for<'de> Deserialize<'de>>(name: &str) -> Option<T> {
    let path = config_dir().join(name);
    let text = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&text)
        .map_err(|e| eprintln!("mallow: ignoring invalid {}: {e}", path.display()))
        .ok()
}

/// Write via a temp file and rename so a crash never leaves a truncated file.
fn write<T: Serialize>(name: &str, value: &T) {
    let dir = config_dir();
    let result = toml::to_string_pretty(value)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let tmp = dir.join(format!(".{name}.tmp"));
            std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, dir.join(name)).map_err(|e| e.to_string())
        });
    if let Err(e) = result {
        eprintln!("mallow: could not save {name}: {e}");
    }
}

impl Config {
    /// Loads `config.toml`. A file that fails to parse is moved aside to
    /// `config.toml.invalid` before defaults are used, so a later save can
    /// never destroy it; invalid values are reset individually. Either case
    /// returns a message for the Settings page.
    pub fn load() -> (Self, Option<String>) {
        let path = config_dir().join("config.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return (Self::default(), None);
        };
        match toml::from_str::<Self>(&text) {
            Ok(mut cfg) => {
                let fixed = cfg.sanitize();
                let msg =
                    (!fixed.is_empty()).then(|| format!("Reset invalid settings: {}", fixed.join(", ")));
                if let Some(m) = &msg {
                    eprintln!("mallow: {m}");
                }
                (cfg, msg)
            }
            Err(e) => {
                let backup = path.with_extension("toml.invalid");
                let kept = std::fs::rename(&path, &backup).is_ok();
                eprintln!("mallow: invalid {}: {e}", path.display());
                let msg = if kept {
                    format!(
                        "Your settings file had an error and was saved as {}. Defaults are in use.",
                        backup.display()
                    )
                } else {
                    format!(
                        "Your settings file has an error ({}). Defaults are in use.",
                        path.display()
                    )
                };
                (Self::default(), Some(msg))
            }
        }
    }

    /// Replace invalid values with defaults; returns the names fixed.
    pub fn sanitize(&mut self) -> Vec<&'static str> {
        let d = Self::default();
        let mut fixed = Vec::new();
        if check_search_url(&self.search_url).is_err() {
            self.search_url = d.search_url;
            fixed.push("search_url");
        }
        if check_homepage(&self.homepage).is_err() {
            self.homepage = d.homepage;
            fixed.push("homepage");
        }
        if crate::theme::parse_hex(&self.accent).is_none() {
            self.accent = d.accent;
            fixed.push("accent");
        }
        if !(ZOOM_RANGE.0..=ZOOM_RANGE.1).contains(&self.default_zoom) {
            self.default_zoom = d.default_zoom;
            fixed.push("default_zoom");
        }
        if parse_languages(&self.languages).is_err() {
            self.languages = d.languages;
            fixed.push("languages");
        }
        if parse_dictionaries(&self.spell_languages).is_err() {
            self.spell_languages = d.spell_languages;
            fixed.push("spell_languages");
        }
        if !UNLOAD_CHOICES.contains(&self.unload_after) {
            self.unload_after = d.unload_after;
            fixed.push("unload_after");
        }
        if !self.download_dir.is_empty() && !std::path::Path::new(&self.download_dir).is_absolute() {
            self.download_dir = d.download_dir;
            fixed.push("download_dir");
        }
        fixed
    }

    pub fn startup(&self) -> Startup {
        self.startup.unwrap_or(if self.restore_session == Some(false) {
            Startup::Homepage
        } else {
            Startup::Restore
        })
    }

    /// The download folder to use now: the configured one if it still
    /// exists, otherwise XDG Downloads (or home).
    pub fn download_dir(&self) -> PathBuf {
        let custom = PathBuf::from(&self.download_dir);
        if !self.download_dir.is_empty() && custom.is_dir() {
            return custom;
        }
        gtk::glib::user_special_dir(gtk::glib::UserDirectory::Downloads).unwrap_or_else(gtk::glib::home_dir)
    }

    pub fn save(&self) {
        write("config.toml", self);
    }
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct SessionTab {
    pub url: String,
    pub title: String,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Session {
    pub active: usize,
    pub tabs: Vec<SessionTab>,
}

impl Session {
    pub fn load() -> Option<Self> {
        read("session.toml")
    }
    pub fn save(&self) {
        write("session.toml", self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_preferences_round_trip() {
        let mut c = Config::default();
        let p = &mut c.passwords;
        (
            p.save,
            p.fill,
            p.upper,
            p.lower,
            p.digits,
            p.symbols,
            p.avoid_ambiguous,
        ) = (false, false, false, true, false, true, false);
        (p.lock_minutes, p.length) = (0, 64);
        p.excluded = vec!["https://a.test".into(), "https://b.test:8443".into()];
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(format!("{:?}", back.passwords), format!("{:?}", c.passwords));
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let c: Config = toml::from_str("theme = \"dark\"\ncookies = \"never\"").unwrap();
        assert_eq!(c.theme, Theme::Dark);
        assert_eq!(c.cookies, Cookies::Never);
        assert!(c.search_url.contains("{}"));
        assert_eq!(c.permissions.camera, Ask::Ask);
        assert_eq!(c.autoplay, Autoplay::NoSound);
        assert!(!c.unload_tabs);
    }

    #[test]
    fn legacy_restore_flag() {
        let c: Config = toml::from_str("restore_session = false").unwrap();
        assert_eq!(c.startup(), Startup::Homepage);
        assert_eq!(Config::default().startup(), Startup::Restore);
        let c: Config = toml::from_str("restore_session = false\nstartup = \"new-tab\"").unwrap();
        assert_eq!(c.startup(), Startup::NewTab);
        // the legacy key is read but never written back
        assert!(!toml::to_string(&c).unwrap().contains("restore_session"));
    }

    #[test]
    fn sanitize_resets_only_bad_values() {
        let mut c: Config = toml::from_str(
            "unload_after = 7\nsearch_url = \"ftp://x\"\naccent = \"red\"\ndefault_zoom = 9.0\nlanguages = \"en-US, fr\"\nhomepage = \"https://a.org\"",
        )
        .unwrap();
        let fixed = c.sanitize();
        assert_eq!(
            fixed,
            vec!["search_url", "accent", "default_zoom", "unload_after"]
        );
        assert_eq!(c.homepage, "https://a.org");
        assert_eq!(c.languages, "en-US, fr");
    }

    #[test]
    fn validators() {
        assert!(check_search_url("https://x.org/?q={}").is_ok());
        assert!(check_search_url("https://x.org/?q=").is_err());
        assert!(check_search_url("javascript:{}").is_err());
        assert!(check_search_url("https://x.org/?q={} x").is_err());
        assert!(check_homepage("").is_ok());
        assert!(check_homepage("https://a.b").is_ok());
        assert!(check_homepage("a.b").is_err());
        assert!(check_homepage("javascript:alert(1)").is_err());
        assert_eq!(parse_languages("en-US, fr").unwrap(), vec!["en-US", "fr"]);
        assert!(parse_languages("en US").is_err());
        assert_eq!(parse_dictionaries("en_US,de_DE").unwrap(), vec!["en_US", "de_DE"]);
        assert!(parse_dictionaries("../x").is_err());
        assert!(parse_languages("").unwrap().is_empty());
    }

    #[test]
    fn session_roundtrip() {
        let s = Session {
            active: 1,
            tabs: vec![SessionTab {
                url: "https://a".into(),
                title: "A".into(),
            }],
        };
        let back: Session = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.tabs[0].url, "https://a");
    }
}
