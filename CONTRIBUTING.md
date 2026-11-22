# Contributing to Mallow

Thanks for helping. This guide covers the build, the checks we run and the habits that keep Mallow small.

## Set up

Install the toolchain and libraries listed in the [README](README.md#install), then:

```sh
cargo build
cargo test
./target/debug/mallow
```

## Before you open a pull request

Run all four and fix what they report:

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
cargo build --release
```

Clippy should report nothing. If you think a lint is wrong, say so in the PR. Don't silence it with `#[allow]` unless the code has a real reason, and write that reason in a comment next to the attribute.

Then open the browser and try the feature you changed. Tell us in the PR what you tested and how, and say plainly what you couldn't test.

## How the code fits together

| File | Job |
|---|---|
| `src/main.rs` | Starts the GTK application and handles signals |
| `src/app.rs` | App-wide state: config, theme, WebKit sessions, content filter, shortcuts |
| `src/window.rs` | One browser window: tab strip, toolbar, new-tab page, popovers |
| `src/view.rs` | Creates WebViews and decides navigation, download and permission policy |
| `src/downloads.rs` | Download destinations and the downloads window |
| `src/store.rs` | History and bookmarks in SQLite, written from a background thread |
| `src/config.rs` | `config.toml` and the saved session |
| `src/theme.rs` | Turns one accent colour into Material colour roles |
| `src/util.rs` | Pure helpers with unit tests: URL parsing, filenames, blocklist rules |
| `assets/style.css` | All styling. Use `var(--role)` colours, never hex values |

Put logic you can test without a display in `util.rs` or `theme.rs`, and add a unit test with it.

## Guidelines

**Keep it light.** Mallow should cost nothing while it sits idle. Don't add timers, polling or animation loops. React to signals instead. If a change adds memory or startup time, measure it before and after and put the numbers in the PR.

**Keep security intact.** Don't turn off TLS checks, WebKit's sandbox, its same-origin rules or its `file://` restrictions to make something work. A change in `view.rs` that alters navigation or download policy needs a sentence in the PR on why it stays safe. Read [SECURITY.md](SECURITY.md) first.

**Avoid new dependencies.** We use the system WebKitGTK and GTK and a handful of crates. If you need a crate, explain what it replaces and why a few lines of code won't do.

**Stay on the GTK thread.** Touch GTK and WebKit objects only on the main thread. Disk writes go through the store's background thread or stay small.

**Write small comments.** Comment the reason the code can't show. Skip comments that repeat what the code says.

**Respect the user.** Never add telemetry, analytics, accounts or requests to services the user didn't pick.

## Testing the UI without a screen

You can run Mallow inside a headless sway session so it never opens a window on your desktop. Give it its own runtime directory and D-Bus session, unset `DISPLAY`, and set `WLR_BACKENDS=headless`. Take screenshots with `grim`. The window exports its actions on D-Bus at `/dev/mallow/Browser/window/1`, so you can drive tabs, find, bookmarks and the site panel with `gdbus call ... org.gtk.Actions.Activate`.

Without a GPU, set `LIBGL_ALWAYS_SOFTWARE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 GSK_RENDERER=cairo` or pages render blank.

## Reporting bugs

Open an issue with your distribution, `xbps-query libwebkitgtk60` (or your distro's equivalent), X11 or Wayland, your window manager, the steps to reproduce and what you expected. Paste terminal output if Mallow printed any.

Report security problems privately. See [SECURITY.md](SECURITY.md).

## License

You agree to license your contribution under the [MIT License](LICENSE).
