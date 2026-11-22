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

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// Empty: the built-in new-tab page.
    pub homepage: String,
    /// `{}` is replaced by the encoded query.
    pub search_url: String,
    pub theme: Theme,
    /// Seed colour for the palette, `#rrggbb`.
    pub accent: String,
    pub animations: bool,
    pub restore_session: bool,
    pub block_trackers: bool,
    pub cookies: Cookies,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            homepage: String::new(),
            search_url: "https://duckduckgo.com/?q={}".into(),
            theme: Theme::System,
            accent: "#6750A4".into(),
            animations: true,
            restore_session: true,
            block_trackers: true,
            cookies: Cookies::NoThirdParty,
        }
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
    pub fn load() -> Self {
        read("config.toml").unwrap_or_default()
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
    fn partial_config_keeps_defaults() {
        let c: Config = toml::from_str("theme = \"dark\"\ncookies = \"never\"").unwrap();
        assert_eq!(c.theme, Theme::Dark);
        assert_eq!(c.cookies, Cookies::Never);
        assert!(c.search_url.contains("{}"));
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
