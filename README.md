<p align="center">
  <img src="assets/icons/dev.mallow.Browser.svg" width="96" alt="">
</p>

<h1 align="center">Mallow</h1>

<p align="center">A small, fast Linux browser built on GTK 4 and WebKitGTK, styled after Material 3.</p>

Mallow gives you tabs, bookmarks, private windows and tracker blocking in a 1 MB binary. It renders pages with the WebKitGTK engine your distribution already ships and updates, so you get engine security fixes from your package manager.

It sends nothing home: no telemetry, no accounts, no sync, no background services.

## Install

You need Rust, GTK 4 and WebKitGTK 6.0. On Void Linux:

```sh
sudo xbps-install -S rust cargo pkg-config gtk4-devel libwebkitgtk60-devel sqlite-devel
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
- **Find in page, zoom, history, downloads** with progress, and light, dark or system themes with six accent colours.

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
| Ctrl+Q | Quit |

## Configuration

Mallow reads `~/.config/mallow/config.toml`. You can leave out any key.

```toml
homepage = ""                              # "" shows the new-tab page
search_url = "https://duckduckgo.com/?q={}"
theme = "system"                           # system, light or dark
accent = "#6750A4"                         # any #rrggbb colour
animations = true
restore_session = true
block_trackers = true
cookies = "no-third-party"                 # no-third-party, always or never
```

You can also change the theme and accent from the menu.

To use your own tracker list, put one domain per line in `~/.config/mallow/blocklist.txt`. It replaces the bundled list in `assets/blocklist.txt`.

Mallow keeps history, bookmarks, cookies and site data in `~/.local/share/mallow` and its cache in `~/.cache/mallow`.

### On window managers without a compositor

Plain X11 can't draw transparent pixels without a compositor. Mallow detects this, drops menu shadows and fills the area behind rounded corners with the page colour. Run a compositor such as `picom` if you want real shadows.

## Performance

We measured a release build in a headless sway session with software rendering. On a GPU your numbers will differ. PSS counts shared memory once, so it gives a fairer total than RSS.

| Scenario | Browser PSS | WebKit processes | WebKit PSS |
|---|---|---|---|
| Empty window | 50 to 59 MiB | 0 | 0 |
| One simple page | 85 MiB | 2 | 109 MiB |
| Plus a Wikipedia article | 92 MiB | 3 | 265 MiB |
| Ten tabs | 97 MiB | 11 | 306 MiB |

The window maps 90 to 180 ms after launch with a warm disk cache. With a page open and idle, Mallow and its WebKit processes used 0 to 1 CPU ticks over ten seconds.

WebKit gives each tab its own web process. That process isolation costs memory, and Mallow keeps it.

## Privacy

Mallow contacts the sites you visit and the search engine you configure (DuckDuckGo by default). It makes no other requests.

A private window keeps no history or cookies once you close it. Sites, your network provider and your employer can still see your traffic.

## Known limitations

- Mallow has no settings window. You set most options in `config.toml`.
- It has no password manager and no address-bar suggestions.
- Loaded tabs stay loaded. Mallow can't suspend them yet.
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
