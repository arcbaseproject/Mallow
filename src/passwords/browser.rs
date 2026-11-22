//! No page-world bridge. Each view has its own channel in a named script world.
use super::*;
use crate::window::{Tab, Window};
use rand::{RngCore, rngs::OsRng};
use serde::Deserialize;
use webkit6::{LoadEvent, WebView, prelude::*};

const WORLD: &str = "dev.mallow.passwords.v1";
#[derive(Deserialize)]
struct Event {
    event: String,
    origin: String,
    token: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    pass: String,
}
impl Drop for Event {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.pass.zeroize();
    }
}
fn trusted_origin(view: &WebView) -> Option<String> {
    let (_, errors) = view.tls_info()?;
    if !errors.is_empty() || view.is_loading() {
        return None;
    }
    origin(&view.uri()?)
}
fn nonce() -> String {
    format!(
        "{:032x}",
        u128::from_le_bytes({
            let mut bytes = [0; 16];
            OsRng.fill_bytes(&mut bytes);
            bytes
        })
    )
}
fn allowed(private: bool, expected: &str, live: Option<&str>, token: &str, received: &str) -> bool {
    !private && !token.is_empty() && token == received && live == Some(expected)
}
struct Bridge {
    window: std::rc::Weak<Window>,
    tab: std::rc::Weak<Tab>,
    view: glib::WeakRef<WebView>,
    token: RefCell<String>,
    channel: String,
    offer: RefCell<Option<(gtk::Button, glib::SignalHandlerId)>>,
    /// Credential from a script-driven login, offered if the page navigates soon.
    candidate: RefCell<Option<(Event, std::time::Instant)>>,
}
impl Bridge {
    fn invalidate(&self) {
        self.token.borrow_mut().clear();
        self.dismiss();
    }
    /// The page navigated after a script-driven login: offer to save it.
    fn flush_candidate(&self) {
        let Some((event, at)) = self.candidate.take() else {
            return;
        };
        let Some(w) = self.window.upgrade() else { return };
        if at.elapsed() > std::time::Duration::from_secs(15) || w.private {
            return;
        }
        let parent: gtk::Window = w.win.clone().upcast();
        let app = w.app.clone();
        glib::spawn_future_local(async move {
            let result = save(&app, &parent, event).await;
            ui::report(&parent, result);
        });
    }
    fn dismiss(&self) {
        if let Some((button, switch)) = self.offer.take()
            && let Some(w) = self.window.upgrade()
        {
            w.stack.disconnect(switch);
            if button.parent().is_some() {
                w.overlay.remove_overlay(&button);
            }
        }
    }
    fn current(&self, event: &Event) -> Option<(Rc<Window>, WebView)> {
        let w = self.window.upgrade()?;
        let t = self.tab.upgrade()?;
        let v = self.view.upgrade()?;
        let live = trusted_origin(&v);
        (w.is_current(&t)
            && t.view.borrow().as_ref() == Some(&v)
            && w.win.is_visible()
            && allowed(
                w.private,
                &event.origin,
                live.as_deref(),
                &self.token.borrow(),
                &event.token,
            ))
        .then_some((w, v))
    }
    fn install(&self) {
        self.invalidate();
        let (Some(w), Some(v)) = (self.window.upgrade(), self.view.upgrade()) else {
            return;
        };
        let Some(origin) = trusted_origin(&v) else { return };
        let cfg = w.app.config.borrow();
        let p = &cfg.passwords;
        let save = p.save && !p.excluded.contains(&origin);
        if !save && !p.fill {
            // Nothing to offer: detach listeners from an earlier install and skip injection.
            v.call_async_javascript_function(
                "globalThis.mallowAbort?.abort(); delete globalThis.mallowFill;",
                None,
                Some(WORLD),
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
            return;
        }
        let args = glib::VariantDict::new(None);
        let token = nonce();
        args.insert("expected", origin);
        args.insert("token", token.clone());
        args.insert("channel", self.channel.clone());
        args.insert("saveEnabled", i32::from(save));
        args.insert("fillEnabled", i32::from(p.fill));
        self.token.replace(token);
        v.call_async_javascript_function(
            include_str!("forms.js"),
            Some(&args.end()),
            Some(WORLD),
            None,
            None::<&gio::Cancellable>,
            |_| {},
        );
    }
    fn receive(self: &Rc<Self>, event: Event) {
        let Some((w, _)) = self.current(&event) else {
            return;
        };
        self.dismiss();
        let parent: gtk::Window = w.win.clone().upcast();
        let cfg = w.app.config.borrow();
        let saveable = |e: &Event| {
            cfg.passwords.save
                && !cfg.passwords.excluded.contains(&e.origin)
                && !e.pass.is_empty()
                && e.pass.len() <= 16384
                && e.user.len() <= 2048
        };
        match event.event.as_str() {
            "candidate" if saveable(&event) => {
                self.candidate.replace(Some((event, std::time::Instant::now())));
            }
            "save" if saveable(&event) => {
                // A real form submission supersedes the script-login candidate.
                self.candidate.take();
                let app = w.app.clone();
                glib::spawn_future_local(async move {
                    let result = save(&app, &parent, event).await;
                    ui::report(&parent, result);
                });
            }
            "choose" | "generate" if cfg.passwords.fill => {
                let button = gtk::Button::with_label(if event.event == "generate" {
                    "Generate password…"
                } else {
                    "Use saved password…"
                });
                button.add_css_class("tonal");
                button.set_halign(gtk::Align::Center);
                button.set_valign(gtk::Align::Start);
                button.set_margin_top(8);
                let bridge = Rc::downgrade(self);
                let event = Rc::new(event);
                button.connect_clicked(move |_| {
                    let Some(bridge) = bridge.upgrade() else { return };
                    bridge.dismiss();
                    let event = event.clone();
                    let parent = parent.clone();
                    glib::spawn_future_local(async move {
                        let result = bridge.fill(&event, &parent).await;
                        ui::report(&parent, result);
                    });
                });
                // Float over the page: inserting it into the layout shifted the form under the cursor.
                w.overlay.add_overlay(&button);
                let bridge = Rc::downgrade(self);
                let switch = w.stack.connect_visible_child_notify(move |_| {
                    if let Some(b) = bridge.upgrade() {
                        b.dismiss()
                    }
                });
                self.offer.replace(Some((button, switch)));
            }
            _ => {}
        }
    }
    async fn fill(&self, event: &Event, parent: &gtk::Window) -> Result<()> {
        let (w, _) = self.current(event).ok_or("Form expired.")?;
        let generate = event.event == "generate";
        let (name, pass) = if generate {
            let pass = generator::generate(&w.app.config.borrow().passwords)?;
            if !ui::consent(parent, "Use generated password?", &event.origin, "Fill password").await {
                return Ok(());
            }
            (String::new(), pass)
        } else {
            authenticate(&w.app, parent, false).await?;
            let expected = event.origin.clone();
            let accounts = access(&w.app, move |v| {
                Ok(v.list()?
                    .into_iter()
                    .filter(|(_, o, _)| *o == expected)
                    .collect::<Vec<_>>())
            })
            .await?;
            let Some(account) = ui::choose(&w.app, parent, &event.origin, &accounts, false).await? else {
                return Ok(());
            };
            let name = account.2.clone();
            let pass = access(&w.app, move |v| v.password(&account)).await?;
            (name, pass)
        };
        let (_, view) = self
            .current(event)
            .ok_or("Page changed. Choose a login field again.")?;
        if !w.app.config.borrow().passwords.fill {
            return Ok(());
        }
        let args = glib::VariantDict::new(None);
        args.insert("token", event.token.as_str());
        args.insert("id", event.id.as_str());
        args.insert("origin", event.origin.as_str());
        args.insert("user", name);
        args.insert("password", pass.as_str());
        args.insert("generate", i32::from(generate));
        let filled = view
            .call_async_javascript_function_future(
                "return globalThis.mallowFill?.(token,id,origin,user,password,Boolean(generate)) === true;",
                Some(&args.end()),
                Some(WORLD),
                None,
            )
            .await
            .map_err(|_| "The form changed.")?;
        if !filled.to_boolean() {
            return Err("The form changed. Select the field again.");
        }
        Ok(())
    }
}
/// None: no account for this site and username. Some(same): whether its password matches.
async fn stored(app: &Rc<App>, e: &Event) -> Result<Option<bool>> {
    let (origin, user, pass) = (e.origin.clone(), e.user.clone(), Secret::new(e.pass.clone()));
    access(app, move |v| {
        match v.list()?.into_iter().find(|(_, o, u)| *o == origin && *u == user) {
            Some(a) => Ok(Some(*v.password(&a)? == *pass)),
            None => Ok(None),
        }
    })
    .await
}
async fn save(app: &Rc<App>, parent: &gtk::Window, mut e: Event) -> Result<()> {
    use gtk::ResponseType as R;
    // A locked vault can't be checked before asking; that case confirms updates after unlock.
    let known = if app.passwords.unlocked() {
        Some(stored(app, &e).await?)
    } else {
        None
    };
    if known == Some(Some(true)) {
        return Ok(());
    }
    let update = known == Some(Some(false));
    let d = ui::dialog(
        parent,
        if update {
            "Update saved password?"
        } else {
            "Save password?"
        },
    );
    let p = gtk::PasswordEntry::builder()
        .editable(false)
        .css_classes(["field"])
        .build();
    p.set_text(&e.pass);
    for label in [&e.origin, &e.user] {
        let l = gtk::Label::builder().label(label).xalign(0.).wrap(true).build();
        d.content_area().append(&l);
    }
    d.content_area().append(&p);
    d.add_button("Never for this website", R::Reject);
    d.add_button("Not now", R::Cancel);
    d.add_button(if update { "Update" } else { "Save" }, R::Ok);
    let r = ui::response(app, &d).await;
    p.set_text("");
    if r == R::Reject {
        let mut cfg = app.config.borrow_mut();
        if !cfg.passwords.excluded.contains(&e.origin) {
            cfg.passwords.excluded.push(e.origin.clone());
            cfg.save();
        }
        drop(cfg);
        app.passwords.policy().activate(None);
        return Ok(());
    }
    if r != R::Ok {
        return Ok(());
    }
    authenticate(app, parent, false).await?;
    if known.is_none() {
        match stored(app, &e).await? {
            Some(true) => return Ok(()),
            Some(false) => {
                let detail = format!("{}\n{}", e.origin, e.user);
                if !ui::consent(parent, "Update saved password?", &detail, "Update").await {
                    return Ok(());
                }
            }
            None => {}
        }
    }
    let origin = e.origin.clone();
    let user = e.user.clone();
    let pass = Secret::new(std::mem::take(&mut e.pass));
    if !app.config.borrow().passwords.save || app.config.borrow().passwords.excluded.contains(&origin) {
        return Ok(());
    }
    access(app, move |v| v.put(&origin, &user, &pass)).await
}
pub fn attach(w: &Rc<Window>, tab: &Rc<Tab>, view: &WebView) {
    if w.private {
        return;
    }
    let Some(manager) = view.user_content_manager() else {
        return;
    };
    let channel = format!("mallow{}", nonce());
    if !manager.register_script_message_handler(&channel, Some(WORLD)) {
        return;
    }
    let bridge = Rc::new(Bridge {
        window: Rc::downgrade(w),
        tab: Rc::downgrade(tab),
        view: view.downgrade(),
        channel: channel.clone(),
        token: Default::default(),
        offer: Default::default(),
        candidate: Default::default(),
    });
    let b = bridge.clone();
    let signal = manager.connect_script_message_received(Some(&channel), move |_, value| {
        if !value.is_string() {
            return;
        }
        let raw = Secret::new(value.to_str().to_string());
        if raw.len() <= 40_000
            && let Ok(event) = serde_json::from_str(&raw)
        {
            b.receive(event);
        }
    });
    let b = bridge.clone();
    view.connect_load_changed(move |_, ev| match ev {
        LoadEvent::Finished => b.install(),
        LoadEvent::Committed => {
            b.flush_candidate();
            b.invalidate()
        }
        _ => b.invalidate(),
    });
    let policy = w.app.passwords.policy().clone();
    let b = bridge.clone();
    let update = policy.connect_activate(move |_, _| b.install());
    let b = bridge.clone();
    view.connect_uri_notify(move |v| {
        // Same-document navigation (pushState) after a script-driven login.
        b.flush_candidate();
        if v.is_loading() {
            b.invalidate()
        } else {
            b.install()
        }
    });
    let b = bridge.clone();
    view.connect_is_loading_notify(move |v| {
        if !v.is_loading() && b.token.borrow().is_empty() {
            b.install();
        }
    });
    view.add_weak_ref_notify_local(move || {
        bridge.dismiss();
        policy.disconnect(update);
        manager.disconnect(signal);
        manager.unregister_script_message_handler(&channel, Some(WORLD));
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorization() {
        let o = "https://a.test";
        assert!(allowed(false, o, Some(o), "doc", "doc"));
        for site in [
            "http://a.test",
            "https://a.test.evil",
            "https://login.a.test",
            "https://a.test:8443",
        ] {
            assert!(!allowed(false, o, Some(site), "doc", "doc"))
        }
        assert!(!allowed(true, o, Some(o), "doc", "doc"));
        assert!(!allowed(false, o, Some(o), "new", "old"));
        assert!(origin("https://a.test@other.test/").is_none());
    }
}

#[cfg(test)]
mod webkit_tests {
    use super::*;
    #[test]
    #[ignore = "requires a GTK display and a sandbox-capable WebKit runtime"]
    fn synthetic_forms_in_webkit() {
        gtk::init().unwrap();
        glib::MainContext::default().block_on(async {
            let session=webkit6::NetworkSession::new_ephemeral();
            let view=WebView::builder().network_session(&session).build();
            let win=gtk::Window::builder().child(&view).build();win.present();
            let (tx,rx)=futures_channel::oneshot::channel();
            let tx=Cell::new(Some(tx));
            view.connect_load_changed(move|_,ev|if ev==LoadEvent::Finished && let Some(tx)=tx.take(){let _=tx.send(());});
            view.load_html(r#"<form method="post"><input autocomplete="username"><input type="password" autocomplete="current-password"></form><iframe srcdoc="<input type=password>"></iframe>"#,Some("https://synthetic.test/"));
            rx.await.unwrap();
            let manager=view.user_content_manager().unwrap();
            assert!(manager.register_script_message_handler("testForms",Some(WORLD)));
            let args=glib::VariantDict::new(None);
            for (k,v) in [("channel","testForms"),("expected","https://synthetic.test"),("token","document-one")]{args.insert(k,v)}
            args.insert("fillEnabled",1);args.insert("saveEnabled",1);
            let installed=view.call_async_javascript_function_future(include_str!("forms.js"),Some(&args.end()),Some(WORLD),None).await.unwrap();assert!(installed.to_boolean());
            let page=view.call_async_javascript_function_future("return typeof globalThis.mallowFill === 'undefined' && !window.webkit?.messageHandlers?.testForms;",None,None,None).await.unwrap();assert!(page.to_boolean());
            let results=view.call_async_javascript_function_future(r#"
                const form=document.forms[0], [user,pass]=form.elements;
                const fill=(id,origin,token,gen=false)=>globalThis.mallowFill(token,id,origin,"synthetic","test-password",gen);
                user.focus(); const ok=fill("1",location.origin,"document-one") && pass.value==="test-password";
                pass.value=""; pass.focus();
                const denied=!fill("2","https://other.test","document-one") && !pass.value;
                user.focus();const stale=!fill("3",location.origin,"previous-document") && !pass.value;
                pass.focus();form.action="https://other.test/";const changed=!fill("4",location.origin,"document-one") && !pass.value;
                form.action="/";pass.autocomplete="new-password";user.focus();
                const generated=fill("5",location.origin,"document-one",true);
                const iframeSafe=!document.querySelector("iframe").contentDocument.querySelector("input").value;
                const extra=document.createElement("form");extra.method="post";extra.innerHTML='<input type=password autocomplete="one-time-code">';
                document.body.append(extra);extra.firstChild.focus();const unrelated=!fill("6",location.origin,"document-one");
                return [ok,denied,stale,changed,generated,iframeSafe,unrelated].map(Number).join("");
            "#,None,Some(WORLD),None).await.unwrap();
            assert_eq!(results.to_str().as_str(),"1111111");
            win.destroy();
        });
    }
}
