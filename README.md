<p align="center">
  <img src="assets/icons/dev.mallow.Browser.svg" width="96" alt="">
</p>

<h1 align="center">Mallow</h1>

<p align="center">A small, fast Linux browser built on GTK 4 and WebKitGTK, styled after Material 3.</p>

Mallow gives you tabs, bookmarks, private windows and tracker blocking in a 1 MB binary. It renders pages with the WebKitGTK engine your distribution already ships and updates, so you get engine security fixes from your package manager.

It sends nothing home: no telemetry, no accounts, no sync, no background services. The only request Mallow makes on its own is the start-up update check, and you can turn it off.

## Install

Mallow runs on Linux. It needs GTK 4.20 or later and WebKitGTK 6.0 2.50 or later. It has no macOS or Windows builds, because WebKitGTK only supports Linux and BSD.

### Installer

```sh
curl -fsSL https://raw.githubusercontent.com/arcbaseproject/Mallow/main/install.sh | sh
```

The installer suggests the package that suits your distribution and checks the download against the release's checksums. It asks before running anything with `sudo`, and it can install into `~/.local` without `sudo`. To pin a version, run `MALLOW_TAG=v0.1.0 sh install.sh`. You can [read the script](install.sh) before you run it.

### Packages

Each [GitHub release](https://github.com/arcbaseproject/Mallow/releases) has builds for x86_64:

- **Debian and Ubuntu:** the `.deb`, built on Ubuntu 26.04. Install it with `sudo apt install ./mallow_*.deb`.
- **Fedora:** the `.rpm`, built on current Fedora. Install it with `sudo dnf install ./mallow-*.rpm`.
- **Other distributions:** the `mallow-*-linux-x86_64.tar.gz` tarball. Install GTK 4 and WebKitGTK 6.0 from your distribution first. Then unpack the tarball and copy its `bin` and `share` folders into `~/.local` or `/usr/local`.

`SHA256SUMS` lists the checksums of all three files. The packages are not signed.

### From source

You need Rust, GTK 4 and WebKitGTK 6.0. On Void Linux:

```sh
sudo xbps-install -S rust cargo pkg-config gtk4-devel libwebkitgtk60-devel sqlite-devel libsodium-devel
```

For audio and video, add `gst-plugins-good1 gst-plugins-bad1 gst-libav`.

Then build and start it:

```sh
./run.sh                         # builds if needed, then launches
./run.sh https://example.org     # open a URL
```

`run.sh` calls `cargo build --release` and runs `target/release/mallow`. A second launch opens a new window in the running browser.

Mallow runs on X11 and Wayland. It works on window managers like dwm, i3, sway and Hyprland, and on full desktops like GNOME, KDE and XFCE.

## What you get

- **Tabs** with favicons. Middle-click a tab to close it, right-click it to reload, duplicate, copy its URL or close it. Ctrl+Shift+T brings back a closed tab.
- **A new-tab page** with a greeting, a search box and your first eight bookmarks as tiles. Mallow draws it with GTK, so an empty tab starts no WebKit process at all.
- **Session restore.** Restored tabs load the first time you open them.
- **An address bar** that takes URLs or searches. Click the star to bookmark a page and rename it on the spot. Click the lock to see the connection, certificate, tracker and cookie status.
- **Private windows** with their own in-memory session and their own downloads list. Closing the window discards everything.
- **Tracker blocking** through WebKit's built-in content blocker.
- **Permission prompts** for camera, microphone, location, notifications and similar requests. Mallow asks you on each request. For a site that wants cookies while embedded in another site, Mallow remembers your answer for that pair of sites until you close the window, and the lock panel lists it.
- **A history page** at `mallow://history` (Ctrl+H): visits grouped by day, search, remove single entries or clear everything.
- **Find in page, zoom, downloads** with progress, and light, dark or system themes with six accent colours.

### Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl+L, Alt+D, F6 | Focus the address bar |
| Ctrl+T, Ctrl+W | New tab, close tab |
| Ctrl+Shift+T | Reopen closed tab |
| Ctrl+Tab, Ctrl+Shift+Tab | Next, previous tab |
| Ctrl+R, F5 | Reload |
| Alt+Left, Alt+Right | Back, forward |
| Ctrl+F | Find in page (Enter for next, Shift+Enter for previous, Esc to close) |
| Ctrl+D | Bookmark or edit bookmark |
| Ctrl+I | Site and connection info |
| Ctrl+Plus, Ctrl+Minus, Ctrl+0 | Zoom in, out, reset |
| Ctrl+H, Ctrl+Shift+O, Ctrl+Shift+Y | History, bookmarks, downloads |
| Ctrl+N, Ctrl+Shift+P | New window, new private window |
| Ctrl+, | Settings |
| Ctrl+Q | Quit |

## Settings

Open Settings from the menu, with Ctrl+, or by typing `mallow://settings` in the address bar. It has seven sections:

| Section | What you can change |
|---|---|
| Appearance | Light, dark or system theme, accent colour (six presets or any colour), compact mode, hiding the tab bar with one tab, animations |
| General | Startup (new tab, homepage, or last session), homepage, what new tabs open, search engine or a custom search address, asking before closing several tabs |
| Privacy & security | Clearing history, cookies, cache or everything, tracker blocking, Intelligent Tracking Prevention, cookie policy, blocking camera, microphone, location or notifications outright |
| Performance | Unloading inactive tabs after a set time, instant back and forward, low memory mode, autoplay, hardware acceleration, smooth scrolling, WebGL, plus version and tab diagnostics |
| Downloads | Download folder, opening it, clearing finished downloads |
| Languages & websites | Languages websites should use, spellchecking and dictionaries, default zoom |
| About | Version, engine versions, update check on start (on or off), a Check now button, license and source link |

Changes apply at once and save to `~/.config/mallow/config.toml`. You can edit that file by hand too; Mallow checks every value when it starts. If the file has a syntax error, Mallow moves it to `config.toml.invalid`, starts with defaults and tells you on the Settings page. Mallow never overwrites a file it couldn't read.

Web pages can't open `mallow://settings`. Mallow ignores the link, the redirect or the pop-up.

To use your own tracker list, put one domain per line in `~/.config/mallow/blocklist.txt`. It replaces the bundled list in `assets/blocklist.txt`.

Mallow keeps history, bookmarks, cookies and site data in `~/.local/share/mallow` and its cache in `~/.cache/mallow`.

### On window managers without a compositor

Plain X11 can't draw transparent pixels without a compositor. Mallow detects this, drops menu shadows and fills the area behind rounded corners with the page colour. Run a compositor such as `picom` if you want real shadows.

## Performance

We measured a release build in a headless sway session with software rendering. On a GPU your numbers will differ. PSS counts shared memory once, so it gives a fairer total than RSS.

| Scenario | Browser PSS | WebKit processes | WebKit PSS |
|---|---|---|---|
| Empty window | 59 MiB | 0 | 0 |
| One simple page | 86 MiB | 2 | 109 MiB |
| Plus a Wikipedia article | 92 MiB | 3 | 248 MiB |
| Ten tabs | 98 MiB | 11 | 306 MiB |

The window maps 90 to 145 ms after launch with a warm disk cache. Left idle with a page open, Mallow and its WebKit processes used no measurable CPU over ten seconds.

WebKit gives each tab its own web process. That process isolation costs memory, and Mallow keeps it.

## Updates

When a newer release appears on GitHub, Mallow shows a toast in the bottom-left corner. Update opens the release page. Mallow never downloads or runs anything itself, because release builds aren't signed. To update a source checkout, run `git pull` and then `./run.sh`.

## Privacy

Mallow contacts the sites you visit and the search engine you configure (DuckDuckGo by default). When it starts, it also asks GitHub's API for the latest Mallow release, sending only its version in the User-Agent. Turn that off in Settings › About. It makes no other requests.

A private window keeps no history or cookies once you close it. Sites, your network provider and your employer can still see your traffic.

## Known limitations

- Settings has no HTTPS-only mode, "ask where to save" downloads or default-browser setup yet.
- Mallow asks for most permissions every time, and forgets embedded-cookie answers when you close the window.
- The bundled tracker list holds about 40 domains, and pages that load in the first moment after startup may slip past it.
- Quitting cancels running downloads without asking.
- Mallow saves your session on quit and on SIGTERM, SIGINT or SIGHUP. A crash or SIGKILL loses changes since the last save.
- Mallow approximates the Material palette with HSL tones, so contrast comes close to Material's HCT values without matching them.

## Documentation

- [CONTRIBUTING.md](CONTRIBUTING.md): build, test and send changes
- [SECURITY.md](SECURITY.md): the security model and how to report a vulnerability
- [LICENSE](LICENSE): MIT

The icons come from Google's [Material Symbols](https://github.com/google/material-design-icons) (Rounded) under the Apache 2.0 license.

Passwords are managed in **Settings → Passwords**. See [Password storage and autofill](PASSWORDS.md) for the vault format, browser integration, and recovery limits.
