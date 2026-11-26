//! Asks GitHub for the latest Mallow release and, on Linux, installs it.
//! An install runs only after the release's SHA256SUMS verifies against
//! RELEASE_KEY and the downloaded package matches its line in that file.

use gtk::{gio, glib, prelude::*};
use soup::prelude::*;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub const REPO: &str = env!("CARGO_PKG_REPOSITORY");

/// Ed25519 public key whose private half (the MALLOW_SIGNING_KEY secret)
/// signs each release's SHA256SUMS in release.yml.
const RELEASE_KEY: &str = "ffc57d3279a90dba4cbec5a4e9a6f3a5d97cd14222940ef3dbb118e4264717ac";

// libsodium; passwords::vault links it.
unsafe extern "C" {
    fn sodium_init() -> i32;
    fn crypto_sign_verify_detached(sig: *const u8, m: *const u8, mlen: u64, pk: *const u8) -> i32;
}

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

fn hex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Is `sig` a valid Ed25519 signature of `msg` by the hex public `key`?
fn signed(msg: &[u8], sig: &[u8], key: &str) -> bool {
    let Some(pk) = hex(key) else { return false };
    // Lengths are checked before libsodium reads the fixed-size buffers.
    sig.len() == 64
        && pk.len() == 32
        && unsafe {
            sodium_init() >= 0
                && crypto_sign_verify_detached(sig.as_ptr(), msg.as_ptr(), msg.len() as u64, pk.as_ptr()) == 0
        }
}

/// The hash `sha256sum` listed for `file`.
fn sum_for<'a>(sums: &'a str, file: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let (hash, name) = l.split_once(' ')?;
        (name.trim_start_matches([' ', '*']) == file).then_some(hash)
    })
}

#[derive(Clone, Copy)]
enum Kind {
    Deb,
    Rpm,
    Tarball,
}

/// How the running copy was installed; None if not from a release package.
fn kind(exe: &Path) -> Option<Kind> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return None;
    }
    if exe == Path::new("/usr/bin/mallow") {
        let deb = Path::new("/var/lib/dpkg/info/mallow.list").exists();
        return Some(if deb { Kind::Deb } else { Kind::Rpm });
    }
    // install.sh's tarball layout: <prefix>/bin/mallow beside <prefix>/share.
    let prefix = exe.parent()?.parent()?;
    let desktop = prefix.join("share/applications/dev.mallow.Browser.desktop");
    (exe.file_name()? == "mallow" && desktop.exists()).then_some(Kind::Tarball)
}

pub fn can_install() -> bool {
    std::env::current_exe().ok().as_deref().and_then(kind).is_some()
}

/// Download, verify and install the release at `release_url` (its
/// /releases/tag/ page); `done` gets the installed executable.
pub fn install(release_url: &str, version: &str, done: impl FnOnce(Result<PathBuf, String>) + 'static) {
    let base = release_url.replacen("/releases/tag/", "/releases/download/", 1);
    let version = version.to_string();
    glib::spawn_future_local(async move { done(install_async(&base, &version).await) });
}

async fn fetch(session: &soup::Session, url: &str) -> Result<glib::Bytes, String> {
    let msg = soup::Message::new("GET", url).map_err(|_| "bad download address".to_string())?;
    let body = session
        .send_and_read_future(&msg, glib::Priority::LOW)
        .await
        .map_err(|e| e.message().to_string())?;
    if msg.status() != soup::Status::Ok {
        return Err(format!("GitHub answered {:?} for {url}", msg.status()));
    }
    Ok(body)
}

async fn install_async(base: &str, version: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let kind = kind(&exe).ok_or("this copy wasn't installed from a release package")?;
    let file = match kind {
        Kind::Deb => format!("mallow_{version}-1_amd64.deb"),
        Kind::Rpm => format!("mallow-{version}-1.x86_64.rpm"),
        Kind::Tarball => format!("mallow-{version}-linux-x86_64.tar.gz"),
    };
    let session = soup::Session::new();
    session.set_user_agent(concat!("Mallow/", env!("CARGO_PKG_VERSION")));
    let sums = fetch(&session, &format!("{base}/SHA256SUMS")).await?;
    let sig = fetch(&session, &format!("{base}/SHA256SUMS.sig")).await?;
    if !signed(&sums, &sig, RELEASE_KEY) {
        return Err("the release signature is invalid".into());
    }
    let sums = std::str::from_utf8(&sums).map_err(|_| "unreadable checksum list")?;
    let want = sum_for(sums, &file).ok_or_else(|| format!("{file} is not in the signed checksum list"))?;
    let data = fetch(&session, &format!("{base}/{file}")).await?;
    let got = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, &data);
    if got.as_deref() != Some(want) {
        return Err("the download doesn't match the signed checksum".into());
    }
    let dir = glib::mkdtemp("mallow-update-XXXXXX").ok_or("no temporary folder")?;
    let result = place(kind, &dir, &file, &data, &exe, version).await;
    let _ = std::fs::remove_dir_all(&dir);
    result.map(|()| exe)
}

async fn place(
    kind: Kind,
    dir: &Path,
    file: &str,
    data: &[u8],
    exe: &Path,
    version: &str,
) -> Result<(), String> {
    let pkg = dir.join(file);
    std::fs::write(&pkg, data).map_err(|e| e.to_string())?;
    let pkg = pkg.into_os_string();
    let args = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
    let argv = match kind {
        Kind::Deb => [args(&["pkexec", "apt-get", "install", "-y"]), vec![pkg]].concat(),
        Kind::Rpm if glib::find_program_in_path("dnf").is_some() => {
            [args(&["pkexec", "dnf", "install", "-y"]), vec![pkg]].concat()
        }
        Kind::Rpm => [
            args(&[
                "pkexec",
                "zypper",
                "--non-interactive",
                "install",
                "--allow-unsigned-rpm",
            ]),
            vec![pkg],
        ]
        .concat(),
        Kind::Tarball => {
            let inner = format!("mallow-{version}-linux-x86_64/bin/mallow");
            let mut tar = args(&["tar", "-xzf"]);
            tar.extend([pkg, "-C".into(), dir.into(), inner.clone().into()]);
            run(&tar).await?;
            let writable = gio::File::for_path(exe.parent().ok_or("no install folder")?)
                .query_info(
                    "access::can-write",
                    gio::FileQueryInfoFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .is_ok_and(|i| i.boolean("access::can-write"));
            // install(1) replaces the file, so the running binary is untouched.
            let mut cmd = args(if writable {
                &["install", "-m755"]
            } else {
                &["pkexec", "install", "-m755"]
            });
            cmd.extend([dir.join(inner).into(), exe.into()]);
            cmd
        }
    };
    run(&argv).await
}

async fn run(argv: &[OsString]) -> Result<(), String> {
    let refs: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
    let p = gio::Subprocess::newv(&refs, gio::SubprocessFlags::NONE).map_err(|e| e.message().to_string())?;
    p.wait_future().await.map_err(|e| e.message().to_string())?;
    if p.is_successful() {
        return Ok(());
    }
    // pkexec exits 126 when the password prompt is dismissed.
    if argv[0] == "pkexec" && p.has_exited() && p.exit_status() == 126 {
        return Err("authentication was cancelled".into());
    }
    let cmd = argv.iter().find(|a| *a != "pkexec").unwrap_or(&argv[0]);
    Err(format!("{} failed", cmd.to_string_lossy()))
}

static RESTART: OnceLock<PathBuf> = OnceLock::new();

/// Start `exe` once this process has quit (see main).
pub fn restart_with(exe: PathBuf) {
    let _ = RESTART.set(exe);
}

pub fn relaunch_if_requested() {
    #[cfg(unix)]
    if let Some(exe) = RESTART.get() {
        use std::os::unix::process::CommandExt;
        // exec drops this process's bus name, so the new one starts as the main instance.
        let e = std::process::Command::new(exe).exec();
        eprintln!("Mallow: couldn't restart {}: {e}", exe.display());
    }
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

    #[test]
    fn signatures() {
        // Made with: openssl pkeyutl -sign -rawin (a throwaway key, not RELEASE_KEY).
        let key = "0c20617476f608b1978e9ffc8e780f5649f5c4d84df5050fc2b3efb6feb03892";
        let sig = hex("cfab88582ba1673d20956babbd5225f22e399a2a4cb095eaf0a14534445f81b49491f7bc5841dc665f92b0a4a50c636d9aeabe95bb4dabdca834c1a58240100d").unwrap();
        let msg = b"abc  mallow_9.9.9-1_amd64.deb\n";
        assert!(signed(msg, &sig, key));
        assert!(!signed(b"abd  mallow_9.9.9-1_amd64.deb\n", &sig, key));
        assert!(!signed(msg, &sig, RELEASE_KEY));
        assert!(!signed(msg, &sig[..63], key));
        assert_eq!(hex(RELEASE_KEY).map(|k| k.len()), Some(32));
        let sums = "abc  mallow_9.9.9-1_amd64.deb\ndef *mallow-9.9.9-1.x86_64.rpm\n";
        assert_eq!(sum_for(sums, "mallow_9.9.9-1_amd64.deb"), Some("abc"));
        assert_eq!(sum_for(sums, "mallow-9.9.9-1.x86_64.rpm"), Some("def"));
        assert_eq!(sum_for(sums, "mallow.deb"), None);
        assert!(kind(Path::new("/home/x/Mallow/target/release/mallow")).is_none());
    }
}
