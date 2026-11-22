//! Pure helpers with no GTK dependency, so they can be unit tested.

use std::path::{Path, PathBuf};

/// Schemes the browser loads itself. Anything else is handed to the desktop
/// after the user confirms.
pub const INTERNAL_SCHEMES: &[&str] = &["http", "https", "file", "about", "data", "blob"];

pub fn scheme(uri: &str) -> Option<&str> {
    let (s, _) = uri.split_once(':')?;
    let ok = !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    ok.then_some(s)
}

/// Turn omnibox text into a URI: a URL as typed, a bare host, or a search.
pub fn resolve_input(input: &str, search_template: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(s) = scheme(text) {
        let s = s.to_ascii_lowercase();
        if matches!(s.as_str(), "http" | "https" | "file") && text.contains("://") {
            return Some(text.to_string());
        }
        if s == "about" {
            return Some(text.to_string());
        }
    }
    if text.starts_with('/') {
        return gtk::glib::filename_to_uri(text, None).ok().map(String::from);
    }
    if !text.contains(char::is_whitespace) && looks_like_host(text) {
        let host = text.split(['/', '?', '#']).next().unwrap_or(text);
        let name = host.split_once(']').map_or_else(
            || host.split(':').next().unwrap_or(host),
            |(ipv6, _)| ipv6.trim_start_matches('['),
        );
        let local = name.eq_ignore_ascii_case("localhost")
            || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
        return Some(format!("{}://{text}", if local { "http" } else { "https" }));
    }
    Some(search_template.replace("{}", &encode_query(text)))
}

fn looks_like_host(text: &str) -> bool {
    let host = text.split(['/', '?', '#']).next().unwrap_or("");
    let name = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    if name.eq_ignore_ascii_case("localhost") || (name.starts_with('[') && name.ends_with(']')) {
        return true;
    }
    let labels: Vec<&str> = name.split('.').collect();
    labels.len() >= 2
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_alphanumeric() || c == '-'))
        && labels
            .last()
            .is_some_and(|tld| tld.chars().any(|c| !c.is_ascii_digit()) || labels.len() == 4)
}

/// application/x-www-form-urlencoded encoding for search queries.
pub fn encode_query(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for b in q.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Host part of a URI, for permission prompts and labels.
pub fn host(uri: &str) -> &str {
    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit_once('@').map_or(authority, |(_, h)| h)
}

/// Value of `key` (e.g. "CN") in an RFC 4514 distinguished name.
pub fn dn_field<'a>(dn: &'a str, key: &str) -> Option<&'a str> {
    dn.split(',')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

/// "ab12cd" -> "AB:12:CD"
pub fn fingerprint(hex: &str) -> String {
    let up = hex.to_ascii_uppercase();
    let pairs: Vec<&str> = (0..up.len())
        .step_by(2)
        .filter_map(|i| up.get(i..i + 2))
        .collect();
    pairs.join(":")
}

/// Letter for a site tile: first letter or digit, uppercased.
pub fn initial(host: &str) -> char {
    host.chars()
        .find(|c| c.is_alphanumeric())
        .map_or('•', |c| c.to_uppercase().next().unwrap_or(c))
}

/// Make a server-suggested filename safe to create in the downloads folder:
/// no directories, no hidden files, no control characters.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .trim_start_matches('.')
        .to_string();
    let mut out = String::new();
    for c in cleaned.chars() {
        if out.len() + c.len_utf8() > 200 {
            break;
        }
        out.push(c);
    }
    if out.is_empty() { "download".into() } else { out }
}

/// `dir/name`, or `dir/stem (n).ext` if that already exists.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// WebKit content-blocker JSON blocking third-party loads from each domain.
/// Lines that are not plain domain names are skipped, so the file cannot
/// inject arbitrary rules.
pub fn block_rules(list: &str) -> String {
    let rules: Vec<String> = list
        .lines()
        .map(str::trim)
        .filter(|d| {
            d.contains('.')
                && !d.starts_with('.')
                && d.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        })
        .map(|d| {
            let esc = d.replace('.', r"\\.");
            format!(
                r#"{{"trigger":{{"url-filter":"^https?://([^/]*\\.)?{esc}[:/]","load-type":["third-party"]}},"action":{{"type":"block"}}}}"#
            )
        })
        .collect();
    format!("[{}]", rules.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    const S: &str = "https://search.example/?q={}";

    #[test]
    fn omnibox() {
        assert_eq!(resolve_input("  ", S), None);
        assert_eq!(resolve_input("https://a.b/c", S).unwrap(), "https://a.b/c");
        assert_eq!(resolve_input("example.com", S).unwrap(), "https://example.com");
        assert_eq!(
            resolve_input("example.com/a b", S).unwrap(),
            "https://search.example/?q=example.com%2Fa+b"
        );
        assert_eq!(
            resolve_input("localhost:8080/x", S).unwrap(),
            "http://localhost:8080/x"
        );
        assert_eq!(resolve_input("127.0.0.1", S).unwrap(), "http://127.0.0.1");
        assert_eq!(
            resolve_input("rust lang", S).unwrap(),
            "https://search.example/?q=rust+lang"
        );
        assert_eq!(
            resolve_input("c++", S).unwrap(),
            "https://search.example/?q=c%2B%2B"
        );
        assert_eq!(resolve_input("1.5", S).unwrap(), "https://search.example/?q=1.5");
        for host in ["localhost.example.org", "127.example.org"] {
            assert_eq!(resolve_input(host, S).unwrap(), format!("https://{host}"));
        }
        assert_eq!(resolve_input("[::1]:8080", S).unwrap(), "http://[::1]:8080");
        assert_eq!(resolve_input("/tmp/a#b?c", S).unwrap(), "file:///tmp/a%23b%3Fc");
        assert_eq!(resolve_input("/etc/hosts", S).unwrap(), "file:///etc/hosts");
        assert_eq!(resolve_input("about:blank", S).unwrap(), "about:blank");
        assert_eq!(
            resolve_input("javascript:alert(1)", S).unwrap(),
            "https://search.example/?q=javascript%3Aalert%281%29"
        );
    }

    #[test]
    fn schemes_and_hosts() {
        assert_eq!(scheme("mailto:x@y"), Some("mailto"));
        assert_eq!(scheme("no scheme"), None);
        assert_eq!(host("https://user@a.b:8/p?q"), "a.b:8");
        assert_eq!(host("https://a.b"), "a.b");
        assert_eq!(initial("github.com"), 'G');
        assert_eq!(initial("[::1]:80"), '1');
        assert_eq!(initial(""), '•');
        assert_eq!(
            dn_field("CN=a.org,O=Let's Encrypt,C=US", "O"),
            Some("Let's Encrypt")
        );
        assert_eq!(dn_field("CN=a.org", "O"), None);
        assert_eq!(fingerprint("ab12cd"), "AB:12:CD");
    }

    #[test]
    fn filenames() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("..\\x.exe"), "x.exe");
        assert_eq!(sanitize_filename(".bashrc"), "bashrc");
        assert_eq!(sanitize_filename("a\u{0}b\nc.txt"), "abc.txt");
        assert_eq!(sanitize_filename(""), "download");
        assert!(sanitize_filename(&"é".repeat(300)).len() <= 200);
    }

    #[test]
    fn unique() {
        let dir = std::env::temp_dir().join(format!("mallow-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (1).txt"));
        assert_eq!(unique_path(&dir, "b"), dir.join("b"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rules() {
        let json = block_rules("ads.example\n# comment\nbad\"inject.com\n");
        assert_eq!(json.matches("trigger").count(), 1);
        assert!(json.contains(r"ads\\.example"));
    }
}
