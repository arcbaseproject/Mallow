//! Downloads land in the XDG download folder under a sanitized, unique name.
//! Files are never opened or executed; the UI only offers "Show in folder".

use crate::util;
use gtk::{gio, glib, prelude::*};
use std::cell::OnceCell;
use webkit6::Download;

#[derive(Default)]
pub struct Downloads {
    window: OnceCell<(gtk::Window, gtk::ListBox)>,
}

impl Downloads {
    fn ui(&self) -> &(gtk::Window, gtk::ListBox) {
        self.window.get_or_init(|| {
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list.add_css_class("downloads");
            let placeholder = gtk::Label::new(Some("No downloads yet"));
            placeholder.add_css_class("dim-label");
            placeholder.set_margin_top(24);
            list.set_placeholder(Some(&placeholder));
            // Not attached to the application, so a hidden Downloads window
            // never keeps the process alive after the last browser window.
            let win = gtk::Window::builder()
                .title("Downloads")
                .default_width(460)
                .default_height(420)
                .hide_on_close(true)
                .child(&gtk::ScrolledWindow::builder().child(&list).build())
                .build();
            win.add_css_class("mallow");
            (win, list)
        })
    }

    pub fn show(&self, parent: Option<&gtk::Window>) {
        let (win, _) = self.ui();
        win.set_transient_for(parent);
        win.present();
    }

    pub fn track(&self, parent: Option<&gtk::Window>, dl: &Download) {
        dl.connect_decide_destination(|dl, suggested| {
            let dir = glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(glib::home_dir);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("mallow: cannot create {}: {e}", dir.display());
            }
            let path = util::unique_path(&dir, &util::sanitize_filename(suggested));
            dl.set_allow_overwrite(false);
            dl.set_destination(&path.to_string_lossy());
            true
        });

        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        name.add_css_class("heading");
        name.set_text(
            dl.request()
                .and_then(|r| r.uri())
                .as_deref()
                .map_or("download", util::host),
        );
        let bar = gtk::ProgressBar::new();
        let status = gtk::Label::builder().xalign(0.0).label("Starting…").build();
        status.add_css_class("dim-label");
        let action = gtk::Button::with_label("Cancel");
        action.set_valign(gtk::Align::Center);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
        text.set_hexpand(true);
        text.append(&name);
        text.append(&bar);
        text.append(&status);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("download-row");
        row.append(&text);
        row.append(&action);
        let (_, list) = self.ui();
        list.prepend(&row);
        self.show(parent);

        let cancel = action.connect_clicked({
            let dl = dl.clone();
            move |_| dl.cancel()
        });
        dl.connect_created_destination({
            let name = name.clone();
            move |_, dest| {
                let file = std::path::Path::new(dest)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned());
                name.set_text(file.as_deref().unwrap_or(dest));
            }
        });
        dl.connect_received_data({
            let (bar, status) = (bar.clone(), status.clone());
            move |dl, _| {
                bar.set_fraction(dl.estimated_progress());
                status.set_text(&format!(
                    "{} received",
                    glib::format_size(dl.received_data_length())
                ));
            }
        });
        let cancel = std::cell::Cell::new(Some(cancel));
        dl.connect_finished({
            let (bar, status, action) = (bar.clone(), status.clone(), action.clone());
            move |dl| {
                // `failed` fires before `finished` on error; keep its message.
                if status.has_css_class("error") {
                    if let Some(id) = cancel.take() {
                        action.disconnect(id);
                    }
                    action.set_visible(false);
                    return;
                }
                bar.set_fraction(1.0);
                status.set_text(&format!(
                    "Done · {}",
                    glib::format_size(dl.received_data_length())
                ));
                if let Some(id) = cancel.take() {
                    action.disconnect(id);
                }
                action.set_label("Show in folder");
                let dest = dl.destination();
                action.connect_clicked(move |b| {
                    let Some(dest) = dest.as_deref() else { return };
                    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(dest)));
                    let win = b.root().and_downcast::<gtk::Window>();
                    launcher.open_containing_folder(win.as_ref(), None::<&gio::Cancellable>, |r| {
                        if let Err(e) = r {
                            eprintln!("mallow: cannot open folder: {e}");
                        }
                    });
                });
            }
        });
        dl.connect_failed(move |_, err| {
            status.add_css_class("error");
            status.set_text(&if err.matches(webkit6::DownloadError::CancelledByUser) {
                "Cancelled".to_string()
            } else {
                format!("Failed: {}", err.message())
            });
        });
    }

    pub fn close(&self) {
        if let Some((window, _)) = self.window.get() {
            window.destroy();
        }
    }
}
