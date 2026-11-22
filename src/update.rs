//! Asks GitHub for the latest Mallow release. Mallow only reports and links
//! to a newer release; it never downloads or runs anything itself, because
//! release binaries are not signed.

use gtk::{gio, glib};
use soup::prelude::*;

pub const REPO: &str = env!("CARGO_PKG_REPOSITORY");

pub enum Outcome {
    Newer { version: String, url: String },
    UpToDate,
    NoReleases,
    Failed(String),
}

fn api_url() -> Option<String> {
    let path = REPO.strip_prefix("https://github.com/")?.trim_end_matches('/');
    Some(format!("https://api.github.com/repos/{path}/releases/latest"))
}

/// `tag_name` from a GitHub release JSON body. Only plain version-like tags
/// are accepted, since the value ends up in the UI and a URL.
pub fn parse_tag(json: &str) -> Option<String> {
    let rest = &json[json.find("\"tag_name\"")? + "\"tag_name\"".len()..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let tag = &rest[..rest.find('"')?];
    let ok = !tag.is_empty()
        && tag.len() <= 64
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-+_".contains(c));
    ok.then(|| tag.to_string())
}

fn numbers(v: &str) -> Option<Vec<u64>> {
    let v = v.trim_start_matches(['v', 'V']);
    // Pre-releases ("1.2.0-beta") never count as updates.
    if v.contains('-') {
        return None;
    }
    let core = v.split('+').next()?;
    core.split('.').map(|p| p.parse().ok()).collect()
}

/// Is release `tag` newer than `current` (both dotted numbers)?
pub fn is_newer(tag: &str, current: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (numbers(tag), numbers(current)) else {
        return false;
    };
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

/// One HTTPS request on the GTK main loop; `done` runs when it finishes.
pub fn check(done: impl FnOnce(Outcome) + 'static) {
    let Some(url) = api_url() else {
        done(Outcome::Failed("no GitHub repository configured".into()));
        return;
    };
    let Ok(msg) = soup::Message::new("GET", &url) else {
        done(Outcome::Failed("bad release address".into()));
        return;
    };
    if let Some(h) = msg.request_headers() {
        h.append("Accept", "application/vnd.github+json");
    }
    let session = soup::Session::new();
    session.set_user_agent(concat!("Mallow/", env!("CARGO_PKG_VERSION")));
    session.set_timeout(15);
    let m = msg.clone();
    session.send_and_read_async(&msg, glib::Priority::LOW, None::<&gio::Cancellable>, move |res| {
        let outcome = match res {
            Err(e) => Outcome::Failed(e.message().to_string()),
            Ok(_) if m.status() == soup::Status::NotFound => Outcome::NoReleases,
            Ok(_) if m.status() != soup::Status::Ok => {
                Outcome::Failed(format!("GitHub answered {:?}", m.status()))
            }
            Ok(body) => match parse_tag(&String::from_utf8_lossy(&body)) {
                Some(tag) if is_newer(&tag, env!("CARGO_PKG_VERSION")) => Outcome::Newer {
                    url: format!("{REPO}/releases/tag/{tag}"),
                    version: tag.trim_start_matches(['v', 'V']).to_string(),
                },
                Some(_) => Outcome::UpToDate,
                None => Outcome::Failed("unexpected reply from GitHub".into()),
            },
        };
        done(outcome);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("v0.2.0-beta", "0.1.0"));
        assert!(!is_newer("latest", "0.1.0"));
    }

    #[test]
    fn tags() {
        let body = r#"{"url":"x","html_url":"y","tag_name": "v0.2.0","name":"Mallow 0.2"}"#;
        assert_eq!(parse_tag(body).as_deref(), Some("v0.2.0"));
        assert_eq!(parse_tag(r#"{"tag_name":"v1\"><script>"}"#), None);
        assert_eq!(parse_tag(r#"{"tag_name":"../../x"}"#), None);
        assert_eq!(parse_tag("{}"), None);
        assert_eq!(
            api_url().unwrap(),
            "https://api.github.com/repos/arcbaseproject/Mallow/releases/latest"
        );
    }
}
