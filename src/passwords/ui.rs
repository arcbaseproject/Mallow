#![allow(deprecated)] // GtkDialog supplies native GTK4 focus and response handling.
use super::*;
use gtk::ResponseType as R;
use vault::Account;

pub(super) fn dialog(parent: &gtk::Window, title: &str) -> gtk::Dialog {
    let d = gtk::Dialog::builder()
        .title(title)
        .transient_for(parent)
        .destroy_with_parent(true)
        .modal(true)
        .default_width(440)
        .build();
    d.add_css_class("mallow");
    // Headline in the body with an empty titlebar, like Mallow's alert dialogs.
    d.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    let b = d.content_area();
    b.set_margin_top(24);
    b.set_margin_bottom(8);
    b.set_margin_start(24);
    b.set_margin_end(24);
    b.set_spacing(12);
    let h = gtk::Label::builder().label(title).xalign(0.).wrap(true).build();
    h.add_css_class("dialog-headline");
    h.set_accessible_role(gtk::AccessibleRole::Heading);
    b.append(&h);
    d
}
fn field(d: &gtk::Dialog, label: &str) -> gtk::Entry {
    let e = gtk::Entry::builder()
        .placeholder_text(label)
        .activates_default(true)
        .css_classes(["field"])
        .build();
    d.content_area().append(&e);
    e
}
fn password(d: &gtk::Dialog, label: &str) -> gtk::PasswordEntry {
    let e = gtk::PasswordEntry::builder()
        .placeholder_text(label)
        .show_peek_icon(true)
        .activates_default(true)
        .css_classes(["field"])
        .build();
    d.content_area().append(&e);
    e
}
fn note(d: &gtk::Dialog, s: &str) {
    let l = gtk::Label::builder().label(s).xalign(0.).wrap(true).build();
    d.content_area().append(&l);
}
pub(super) async fn response(app: &App, d: &gtk::Dialog) -> R {
    app.passwords.watch(d);
    // Enter in an entry confirms. Dialogs without an Ok button are unaffected.
    d.set_default_response(R::Ok);
    if let Some(ok) = d.widget_for_response(R::Ok) {
        ok.add_css_class("filled");
    }
    let r = d.run_future().await;
    d.set_visible(false);
    r
}
pub(super) async fn master(app: &App, parent: &gtk::Window, setup: bool) -> Result<Secret> {
    let d = dialog(
        parent,
        if setup {
            "Set a master password"
        } else {
            "Unlock passwords"
        },
    );
    if setup {
        note(
            &d,
            "Use at least 12 characters. Losing this password means losing access to the vault.",
        )
    }
    let p = password(&d, "Master password");
    let verify = setup.then(|| password(&d, "Confirm master password"));
    d.add_button("Cancel", R::Cancel);
    d.add_button(if setup { "Set password" } else { "Unlock" }, R::Ok);
    let r = response(app, &d).await;
    let pass = Secret::new(p.text().to_string());
    p.set_text("");
    if r != R::Ok {
        return Err("Cancelled.");
    }
    if let Some(v) = verify {
        let matches = v.text().as_str() == pass.as_str();
        v.set_text("");
        if !matches {
            return Err("Passwords do not match.");
        }
    }
    Ok(pass)
}
pub(super) fn report(parent: &gtk::Window, result: Result<()>) {
    if let Err(e) = result
        && e != "Cancelled."
        && parent.is_visible()
    {
        gtk::AlertDialog::builder().message(e).build().show(Some(parent));
    }
}
pub(super) async fn consent(parent: &gtk::Window, title: &str, description: &str, yes: &str) -> bool {
    gtk::AlertDialog::builder()
        .modal(true)
        .message(title)
        .detail(description)
        .buttons(["Cancel", yes])
        .default_button(0)
        .cancel_button(0)
        .build()
        .choose_future(Some(parent))
        .await
        == Ok(1)
}
pub(super) async fn choose(
    app: &App,
    parent: &gtk::Window,
    title: &str,
    accounts: &[Account],
    management: bool,
) -> Result<Option<Account>> {
    let d = dialog(parent, title);
    let query = gtk::SearchEntry::builder()
        .placeholder_text("Search sites and usernames")
        .activates_default(true)
        .css_classes(["field"])
        .build();
    d.content_area().append(&query);
    let list = gtk::ListBox::new();
    for (_, origin, name) in accounts {
        let l = gtk::Label::builder()
            .label({
                let name = if name.is_empty() { "(no username)" } else { name };
                // Filling is for one site, already named in the headline.
                if management {
                    format!("{origin}\n{name}")
                } else {
                    name.to_string()
                }
            })
            .xalign(0.)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        list.append(&l);
    }
    let q = query.clone();
    list.set_filter_func(move |r| {
        r.child()
            .and_downcast::<gtk::Label>()
            .is_some_and(|l| l.text().to_lowercase().contains(&q.text().to_lowercase()))
    });
    // Keep the first match selected so Enter in the search field picks it.
    let select_first = |l: &gtk::ListBox| {
        l.unselect_all();
        let mut rows = std::iter::successors(l.row_at_index(0), |r| l.row_at_index(r.index() + 1));
        l.select_row(rows.find(|r| r.is_child_visible()).as_ref());
    };
    let weak = list.downgrade();
    query.connect_search_changed(move |_| {
        if let Some(l) = weak.upgrade() {
            l.invalidate_filter();
            select_first(&l);
        }
    });
    // The search entry swallows Escape; treat it as Close.
    let weak = d.downgrade();
    query.connect_stop_search(move |_| {
        if let Some(d) = weak.upgrade() {
            d.response(R::Cancel)
        }
    });
    let weak = d.downgrade();
    list.connect_row_activated(move |_, _| {
        if let Some(d) = weak.upgrade() {
            d.response(R::Ok)
        }
    });
    d.content_area().append(
        &gtk::ScrolledWindow::builder()
            .min_content_height(160)
            .child(&list)
            .build(),
    );
    if accounts.is_empty() {
        note(&d, "No saved accounts.")
    }
    d.add_button("Close", R::Cancel);
    if management {
        d.add_button("Add password", R::Apply);
    }
    let ok = d.add_button(
        if management {
            "Manage account"
        } else {
            "Fill account"
        },
        R::Ok,
    );
    ok.set_sensitive(false);
    list.connect_row_selected(move |_, row| ok.set_sensitive(row.is_some()));
    select_first(&list);
    match response(app, &d).await {
        R::Apply if management => Ok(None),
        R::Ok => list
            .selected_row()
            .and_then(|r| accounts.get(r.index() as usize))
            .cloned()
            .map(Some)
            .ok_or("Select an account."),
        _ => Err("Cancelled."),
    }
}
pub async fn manage(app: &Rc<App>, parent: &gtk::Window) -> Result<()> {
    authenticate(app, parent, false).await?;
    loop {
        let accounts = access(app, |v| v.list()).await?;
        let Some(account) = choose(app, parent, "Saved passwords", &accounts, true).await? else {
            let d = dialog(parent, "Add password");
            let site = field(&d, "HTTPS website");
            let user = field(&d, "Username or email");
            let p = password(&d, "Password");
            d.add_button("Cancel", R::Cancel);
            d.add_button("Save", R::Ok);
            if response(app, &d).await == R::Ok {
                let origin = origin(&site.text()).ok_or("Enter a valid HTTPS website.")?;
                let user = user.text().to_string();
                let pass = Secret::new(p.text().to_string());
                p.set_text("");
                if accounts.iter().any(|(_, o, u)| *o == origin && *u == user)
                    && !consent(
                        parent,
                        "Update existing account?",
                        "This site and username already have a saved password.",
                        "Update",
                    )
                    .await
                {
                    continue;
                }
                access(app, move |v| v.put(&origin, &user, &pass)).await?;
            }
            continue;
        };
        let d = dialog(parent, "Manage account");
        note(&d, &format!("{}\n{}", account.1, account.2));
        for (label, code) in [
            ("Copy username", 1),
            ("Reveal password", 2),
            ("Copy password", 3),
            ("Edit account", 4),
            ("Delete account", 5),
        ] {
            let b = gtk::Button::with_label(label);
            b.add_css_class("action-row");
            if let Some(l) = b.child() {
                l.set_halign(gtk::Align::Start);
            }
            if code == 5 {
                b.add_css_class("destructive");
            }
            let weak = d.downgrade();
            b.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.response(R::Other(code))
                }
            });
            d.content_area().append(&b);
        }
        d.add_button("Back", R::Cancel);
        match response(app, &d).await {
            R::Other(1) => parent.clipboard().set_text(&account.2),
            R::Other(code @ (2 | 3)) => {
                authenticate(app, parent, true).await?;
                let secret = access(app, move |v| v.password(&account)).await?;
                if code == 3 {
                    parent.clipboard().set_text(&secret)
                } else {
                    let d = dialog(parent, "Saved password");
                    let p = password(&d, "Password");
                    p.set_text(&secret);
                    p.set_editable(false);
                    drop(secret);
                    d.add_button("Close", R::Close);
                    response(app, &d).await;
                    p.set_text("");
                }
            }
            R::Other(4) => {
                let d = dialog(parent, "Edit account");
                note(&d, &account.1);
                let username = field(&d, "Username or email");
                username.set_text(&account.2);
                let p = password(&d, "New password (optional)");
                d.add_button("Cancel", R::Cancel);
                d.add_button("Update", R::Ok);
                if response(app, &d).await == R::Ok {
                    let pass = Secret::new(p.text().to_string());
                    p.set_text("");
                    let name = username.text().to_string();
                    access(app, move |v| v.edit(&account, &name, &pass)).await?;
                }
            }
            R::Other(5) if consent(parent, "Delete this account?", &account.1, "Delete").await => {
                access(app, move |v| v.delete(Some(account.0))).await?
            }
            _ => {}
        }
    }
}
pub async fn generator_dialog(app: &App, parent: &gtk::Window) -> Result<()> {
    let secret = generator::generate(&app.config.borrow().passwords)?;
    let d = dialog(parent, "Generated password");
    let p = password(&d, "Generated password");
    p.set_text(&secret);
    d.add_button("Close", R::Close);
    d.add_button("Copy", R::Ok);
    if response(app, &d).await == R::Ok {
        parent.clipboard().set_text(&p.text())
    }
    p.set_text("");
    Ok(())
}
