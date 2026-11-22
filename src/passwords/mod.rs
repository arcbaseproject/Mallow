#![allow(deprecated)] // Native GtkDialog is supported throughout GTK 4.
pub mod browser;
mod vault;

pub fn origin(uri: &str) -> Option<String> {
    let url = url::Url::parse(uri).ok()?;
    (url.scheme() == "https" && url.host().is_some() && url.username().is_empty() && url.password().is_none())
        .then(|| url.origin().ascii_serialization())
}

mod generator;
pub use generator::Preferences;

mod ui;
use crate::{app::App, config};
use gtk::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, Mutex},
};
pub use ui::{generator_dialog, manage};
use vault::{Result, Secret, Vault};

#[derive(Default)]
pub struct Manager {
    vault: RefCell<Option<Arc<Mutex<Vault>>>>,
    policy: std::cell::OnceCell<gio::SimpleAction>,
    status: std::cell::OnceCell<gtk::Label>,
    timer: RefCell<Option<glib::SourceId>>,
    generation: Cell<u64>,
    dialogs: RefCell<Vec<glib::WeakRef<gtk::Dialog>>>,
}
impl Manager {
    pub fn policy(&self) -> &gio::SimpleAction {
        self.policy
            .get_or_init(|| gio::SimpleAction::new("password-policy", None))
    }
    pub fn status(&self) -> &gtk::Label {
        self.status
            .get_or_init(|| gtk::Label::new(Some(if self.unlocked() { "Unlocked" } else { "Locked" })))
    }
    pub fn unlocked(&self) -> bool {
        self.vault.borrow().is_some()
    }
    pub fn lock(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.vault.take();
        if let Some(status) = self.status.get() {
            status.set_label("Locked");
        }
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        let dialogs = self.dialogs.take();
        for dialog in dialogs.into_iter().filter_map(|w| w.upgrade()) {
            dialog.response(gtk::ResponseType::Cancel);
        }
    }
    fn watch(&self, d: &gtk::Dialog) {
        let mut dialogs = self.dialogs.borrow_mut();
        dialogs.retain(|w| w.upgrade().is_some());
        dialogs.push(d.downgrade());
    }
}
pub fn reset_timer(app: &Rc<App>) {
    let mgr = &app.passwords;
    if let Some(timer) = mgr.timer.take() {
        timer.remove();
    }
    let mins = app.config.borrow().passwords.lock_minutes.min(120);
    if mgr.unlocked() && mins > 0 {
        let weak = Rc::downgrade(app);
        mgr.timer
            .replace(Some(glib::timeout_add_seconds_local_once(mins * 60, move || {
                if let Some(app) = weak.upgrade() {
                    app.passwords.timer.take();
                    app.passwords.lock();
                }
            })));
    }
}
async fn work<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    gio::spawn_blocking(f)
        .await
        .map_err(|_| "Vault operation failed.")?
}
async fn access<T: Send + 'static>(
    app: &Rc<App>,
    f: impl FnOnce(&mut Vault) -> Result<T> + Send + 'static,
) -> Result<T> {
    let epoch = app.passwords.generation.get();
    let vault = app.passwords.vault.borrow().clone().ok_or("Vault locked.")?;
    reset_timer(app);
    let result = work(move || {
        let mut v = vault.lock().map_err(|_| "Vault unavailable.")?;
        f(&mut v)
    })
    .await?;
    if epoch != app.passwords.generation.get() {
        return Err("Vault locked.");
    }
    Ok(result)
}
async fn authenticate(app: &Rc<App>, parent: &gtk::Window, fresh: bool) -> Result<()> {
    if app.passwords.unlocked() && !fresh {
        reset_timer(app);
        return Ok(());
    }
    let epoch = app.passwords.generation.get();
    let path = config::data_dir().join("passwords.sqlite");
    let check = path.clone();
    let exists = work(move || check.try_exists().map_err(|_| "Cannot inspect vault.")).await?;
    let pass = ui::master(app, parent, !exists).await?;
    let vault = work(move || {
        if exists {
            Vault::unlock(&path, &pass)
        } else {
            Vault::create(&path, &pass)
        }
    })
    .await?;
    if epoch != app.passwords.generation.get() || !parent.is_visible() {
        return Err("Cancelled.");
    }
    app.passwords.vault.replace(Some(Arc::new(Mutex::new(vault))));
    app.passwords.status().set_label("Unlocked");
    reset_timer(app);
    Ok(())
}

#[derive(Clone, Copy)]
pub enum Action {
    Accounts,
    Unlock,
    Lock,
    Change,
    Generate,
    Delete,
}
pub fn action(app: Rc<App>, parent: gtk::Window, action: Action) {
    glib::spawn_future_local(async move {
        let result = async {
            match action {
                Action::Accounts => manage(&app, &parent).await?,
                Action::Generate => generator_dialog(&app, &parent).await?,
                Action::Lock => app.passwords.lock(),
                Action::Unlock => authenticate(&app, &parent, false).await?,
                Action::Change => {
                    authenticate(&app, &parent, true).await?;
                    let pass = ui::master(&app, &parent, true).await?;
                    access(&app, move |v| v.change(&pass)).await?;
                    app.passwords.lock();
                }
                Action::Delete => {
                    authenticate(&app, &parent, true).await?;
                    if ui::consent(
                        &parent,
                        "Delete all saved passwords?",
                        "This permanently removes every account from this vault.",
                        "Delete all",
                    )
                    .await
                    {
                        access(&app, |v| v.delete(None)).await?
                    }
                }
            }
            Ok(())
        }
        .await;
        ui::report(&parent, result);
    });
}
