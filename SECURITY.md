# Security

Mallow opens pages from strangers on the internet. This file explains what protects you, what you need to keep updated and how to tell us about a problem.

## Report a vulnerability

Report it privately. Don't open a public issue.

Use GitHub's private reporting: open the [Security tab](https://github.com/arcbaseproject/Mallow/security) and choose **Report a vulnerability**, or go straight to the [new advisory form](https://github.com/arcbaseproject/Mallow/security/advisories/new). Only the maintainers see your report.

Tell us the Mallow version or commit, your WebKitGTK version, the steps to reproduce and the impact you expect. We aim to reply within a week and will credit you in the fix unless you ask us not to.

Some bugs live in WebKitGTK rather than Mallow. Send those to the [WebKitGTK project](https://webkitgtk.org/security.html). If you can't tell which side a bug belongs to, send it to us and we'll forward it.

## Keep WebKitGTK updated

Mallow uses the WebKitGTK your system installs and never ships its own copy. WebKitGTK renders pages, runs JavaScript, handles TLS and sandboxes web content, so its version matters more than Mallow's.

On Void Linux:

```sh
sudo xbps-install -Su                 # update everything, including WebKitGTK
xbps-query libwebkitgtk60             # show the installed version
```

Compare your version against the [WebKitGTK security advisories](https://webkitgtk.org/security.html). Restart Mallow after an update so it loads the new engine.

## What Mallow does to protect you

**Engine sandbox.** WebKitGTK runs each tab in its own web process inside a bubblewrap sandbox. Mallow leaves that setup alone.

**TLS.** WebKit rejects invalid certificates and Mallow offers no way around that. You see WebKit's error page instead. The lock icon opens the certificate details.

**Downloads.** Mallow saves files to your Downloads folder under a cleaned-up name with no folders, no leading dots and no control characters. It never overwrites a file and never opens or runs one. The downloads window only offers "Show in folder".

**Other apps.** A page can't open `mailto:`, `magnet:` or other non-web links on its own. You have to click the link, then confirm in a dialog before Mallow hands it to your desktop.

**Local files.** WebKit's defaults stop web pages from loading `file://` URLs. You can still type a file path in the address bar.

**Permissions.** A site has to ask before it uses your camera, microphone, location, notifications, clipboard or similar features, and Mallow asks you on every request. WebKit doesn't tell Mallow which frame sent these requests, so Mallow won't reuse an answer that an embedded page could borrow. The one exception is a site asking for cookies while embedded in another site: WebKit names both sites, so Mallow remembers your answer for that pair until you close the window. The lock icon lists those answers and lets you reset them.

**Updates.** Mallow only checks GitHub for a newer release and links to it. It never downloads or runs an update itself, so a compromised release page can't run code on your machine without you building it.

**Passwords.** Mallow can save website passwords in a local vault encrypted under your master password. [PASSWORDS.md](PASSWORDS.md) describes how it stores them and what it protects against.

**Private windows.** Each private window gets its own in-memory WebKit session. Mallow writes no history for it, and WebKit discards its cookies and site data when you close the window.

## What Mallow doesn't protect against

- A private window hides your browsing from other people who use your computer. It doesn't hide you from the sites you visit, your network or your employer.
- Mallow can't fix a WebKitGTK bug until your distribution ships the fix.
- Tracker blocking uses a short domain list. It reduces tracking without stopping it.
- Some sites built only for Chrome won't work, and Mallow won't weaken WebKit to make them work.

## Supported versions

We fix security issues on the latest release only.
