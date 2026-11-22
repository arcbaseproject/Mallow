# Password storage and autofill

Mallow can save website passwords in a local encrypted vault and fill them on request. Manage it in **Settings → Passwords**. No independent security audit has been performed.

## Storage

The vault is a SQLite file, `passwords.sqlite`, in Mallow's XDG data directory. It is created with mode 0600 the first time you set a master password. Creation builds the file under a temporary name and publishes it only when complete, and it never replaces an existing vault.

- Your master password goes through Argon2id (libsodium, 64 MiB, 3 passes, random 16-byte salt) to derive a wrapping key.
- The wrapping key encrypts a random 256-bit data key with XChaCha20-Poly1305.
- The data key encrypts each account. The site and username go in one envelope, bound to the row id. The password goes in a separate envelope, bound to the row id, site, and username. A ciphertext copied to another row or account fails to decrypt.
- Changing the master password rewraps the data key. It does not re-encrypt any accounts.

Mallow uses its own vault because it works without a running desktop keyring. It does not use the Secret Service.

### No recovery

Nothing can recover a forgotten master password. Without it, the vault cannot be decrypted. Mallow never deletes or resets a vault it cannot open, even one that is damaged. To start over, delete the file yourself.

### Metadata visible without the password

Anyone who can read the file can see the number of accounts, their row ids and order, the approximate length of each encrypted field, the KDF parameters, and the file's timestamps. Sites, usernames, and passwords are encrypted.

## Locking

The vault unlocks when an action needs it. It locks after the configured inactivity period (5 minutes by default; choose **Never** to turn it off), on **Lock**, after a master-password change, and when Mallow quits. Locking cancels open password dialogs and discards results from operations still running.

## Browser integration

Mallow attaches the form script only in normal windows. Private windows never save, fill, or open the vault.

The script runs in an isolated script world, so page scripts cannot see it or its message channel. It runs only in the main frame of an HTTPS page with no certificate errors. Embedded frames are ignored. Every message carries a token tied to the current document. Mallow ignores messages after navigation, from background tabs, or from a different origin than the one loaded. Origins must match exactly. A different scheme, subdomain, or port counts as another site.

When both **Offer to save** and **Offer to fill** are off, Mallow injects nothing and detaches any listeners already installed.

### Saving

Mallow offers to save only after you submit a form yourself, by clicking its submit button or pressing Enter within one second of the submission. The form must use POST to the same origin in the same frame.

Many sites, including multi-step sign-ins such as Google's, log you in with page scripts instead of submitting a form. For these, Mallow notes the password when you click a button or press Enter while a password you typed is in the field. It offers to save only if the page navigates within 15 seconds, which covers both a full page load and a change of address within the page. A failed attempt that stays on the page gets no prompt. If you leave the page within those 15 seconds after a failed attempt, Mallow can still offer to save the wrong password, so choose **Not now**. When the password step has no username field, Mallow uses the username you typed earlier on the same site.

The prompt offers **Save**, **Not now**, and **Never for this website**. If the vault is unlocked and the site, username, and password are already saved unchanged, Mallow shows no prompt. If only the password differs, the prompt offers **Update** instead of **Save**. If the vault is locked, Mallow cannot check beforehand, so it asks for confirmation after unlocking before replacing a saved password. You can clear never-save sites in Settings.

### Filling

When you focus a recognized login field, Mallow shows a **Use saved password…** button above the page. It floats over the page so the page layout does not move. A registration or password-change field shows **Generate password…** instead. Mallow fills nothing until you pick an account, or confirm a generated password, in a native dialog. In the account chooser, the first match is selected, so typing to search and pressing Enter fills that account. Escape closes the chooser. Before filling, Mallow checks again that the document, URL, form, and fields have not changed. It never submits a form.

### Supported forms

- A POST form on the same origin with one to three visible password fields and at most one username, email, or phone field.
- A password field with `autocomplete="new-password"` marks a registration or change form. Unannotated confirmation fields are not detected.
- The following are skipped: forms with `autocomplete="off"` on a password field; one-time code, PIN, and card fields; GET forms; forms that submit to another origin or target; and pages with `<base target>`.

## Exposure you should know about

- **The site receives what is filled.** Once a password is in the form, the page's own scripts can read it, as with typing it.
- **Clipboard.** Copying a username or password puts it on the system clipboard. Mallow does not clear the clipboard afterward. Clipboard managers and history tools may keep a copy.
- **Memory.** Secrets are zeroed after use where practical. While unlocked, the data key stays in process memory. It can end up in swap or a core dump. Any program running as your user can read the process memory or record your keystrokes.
- **Same-user compromise.** The vault protects a copied or stolen file. It does not protect against malware already running in your account.

## Not implemented

There is no import, export, sync, Secret Service backend, passkey support, or master-password recovery.
