//! Downloads land in the XDG download folder under a sanitized, unique name.
//! Files are never opened or executed; the UI only offers "Show in folder".

use crate::engine::{Download, DownloadEnd};
use crate::util;
use gtk::{gio, glib, prelude::*};
use std::cell::OnceCell;

#[derive(Default)]
pub struct Downloads {
    window: OnceCell<(gtk::Window, gtk::ListBox)>,
    running: std::rc::Rc<std::cell::Cell<usize>>,
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

    /// Transfers that have started and not yet ended.
    pub fn running(&self) -> usize {
        self.running.get()
    }

    pub fn show(&self, parent: Option<&gtk::Window>) {
        let (win, _) = self.ui();
        win.set_transient_for(parent);
        win.present();
    }

    /// `dir` is the folder chosen in Settings at the time the download starts.
    pub fn track(&self, parent: Option<&gtk::Window>, dl: &Download, dir: std::path::PathBuf) {
        dl.connect_destination(move |suggested| {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("mallow: cannot create {}: {e}", dir.display());
            }
            util::unique_path(&dir, &util::sanitize_filename(suggested))
        });

        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        name.add_css_class("heading");
        name.set_text(dl.source().as_deref().map_or("download", util::host));
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
        dl.connect_created({
            let name = name.clone();
            move |dest| {
                name.set_text(&dest.file_name().unwrap_or(dest.as_os_str()).to_string_lossy());
            }
        });
        dl.connect_progress({
            let (bar, status) = (bar.clone(), status.clone());
            move |fraction, bytes| {
                bar.set_fraction(fraction);
                status.set_text(&format!("{} received", glib::format_size(bytes)));
            }
        });
        let cancel = std::cell::Cell::new(Some(cancel));
        let running = self.running.clone();
        running.set(running.get() + 1);
        dl.connect_end(move |finished, end| {
            running.set(running.get().saturating_sub(1));
            row.add_css_class("finished");
            if let Some(id) = cancel.take() {
                action.disconnect(id);
            }
            match end {
                DownloadEnd::Cancelled | DownloadEnd::Failed(_) => {
                    status.add_css_class("error");
                    status.set_text(&match end {
                        DownloadEnd::Failed(msg) => format!("Failed: {msg}"),
                        _ => "Cancelled".to_string(),
                    });
                    action.set_visible(false);
                }
                DownloadEnd::Done => {
                    bar.set_fraction(1.0);
                    status.set_text(&format!(
                        "Done · {}",
                        glib::format_size(finished.received_bytes())
                    ));
                    action.set_label("Show in folder");
                    let dest = finished.destination();
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
            }
        });
    }

    /// Remove finished, failed and cancelled entries; files stay on disk.
    pub fn clear_finished(&self) {
        let Some((_, list)) = self.window.get() else {
            return;
        };
        let mut i = 0;
        while let Some(row) = list.row_at_index(i) {
            if row.child().is_some_and(|c| c.has_css_class("finished")) {
                list.remove(&row);
            } else {
                i += 1;
            }
        }
    }

    pub fn close(&self) {
        if let Some((window, _)) = self.window.get() {
            window.destroy();
        }
    }
}
