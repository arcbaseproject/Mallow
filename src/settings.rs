//! Native settings page shown at `mallow://settings`.
//!
//! Each section is built the first time it is opened, and the whole page is
//! dropped with its tab. Controls receive their current value before their
//! handlers connect, so setting up the page never writes the config.

use crate::{
    app::App,
    config::{self, Ask, Cookies, NewTab, Startup, Theme},
    engine::DataKinds,
    theme,
    window::Window,
};
use gtk::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

/// (id, label, icon)
const SECTIONS: &[(&str, &str, &str)] = &[
    ("appearance", "Appearance", "m-palette-symbolic"),
    ("general", "General", "m-home-symbolic"),
    ("passwords", "Passwords", "m-lock-symbolic"),
    ("privacy", "Privacy & security", "m-shield-symbolic"),
    ("performance", "Performance", "m-speed-symbolic"),
    ("downloads", "Downloads", "m-download-symbolic"),
    ("websites", "Languages & websites", "m-language-symbolic"),
    ("about", "About Mallow", "m-info-symbolic"),
];

/// Below this width the sidebar turns into a drop-down.
const NARROW: i32 = 760;
/// Widest the content column grows.
const COLUMN: i32 = 760;

#[derive(Clone)]
struct Ctx {
    app: Rc<App>,
    win: Weak<Window>,
}

impl Ctx {
    fn window(&self) -> Option<Rc<Window>> {
        self.win.upgrade()
    }
    fn parent(&self) -> Option<gtk::Window> {
        self.window().map(|w| w.win.clone().upcast())
    }
}

pub fn page(w: &Rc<Window>) -> gtk::Widget {
    let ctx = Ctx {
        app: w.app.clone(),
        win: Rc::downgrade(w),
    };

    // Sidebar
    let nav = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    nav.add_css_class("settings-nav-list");
    for (_, label, icon) in SECTIONS {
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        row_box.append(&gtk::Image::from_icon_name(icon));
        row_box.append(&gtk::Label::builder().label(*label).xalign(0.0).build());
        nav.append(&gtk::ListBoxRow::builder().child(&row_box).build());
    }
    let nav_title = gtk::Label::builder().label("Settings").xalign(0.0).build();
    nav_title.add_css_class("settings-nav-title");
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar.add_css_class("settings-nav");
    sidebar.append(&nav_title);
    sidebar.append(&nav);

    // Narrow-window category picker
    let labels: Vec<&str> = SECTIONS.iter().map(|s| s.1).collect();
    let picker = gtk::DropDown::from_strings(&labels);
    picker.add_css_class("settings-picker");
    picker.set_visible(false);
    picker.update_property(&[gtk::accessible::Property::Label("Settings category")]);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(150)
        .vhomogeneous(false)
        .build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
    column.add_css_class("settings-column");
    if let Some(problem) = ctx.app.config_problem.borrow().clone() {
        column.append(&banner(&ctx, &problem));
    }
    column.append(&picker);
    column.append(&stack);
    // Search shows every section at once, filtered to matching rows.
    let results = gtk::Box::new(gtk::Orientation::Vertical, 32);
    results.set_visible(false);
    column.append(&results);
    let nothing = gtk::Label::builder()
        .label("No settings match your search.")
        .xalign(0.0)
        .visible(false)
        .build();
    nothing.add_css_class("settings-note");
    column.append(&nothing);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search settings")
        .build();
    search.add_css_class("omnibox");
    search.add_css_class("settings-search");
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .child(&column)
        .build();

    // Zero-height probe: its resize signal tracks the page width without
    // watching the window or polling.
    let probe = gtk::DrawingArea::builder()
        .hexpand(true)
        .content_height(0)
        .build();
    let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main.append(&probe);
    main.append(&search);
    main.append(&scroller);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("settings");
    root.append(&sidebar);
    root.append(&main);

    let syncing = Rc::new(Cell::new(false));
    let show = {
        let (ctx, stack, nav, picker, syncing) = (
            ctx.clone(),
            stack.clone(),
            nav.clone(),
            picker.clone(),
            syncing.clone(),
        );
        let search = search.clone();
        move |i: usize| {
            let Some(&(id, _, _)) = SECTIONS.get(i) else {
                return;
            };
            // Picking a category leaves search mode.
            if !syncing.get() && !search.text().is_empty() {
                search.set_text("");
            }
            if stack.child_by_name(id).is_none() {
                stack.add_named(&build(&ctx, id), Some(id));
            }
            stack.set_visible_child_name(id);
            syncing.set(true);
            if let Some(row) = nav.row_at_index(i as i32) {
                nav.select_row(Some(&row));
            }
            picker.set_selected(i as u32);
            syncing.set(false);
        }
    };
    let show = Rc::new(show);
    {
        let (show, syncing) = (show.clone(), syncing.clone());
        nav.connect_row_selected(move |_, row| {
            if let (false, Some(row)) = (syncing.get(), row) {
                show(row.index() as usize);
            }
        });
    }
    {
        let show = show.clone();
        picker.connect_selected_notify(move |p| {
            if !syncing.get() {
                show(p.selected() as usize);
            }
        });
    }
    {
        let (ctx, stack, results, nothing, nav) = (
            ctx.clone(),
            stack.clone(),
            results.clone(),
            nothing.clone(),
            nav.clone(),
        );
        let hidden = RefCell::new(Vec::<gtk::Widget>::new());
        let (show, before) = (show.clone(), Cell::new(0));
        search.connect_search_changed(move |e| {
            for w in hidden.take() {
                w.set_visible(true);
            }
            let query = e.text().trim().to_lowercase();
            let searching = results.is_visible();
            if query.is_empty() {
                if searching {
                    for &(id, _, _) in SECTIONS {
                        if let Some(page) = results.first_child() {
                            results.remove(&page);
                            stack.add_named(&page, Some(id));
                        }
                    }
                    results.set_visible(false);
                    nothing.set_visible(false);
                    stack.set_visible(true);
                    show(before.get());
                }
                return;
            }
            if !searching {
                for &(id, _, _) in SECTIONS {
                    let page = stack.child_by_name(id).unwrap_or_else(|| build(&ctx, id));
                    if page.parent().is_some() {
                        stack.remove(&page);
                    }
                    results.append(&page);
                }
                stack.set_visible(false);
                results.set_visible(true);
                // Unselected, so clicking any category (even the last one) leaves search.
                before.set(nav.selected_row().map_or(0, |r| r.index() as usize));
                nav.unselect_all();
            }
            let mut hidden = hidden.borrow_mut();
            let mut any = false;
            let mut page = results.first_child();
            while let Some(p) = page {
                any |= filter_page(&p, &query, &mut hidden);
                page = p.next_sibling();
            }
            nothing.set_visible(!any);
        });
    }
    let search_margin = search.clone();
    probe.connect_resize(move |_, width, _| {
        let narrow = width < NARROW;
        sidebar.set_visible(!narrow);
        picker.set_visible(narrow);
        // Centre a column of at most COLUMN pixels.
        let avail = if narrow { width } else { width - 260 };
        let margin = ((avail - COLUMN) / 2).max(if narrow { 16 } else { 32 });
        column.set_margin_start(margin);
        column.set_margin_end(margin);
        search_margin.set_margin_start(margin);
        search_margin.set_margin_end(margin);
    });
    show(0);
    root.upcast()
}

/// Every label's text under `w`, lowercased, for search.
fn label_text(w: &gtk::Widget, out: &mut String) {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        out.push_str(&l.text().to_lowercase());
        out.push('\n');
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        label_text(&c, out);
        child = c.next_sibling();
    }
}

/// Hide the rows of a page that don't match `query`; a matching section title
/// keeps its whole card. Returns whether anything on the page matches.
fn filter_page(page: &gtk::Widget, query: &str, hidden: &mut Vec<gtk::Widget>) -> bool {
    let mut hide = |w: &gtk::Widget| {
        if w.is_visible() {
            w.set_visible(false);
            hidden.push(w.clone());
        }
    };
    let matches = |w: &gtk::Widget| {
        let mut t = String::new();
        label_text(w, &mut t);
        t.contains(query)
    };
    let mut any = false;
    let mut child = page.first_child();
    while let Some(c) = child {
        child = c.next_sibling();
        if c.has_css_class("settings-title") {
            continue;
        }
        let card = child.clone().filter(|n| n.has_css_class("settings-card"));
        if let (true, Some(card)) = (c.has_css_class("settings-section"), card) {
            child = card.next_sibling();
            let whole = matches(&c);
            let mut found = false;
            let mut row = card.first_child();
            while let Some(r) = row {
                row = r.next_sibling();
                if whole || matches(&r) {
                    found = true;
                } else {
                    hide(&r);
                }
            }
            if !found {
                hide(&c);
                hide(&card);
            }
            any |= found;
        } else {
            hide(&c);
        }
    }
    if !any {
        hide(page);
    }
    any
}

fn build(ctx: &Ctx, id: &str) -> gtk::Widget {
    match id {
        "appearance" => appearance(ctx),
        "general" => general(ctx),
        "passwords" => passwords(ctx),
        "privacy" => privacy(ctx),
        "performance" => performance(ctx),
        "downloads" => downloads(ctx),
        "websites" => websites(ctx),
        _ => about(ctx),
    }
}

// ---------------------------------------------------------------- components

fn page_box(title: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    b.add_css_class("settings-page");
    let h = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    h.add_css_class("settings-title");
    h.set_accessible_role(gtk::AccessibleRole::Heading);
    b.append(&h);
    b
}

/// A titled group of rows; returns the group to append rows to.
fn card(page: &gtk::Box, title: &str) -> gtk::Box {
    let h = gtk::Label::builder().label(title).xalign(0.0).build();
    h.add_css_class("settings-section");
    h.set_accessible_role(gtk::AccessibleRole::Heading);
    let group = gtk::Box::new(gtk::Orientation::Vertical, 2);
    group.add_css_class("settings-card");
    page.append(&h);
    page.append(&group);
    group
}

fn text(title: &str, sub: Option<&str>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    b.set_hexpand(true);
    b.set_valign(gtk::Align::Center);
    let t = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    t.add_css_class("settings-row-title");
    b.append(&t);
    if let Some(sub) = sub {
        let s = gtk::Label::builder().label(sub).xalign(0.0).wrap(true).build();
        s.add_css_class("settings-row-sub");
        b.append(&s);
    }
    b
}

fn row(group: &gtk::Box, title: &str, sub: Option<&str>, control: Option<&gtk::Widget>) -> gtk::Box {
    let r = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    r.add_css_class("settings-row");
    r.append(&text(title, sub));
    if let Some(c) = control {
        c.set_valign(gtk::Align::Center);
        c.update_property(&[gtk::accessible::Property::Label(title)]);
        r.append(c);
    }
    group.append(&r);
    r
}

fn switch_row(
    group: &gtk::Box,
    title: &str,
    sub: Option<&str>,
    on: bool,
    f: impl Fn(bool) + 'static,
) -> gtk::Switch {
    let sw = gtk::Switch::builder().active(on).build();
    sw.connect_active_notify(move |s| f(s.is_active()));
    row(group, title, sub, Some(sw.upcast_ref()));
    sw
}

fn dropdown_row(
    group: &gtk::Box,
    title: &str,
    sub: Option<&str>,
    items: &[&str],
    selected: usize,
    f: impl Fn(usize) + 'static,
) -> gtk::DropDown {
    let dd = gtk::DropDown::from_strings(items);
    dd.set_selected(selected as u32);
    dd.connect_selected_notify(move |d| f(d.selected() as usize));
    row(group, title, sub, Some(dd.upcast_ref()));
    dd
}

/// Radio choices, one row each.
fn radio_rows(group: &gtk::Box, options: &[(&str, &str)], selected: usize, f: impl Fn(usize) + 'static) {
    let f = Rc::new(f);
    let mut first: Option<gtk::CheckButton> = None;
    for (i, (title, sub)) in options.iter().enumerate() {
        let check = gtk::CheckButton::builder().active(i == selected).build();
        check.set_group(first.as_ref());
        first.get_or_insert_with(|| check.clone());
        let f = f.clone();
        check.connect_toggled(move |c| {
            if c.is_active() {
                f(i);
            }
        });
        let r = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        r.add_css_class("settings-row");
        r.append(&check);
        r.append(&text(title, Some(sub)));
        check.update_property(&[gtk::accessible::Property::Label(title)]);
        // The whole row is the click target.
        let click = gtk::GestureClick::new();
        let c = check.clone();
        click.connect_released(move |_, _, _, _| c.set_active(true));
        r.add_controller(click);
        group.append(&r);
    }
}

/// Text input that validates as you type and applies on Enter or when it
/// loses focus. Invalid text is never applied or saved.
fn entry_row(
    group: &gtk::Box,
    title: &str,
    sub: &str,
    value: &str,
    placeholder: &str,
    check: impl Fn(&str) -> Result<(), &'static str> + 'static,
    apply: impl Fn(&str) + 'static,
) -> gtk::Box {
    let entry = gtk::Entry::builder()
        .text(value)
        .placeholder_text(placeholder)
        .hexpand(true)
        .build();
    entry.add_css_class("field");
    entry.update_property(&[gtk::accessible::Property::Label(title)]);
    let error = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    error.add_css_class("settings-error");
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    b.add_css_class("settings-row");
    b.append(&text(title, Some(sub)));
    b.append(&entry);
    b.append(&error);
    group.append(&b);

    let last = Rc::new(std::cell::RefCell::new(value.trim().to_string()));
    let check = Rc::new(check);
    let commit = {
        let (error, check, last) = (error.clone(), check.clone(), last.clone());
        Rc::new(move |e: &gtk::Entry| {
            let v = e.text().trim().to_string();
            match check(&v) {
                Ok(()) if *last.borrow() != v => {
                    *last.borrow_mut() = v.clone();
                    apply(&v);
                }
                Ok(()) => {}
                Err(msg) => {
                    error.set_text(msg);
                    error.set_visible(true);
                }
            }
        })
    };
    entry.connect_changed(move |e| {
        let ok = check(e.text().trim()).is_ok();
        if ok {
            error.set_visible(false);
            e.remove_css_class("error");
        } else {
            e.add_css_class("error");
        }
    });
    let c = commit.clone();
    entry.connect_activate(move |e| c(e));
    let focus = gtk::EventControllerFocus::new();
    let e = entry.clone();
    focus.connect_leave(move |_| commit(&e));
    entry.add_controller(focus);
    b
}

fn button(label: &str, style: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class(style);
    b.set_valign(gtk::Align::Center);
    b
}

fn confirm(ctx: &Ctx, message: &str, detail: &str, action: &str, f: impl FnOnce() + 'static) {
    let dialog = gtk::AlertDialog::builder()
        .modal(true)
        .message(message)
        .detail(detail)
        .buttons(["Cancel", action])
        .cancel_button(0)
        .default_button(0)
        .build();
    dialog.choose(ctx.parent().as_ref(), None::<&gio::Cancellable>, move |r| {
        if r == Ok(1) {
            f();
        }
    });
}

fn note(page: &gtk::Box, msg: &str) {
    let l = gtk::Label::builder().label(msg).xalign(0.0).wrap(true).build();
    l.add_css_class("settings-note");
    page.append(&l);
}

fn banner(ctx: &Ctx, msg: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.add_css_class("settings-banner");
    let icon = gtk::Image::from_icon_name("m-warning-symbolic");
    b.append(&icon);
    let l = gtk::Label::builder()
        .label(msg)
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    b.append(&l);
    let dismiss = button("Dismiss", "text-btn");
    let (app, weak) = (ctx.app.clone(), b.downgrade());
    dismiss.connect_clicked(move |_| {
        app.config_problem.replace(None);
        if let Some(b) = weak.upgrade() {
            b.set_visible(false);
        }
    });
    b.append(&dismiss);
    b
}

/// Fill a circle with a `#rrggbb` colour, or a hue wheel for `None`.
fn draw_dot(cr: &gtk::cairo::Context, w: i32, h: i32, colour: Option<&str>) {
    let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let r = cx.min(cy);
    match colour.and_then(theme::parse_hex) {
        Some((red, green, blue)) => {
            cr.set_source_rgb(red, green, blue);
            cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
        None => {
            const SLICES: usize = 12;
            for i in 0..SLICES {
                let a0 = i as f64 / SLICES as f64 * std::f64::consts::TAU;
                let a1 = (i + 1) as f64 / SLICES as f64 * std::f64::consts::TAU;
                if let Some((red, green, blue)) = theme::parse_hex(&theme::seed_for_hue(i as f64 * 30.0)) {
                    cr.set_source_rgb(red, green, blue);
                }
                cr.move_to(cx, cy);
                cr.arc(cx, cy, r, a0 - 0.01, a1 + 0.01);
                cr.close_path();
                let _ = cr.fill();
            }
        }
    }
}

type OnPick = Rc<std::cell::RefCell<Option<Box<dyn Fn(&str)>>>>;

/// The "custom colour" swatch: a hue wheel (or the custom accent) that opens
/// a Material picker with a hue slider, hex field and preview.
fn custom_colour_button(ctx: &Ctx) -> (gtk::MenuButton, gtk::DrawingArea, OnPick) {
    let on_pick: OnPick = Rc::default();
    let dot = gtk::DrawingArea::builder()
        .content_width(40)
        .content_height(40)
        .build();
    let app = ctx.app.clone();
    dot.set_draw_func(move |_, cr, w, h| {
        let accent = app.config.borrow().accent.clone();
        let custom = !theme::PRESETS.iter().any(|p| p.eq_ignore_ascii_case(&accent));
        draw_dot(cr, w, h, custom.then_some(accent.as_str()));
    });

    let current = ctx.app.config.borrow().accent.clone();
    let state = Rc::new(std::cell::RefCell::new(current.to_ascii_uppercase()));
    let preview = gtk::DrawingArea::builder()
        .content_width(56)
        .content_height(56)
        .build();
    let s = state.clone();
    preview.set_draw_func(move |_, cr, w, h| draw_dot(cr, w, h, Some(&s.borrow())));
    let entry = gtk::Entry::builder()
        .text(state.borrow().as_str())
        .width_chars(9)
        .max_length(7)
        .build();
    entry.add_css_class("field");
    entry.update_property(&[gtk::accessible::Property::Label("Hex colour")]);
    let hue = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 359.0, 1.0);
    hue.set_draw_value(false);
    hue.set_hexpand(true);
    hue.add_css_class("hue-slider");
    hue.set_value(theme::hue(&current).unwrap_or(270.0));
    hue.update_property(&[gtk::accessible::Property::Label("Hue")]);

    let title = gtk::Label::builder().label("Custom colour").xalign(0.0).build();
    title.add_css_class("popover-title");
    let sub = gtk::Label::builder()
        .label("Slide to pick a hue, or type a hex colour")
        .xalign(0.0)
        .build();
    sub.add_css_class("settings-row-sub");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let field = gtk::Box::new(gtk::Orientation::Vertical, 4);
    field.set_valign(gtk::Align::Center);
    let hex_label = gtk::Label::builder().label("Hex").xalign(0.0).build();
    hex_label.add_css_class("field-label");
    field.append(&hex_label);
    field.append(&entry);
    top.append(&preview);
    top.append(&field);
    let cancel = button("Cancel", "text-btn");
    let apply = button("Apply", "filled");
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.append(&cancel);
    actions.append(&apply);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.add_css_class("colour-picker");
    for w in [
        title.upcast_ref::<gtk::Widget>(),
        sub.upcast_ref(),
        top.upcast_ref(),
        hue.upcast_ref(),
        actions.upcast_ref(),
    ] {
        content.append(w);
    }
    let pop = gtk::Popover::builder().child(&content).build();
    pop.add_css_class("info-popover");
    pop.set_default_widget(Some(&apply));

    let button = gtk::MenuButton::builder()
        .child(&dot)
        .popover(&pop)
        .tooltip_text("Custom colour")
        .build();
    button.add_css_class("custom-swatch");
    button.set_valign(gtk::Align::Center);
    button.update_property(&[gtk::accessible::Property::Label("Custom accent colour")]);

    // Slider and hex field drive each other; `syncing` stops the echo.
    let syncing = Rc::new(Cell::new(false));
    {
        let (state, entry, preview, syncing) =
            (state.clone(), entry.clone(), preview.clone(), syncing.clone());
        hue.connect_value_changed(move |s| {
            if syncing.get() {
                return;
            }
            *state.borrow_mut() = theme::seed_for_hue(s.value());
            syncing.set(true);
            entry.set_text(&state.borrow());
            syncing.set(false);
            entry.remove_css_class("error");
            preview.queue_draw();
        });
    }
    {
        let (state, hue, preview, apply) = (state.clone(), hue.clone(), preview.clone(), apply.clone());
        entry.connect_changed(move |e| {
            if syncing.get() {
                return;
            }
            let text = e.text().trim().to_ascii_uppercase();
            let text = if text.starts_with('#') {
                text
            } else {
                format!("#{text}")
            };
            let valid = theme::parse_hex(&text).is_some();
            apply.set_sensitive(valid);
            if !valid {
                e.add_css_class("error");
                return;
            }
            e.remove_css_class("error");
            *state.borrow_mut() = text.clone();
            syncing.set(true);
            hue.set_value(theme::hue(&text).unwrap_or(0.0));
            syncing.set(false);
            preview.queue_draw();
        });
    }
    {
        let (app, state, pop, on_pick) = (ctx.app.clone(), state.clone(), pop.downgrade(), on_pick.clone());
        apply.connect_clicked(move |_| {
            let hex = state.borrow().clone();
            if theme::parse_hex(&hex).is_none() {
                return;
            }
            app.set_accent(&hex);
            if let Some(f) = on_pick.borrow().as_ref() {
                f(&hex);
            }
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
        });
    }
    let p = pop.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(p) = p.upgrade() {
            p.popdown();
        }
    });
    // Reopening starts from the current accent, not an abandoned edit.
    let (app, entry2) = (ctx.app.clone(), entry.clone());
    pop.connect_show(move |_| {
        let accent = app.config.borrow().accent.to_ascii_uppercase();
        entry2.set_text(&accent);
    });
    (button, dot, on_pick)
}

// ------------------------------------------------------------------ sections

fn appearance(ctx: &Ctx) -> gtk::Widget {
    let page = page_box("Appearance");
    let cfg = ctx.app.config.borrow().clone();

    let group = card(&page, "Theme");
    let themes = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    themes.add_css_class("theme-picker");
    themes.set_homogeneous(true);
    let mut first: Option<gtk::ToggleButton> = None;
    for (t, icon, label) in [
        (Theme::System, "m-brightness_auto-symbolic", "System"),
        (Theme::Light, "m-light_mode-symbolic", "Light"),
        (Theme::Dark, "m-dark_mode-symbolic", "Dark"),
    ] {
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.append(&gtk::Image::from_icon_name(icon));
        inner.append(&gtk::Label::new(Some(label)));
        let b = gtk::ToggleButton::builder()
            .child(&inner)
            .active(cfg.theme == t)
            .build();
        b.set_group(first.as_ref());
        first.get_or_insert_with(|| b.clone());
        let app = ctx.app.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                app.set_theme(t);
            }
        });
        themes.append(&b);
    }
    let r = gtk::Box::new(gtk::Orientation::Vertical, 12);
    r.add_css_class("settings-row");
    r.append(&text(
        "Colour scheme",
        Some("System follows your desktop's light or dark preference"),
    ));
    r.append(&themes);
    group.append(&r);

    let group = card(&page, "Accent colour");
    let swatches = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    swatches.add_css_class("swatches");
    let buttons: Rc<Vec<gtk::Button>> = Rc::new(
        theme::PRESETS
            .iter()
            .enumerate()
            .map(|(i, hex)| {
                let b = gtk::Button::new();
                b.add_css_class("swatch");
                b.add_css_class("swatch-lg");
                b.add_css_class(&format!("swatch-{i}"));
                b.set_tooltip_text(Some(hex));
                b.set_valign(gtk::Align::Center);
                b.update_property(&[gtk::accessible::Property::Label(hex)]);
                if hex.eq_ignore_ascii_case(&cfg.accent) {
                    b.add_css_class("selected");
                }
                swatches.append(&b);
                b
            })
            .collect(),
    );
    let custom = custom_colour_button(ctx);
    swatches.append(&custom.0);
    let select = {
        let (buttons, custom) = (buttons.clone(), custom.clone());
        move |accent: &str| {
            let mut preset = false;
            for (b, hex) in buttons.iter().zip(theme::PRESETS) {
                if hex.eq_ignore_ascii_case(accent) {
                    preset = true;
                    b.add_css_class("selected");
                } else {
                    b.remove_css_class("selected");
                }
            }
            if preset {
                custom.0.remove_css_class("selected");
            } else {
                custom.0.add_css_class("selected");
            }
            custom.1.queue_draw();
        }
    };
    let select = Rc::new(select);
    select(&cfg.accent);
    for (b, hex) in buttons.iter().zip(theme::PRESETS) {
        let (app, select) = (ctx.app.clone(), select.clone());
        b.connect_clicked(move |_| {
            app.set_accent(hex);
            select(hex);
        });
    }
    let s2 = select.clone();
    *custom.2.borrow_mut() = Some(Box::new(move |h: &str| s2(h)));
    let r = gtk::Box::new(gtk::Orientation::Vertical, 12);
    r.add_css_class("settings-row");
    r.append(&text(
        "Seed colour",
        Some("Mallow builds its whole light and dark palette from this one colour"),
    ));
    r.append(&swatches);
    group.append(&r);

    let group = card(&page, "Layout");
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Compact mode",
        Some("Tighter tab bar and toolbar"),
        cfg.compact,
        move |on| {
            app.update(|c| c.compact = on);
            app.apply_windows();
        },
    );
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Hide the tab bar with one tab",
        Some("The tab bar comes back when you open a second tab"),
        cfg.hide_single_tab,
        move |on| {
            app.update(|c| c.hide_single_tab = on);
            app.apply_windows();
        },
    );
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Animations",
        Some("Tab, menu and page transitions. Your system's reduced-motion setting also applies."),
        cfg.animations,
        move |on| {
            app.update(|c| c.animations = on);
            app.apply_theme();
        },
    );
    page.upcast()
}

fn general(ctx: &Ctx) -> gtk::Widget {
    let page = page_box("General");
    let cfg = ctx.app.config.borrow().clone();

    let group = card(&page, "On startup");
    let modes = [Startup::NewTab, Startup::Homepage, Startup::Restore];
    let app = ctx.app.clone();
    radio_rows(
        &group,
        &[
            ("Open a new tab", "Start with the new-tab page"),
            (
                "Open your homepage",
                "Uses the homepage below, or the new-tab page if it's empty",
            ),
            (
                "Continue where you left off",
                "Reopens the tabs from your last session",
            ),
        ],
        modes.iter().position(|m| *m == cfg.startup()).unwrap_or(2),
        move |i| app.update(|c| c.startup = Some(modes[i])),
    );

    let group = card(&page, "Homepage and new tabs");
    let app = ctx.app.clone();
    entry_row(
        &group,
        "Homepage",
        "Leave empty to use the new-tab page",
        &cfg.homepage,
        "New-tab page",
        config::check_homepage,
        move |v| app.update(|c| c.homepage = v.to_string()),
    );
    let app = ctx.app.clone();
    dropdown_row(
        &group,
        "New tabs open",
        Some("Ctrl+T and the + button"),
        &["New-tab page", "Homepage"],
        (cfg.new_tab == NewTab::Homepage) as usize,
        move |i| app.update(|c| c.new_tab = if i == 1 { NewTab::Homepage } else { NewTab::Page }),
    );

    let group = card(&page, "Search");
    let mut names: Vec<&str> = config::SEARCH_ENGINES.iter().map(|e| e.0).collect();
    names.push("Custom");
    let current = config::SEARCH_ENGINES
        .iter()
        .position(|e| e.1 == cfg.search_url)
        .unwrap_or(config::SEARCH_ENGINES.len());
    let custom_value = if current == config::SEARCH_ENGINES.len() {
        cfg.search_url.clone()
    } else {
        String::new()
    };
    let app = ctx.app.clone();
    let custom = entry_row(
        &group,
        "Custom search address",
        "Put {} where your search terms go, for example https://example.org/search?q={}",
        &custom_value,
        "https://example.org/search?q={}",
        config::check_search_url,
        move |v| app.update(|c| c.search_url = v.to_string()),
    );
    custom.set_visible(current == config::SEARCH_ENGINES.len());
    // Keep the drop-down above the custom field.
    group.remove(&custom);
    let app = ctx.app.clone();
    let custom_weak = custom.downgrade();
    dropdown_row(
        &group,
        "Search engine",
        Some("Used when you type words in the address bar"),
        &names,
        current,
        move |i| {
            if let Some(c) = custom_weak.upgrade() {
                c.set_visible(i == config::SEARCH_ENGINES.len());
            }
            if let Some((_, url)) = config::SEARCH_ENGINES.get(i) {
                app.update(|c| c.search_url = url.to_string());
            }
        },
    );
    group.append(&custom);

    let group = card(&page, "Windows");
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Ask before closing several tabs",
        Some("When you close a window that has more than one tab open"),
        cfg.confirm_close,
        move |on| app.update(|c| c.confirm_close = on),
    );
    page.upcast()
}

fn privacy(ctx: &Ctx) -> gtk::Widget {
    let page = page_box("Privacy & security");
    let cfg = ctx.app.config.borrow().clone();

    let group = card(&page, "Clear browsing data");
    let clear = |title: &str, sub: &str, history: bool, types: DataKinds, what: &'static str| {
        let b = button("Clear", "tonal-btn");
        let r = row(&group, title, Some(sub), Some(b.upcast_ref()));
        r.set_widget_name(what);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |b| {
            let (app, b2) = (ctx2.app.clone(), b.downgrade());
            confirm(
                &ctx2,
                &format!("Clear {what}?"),
                "This can't be undone.",
                "Clear",
                move || {
                    app.clear_data(history, types);
                    if let Some(b) = b2.upgrade() {
                        b.set_label("Cleared");
                        b.set_sensitive(false);
                    }
                },
            );
        });
    };
    clear(
        "History",
        "Pages you visited. Bookmarks stay.",
        true,
        DataKinds::NONE,
        "your history",
    );
    clear(
        "Cookies and site data",
        "Signs you out of most websites",
        false,
        DataKinds {
            site_data: true,
            cache: false,
        },
        "cookies and site data",
    );
    clear(
        "Cached files",
        "Copies of pages and images that make sites load faster",
        false,
        DataKinds {
            site_data: false,
            cache: true,
        },
        "cached files",
    );
    clear(
        "Everything",
        "History, cookies, site data and cache",
        true,
        DataKinds::ALL,
        "all browsing data",
    );

    let group = card(&page, "Tracking");
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Block trackers",
        Some("Stops known third-party ad and analytics domains from loading"),
        cfg.block_trackers,
        move |on| app.set_block_trackers(on),
    );
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Intelligent Tracking Prevention",
        Some("WebKit limits cookies and storage for sites it sees tracking you across other sites"),
        cfg.itp,
        move |on| {
            app.update(|c| c.itp = on);
            app.apply_network();
        },
    );
    let policies = [Cookies::NoThirdParty, Cookies::Always, Cookies::Never];
    let app = ctx.app.clone();
    dropdown_row(
        &group,
        "Cookies",
        Some("Blocking all cookies signs you out of most sites"),
        &[
            "Block third-party cookies",
            "Allow all cookies",
            "Block all cookies",
        ],
        policies.iter().position(|p| *p == cfg.cookies).unwrap_or(0),
        move |i| {
            app.update(|c| c.cookies = policies[i]);
            app.apply_network();
        },
    );

    let group = card(&page, "Site permissions");
    let choice = |a: Ask| (a == Ask::Block) as usize;
    let perm = |title: &str, value: Ask, set: fn(&mut config::Permissions, Ask)| {
        let app = ctx.app.clone();
        dropdown_row(&group, title, None, &["Ask", "Block"], choice(value), move |i| {
            app.update(|c| set(&mut c.permissions, if i == 1 { Ask::Block } else { Ask::Ask }));
        });
    };
    perm("Camera", cfg.permissions.camera, |p, a| p.camera = a);
    perm("Microphone", cfg.permissions.microphone, |p, a| p.microphone = a);
    perm("Location", cfg.permissions.location, |p, a| p.location = a);
    perm("Notifications", cfg.permissions.notifications, |p, a| {
        p.notifications = a
    });
    note(
        &page,
        "Sites must ask before using these. Other requests, like clipboard access, always ask. \
         Mallow asks again for each request and never saves your answer to disk.",
    );

    let group = card(&page, "Private windows");
    row(
        &group,
        "What private windows do",
        Some(
            "Each private window keeps its own cookies, site data and downloads list in memory and \
             forgets them when you close it. Mallow writes no history for it. Websites, your \
             network and your employer can still see what you do.",
        ),
        None,
    );
    let w = ctx.win.clone();
    let open = button("Open private window", "tonal-btn");
    open.connect_clicked(move |_| {
        if let Some(w) = w.upgrade() {
            let _ = WidgetExt::activate_action(&w.win, "app.new-private-window", None);
        }
    });
    row(
        &group,
        "Start one now",
        Some("Ctrl+Shift+P"),
        Some(open.upcast_ref()),
    );
    page.upcast()
}

fn performance(ctx: &Ctx) -> gtk::Widget {
    let page = page_box("Performance");
    let cfg = ctx.app.config.borrow().clone();

    let group = card(&page, "Memory");
    let after = gtk::DropDown::from_strings(
        &config::UNLOAD_CHOICES
            .iter()
            .map(|m| {
                if *m < 60 {
                    format!("{m} min")
                } else {
                    format!("{} h", m / 60)
                }
            })
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    after.set_selected(
        config::UNLOAD_CHOICES
            .iter()
            .position(|m| *m == cfg.unload_after)
            .unwrap_or(3) as u32,
    );
    after.set_sensitive(cfg.unload_tabs);
    let app = ctx.app.clone();
    let after_weak = after.downgrade();
    switch_row(
        &group,
        "Unload inactive tabs",
        Some(
            "Frees a tab's memory when you haven't looked at it for a while. It reloads when you return, with its history kept. Tabs playing audio stay loaded.",
        ),
        cfg.unload_tabs,
        move |on| {
            app.update(|c| c.unload_tabs = on);
            if let Some(a) = after_weak.upgrade() {
                a.set_sensitive(on);
            }
            app.apply_unloading();
        },
    );
    let app = ctx.app.clone();
    after.connect_selected_notify(move |d| {
        if let Some(m) = config::UNLOAD_CHOICES.get(d.selected() as usize) {
            app.update(|c| c.unload_after = *m);
        }
    });
    row(
        &group,
        "Unload after",
        Some("Time a tab spends in the background"),
        Some(after.upcast_ref()),
    );
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Instant back and forward",
        Some(
            "Keeps recently left pages in memory so Back and Forward show them at once. Turn off to use less memory.",
        ),
        cfg.page_cache,
        move |on| {
            app.update(|c| c.page_cache = on);
            app.apply_engine();
        },
    );

    let app = ctx.app.clone();
    switch_row(
        &group,
        "Low memory mode",
        Some("Keeps smaller caches of pages and images. Sites you revisit load a little slower."),
        cfg.low_memory,
        move |on| {
            app.update(|c| c.low_memory = on);
            app.apply_engine();
        },
    );

    let group = card(&page, "Media");
    let policies = [
        config::Autoplay::Allow,
        config::Autoplay::NoSound,
        config::Autoplay::Deny,
    ];
    let app = ctx.app.clone();
    dropdown_row(
        &group,
        "Autoplay",
        Some("What videos may start on their own. Applies to tabs you open from now on."),
        &["Allow", "Only without sound", "Never"],
        policies.iter().position(|p| *p == cfg.autoplay).unwrap_or(1),
        move |i| app.update(|c| c.autoplay = policies[i]),
    );

    let group = card(&page, "Rendering");
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Hardware acceleration",
        Some(
            "Draws pages with your graphics card. Turn it off if pages flicker or show glitches. Reload open pages to apply.",
        ),
        cfg.hardware_acceleration,
        move |on| {
            app.update(|c| c.hardware_acceleration = on);
            app.apply_engine();
        },
    );
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Smooth scrolling",
        Some("Animates scrolling on pages. Turn off for instant jumps and slightly less work."),
        cfg.smooth_scrolling,
        move |on| {
            app.update(|c| c.smooth_scrolling = on);
            app.apply_engine();
        },
    );
    let app = ctx.app.clone();
    switch_row(
        &group,
        "WebGL",
        Some(
            "3D graphics for maps, games and some editors. Turning it off saves graphics memory but breaks those sites.",
        ),
        cfg.webgl,
        move |on| {
            app.update(|c| c.webgl = on);
            app.apply_engine();
        },
    );

    let group = card(&page, "Built in");
    row(
        &group,
        "Lazy tabs",
        Some("New tabs and restored tabs start no web engine until they load a page"),
        None,
    );
    row(
        &group,
        "No background work",
        Some("Mallow runs no polling, sync or telemetry while idle"),
        None,
    );

    let group = card(&page, "Diagnostics");
    let value = |s: &str| {
        let l = gtk::Label::new(Some(s));
        l.add_css_class("settings-value");
        l.set_selectable(true);
        l
    };
    let webkit = crate::engine::version();
    let gtk_v = format!(
        "{}.{}.{}",
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version()
    );
    row(
        &group,
        "Mallow",
        None,
        Some(value(env!("CARGO_PKG_VERSION")).upcast_ref()),
    );
    row(
        &group,
        crate::engine::NAME,
        None,
        Some(value(&webkit).upcast_ref()),
    );
    row(&group, "GTK", None, Some(value(&gtk_v).upcast_ref()));
    let tabs = value(&ctx.app.tab_count().to_string());
    row(
        &group,
        "Open tabs",
        Some("In all windows"),
        Some(tabs.upcast_ref()),
    );
    let loaded = value(&ctx.app.loaded_tab_count().to_string());
    row(
        &group,
        "Tabs using memory",
        Some("Tabs with a loaded page; the rest cost almost nothing"),
        Some(loaded.upcast_ref()),
    );
    let refresh = button("Refresh", "text-btn");
    refresh.set_halign(gtk::Align::End);
    let app = ctx.app.clone();
    refresh.connect_clicked(move |_| {
        tabs.set_text(&app.tab_count().to_string());
        loaded.set_text(&app.loaded_tab_count().to_string());
    });
    page.append(&refresh);
    page.upcast()
}

fn downloads(ctx: &Ctx) -> gtk::Widget {
    let page = page_box("Downloads");
    let group = card(&page, "Location");
    let dir = ctx.app.config.borrow().download_dir();
    let path = gtk::Label::builder()
        .label(dir.to_string_lossy())
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .build();
    path.add_css_class("settings-value");
    let change = button("Change…", "tonal-btn");
    let r = row(&group, "Save files to", None, Some(change.upcast_ref()));
    if let Some(t) = r.first_child().and_downcast::<gtk::Box>() {
        t.append(&path);
    }
    let ctx2 = ctx.clone();
    let path_weak = path.downgrade();
    change.connect_clicked(move |_| {
        let dialog = gtk::FileDialog::builder()
            .title("Choose download folder")
            .modal(true)
            .initial_folder(&gio::File::for_path(ctx2.app.config.borrow().download_dir()))
            .build();
        let (app, path_weak) = (ctx2.app.clone(), path_weak.clone());
        dialog.select_folder(ctx2.parent().as_ref(), None::<&gio::Cancellable>, move |r| {
            let Some(dir) = r.ok().and_then(|f| f.path()) else {
                return;
            };
            if !dir.is_dir() {
                return;
            }
            app.update(|c| c.download_dir = dir.to_string_lossy().into_owned());
            if let Some(p) = path_weak.upgrade() {
                p.set_text(&dir.to_string_lossy());
            }
        });
    });
    let open = button("Open folder", "tonal-btn");
    let ctx2 = ctx.clone();
    open.connect_clicked(move |_| {
        let dir = ctx2.app.config.borrow().download_dir();
        gtk::FileLauncher::new(Some(&gio::File::for_path(dir))).launch(
            ctx2.parent().as_ref(),
            None::<&gio::Cancellable>,
            |r| {
                if let Err(e) = r {
                    eprintln!("mallow: cannot open download folder: {e}");
                }
            },
        );
    });
    row(
        &group,
        "Downloads folder",
        Some("Opens in your file manager"),
        Some(open.upcast_ref()),
    );
    let reset = button("Use default", "text-btn");
    let (app, path_weak) = (ctx.app.clone(), path.downgrade());
    reset.connect_clicked(move |_| {
        app.update(|c| c.download_dir.clear());
        if let Some(p) = path_weak.upgrade() {
            p.set_text(&app.config.borrow().download_dir().to_string_lossy());
        }
    });
    row(
        &group,
        "Default folder",
        Some("Your desktop's Downloads folder"),
        Some(reset.upcast_ref()),
    );

    let group = card(&page, "Downloads list");
    let clear = button("Clear", "tonal-btn");
    let w = ctx.win.clone();
    clear.connect_clicked(move |b| {
        if let Some(w) = w.upgrade() {
            w.downloads.clear_finished();
            b.set_label("Cleared");
        }
    });
    row(
        &group,
        "Clear finished downloads",
        Some("Removes them from the list. The files stay in your folder."),
        Some(clear.upcast_ref()),
    );
    note(
        &page,
        "Mallow never opens or runs downloaded files and never overwrites an existing file.",
    );
    page.upcast()
}

fn websites(ctx: &Ctx) -> gtk::Widget {
    let page = page_box("Languages & websites");
    let cfg = ctx.app.config.borrow().clone();

    let group = card(&page, "Languages");
    let app = ctx.app.clone();
    entry_row(
        &group,
        "Preferred website languages",
        "Sites use this to choose a language, in order. Leave empty to use your system language. Example: en-US, fr",
        &cfg.languages,
        "System language",
        |v| config::parse_languages(v).map(|_| ()),
        move |v| {
            app.update(|c| c.languages = v.to_string());
            app.apply_engine();
        },
    );

    let group = card(&page, "Spelling");
    let app = ctx.app.clone();
    let dicts = Rc::new(Cell::new(None::<glib::WeakRef<gtk::Box>>));
    let d2 = dicts.clone();
    switch_row(
        &group,
        "Check spelling",
        Some("Underlines misspelled words in text fields"),
        cfg.spellcheck,
        move |on| {
            app.update(|c| c.spellcheck = on);
            app.apply_engine();
            if let Some(b) = d2.take() {
                if let Some(w) = b.upgrade() {
                    w.set_sensitive(on);
                }
                d2.set(Some(b));
            }
        },
    );
    let app = ctx.app.clone();
    let dict_row = entry_row(
        &group,
        "Dictionaries",
        "Leave empty for your system language. Needs the matching hunspell dictionary installed. Example: en_US, de_DE",
        &cfg.spell_languages,
        "System language",
        |v| config::parse_dictionaries(v).map(|_| ()),
        move |v| {
            app.update(|c| c.spell_languages = v.to_string());
            app.apply_engine();
        },
    );
    dict_row.set_sensitive(cfg.spellcheck);
    dicts.set(Some(dict_row.downgrade()));

    let group = card(&page, "Zoom");
    let (lo, hi) = config::ZOOM_RANGE;
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, lo * 100.0, hi * 100.0, 10.0);
    scale.set_value(cfg.default_zoom * 100.0);
    scale.set_hexpand(true);
    scale.set_draw_value(false);
    for mark in [50.0, 100.0, 150.0, 200.0] {
        scale.add_mark(mark, gtk::PositionType::Bottom, None);
    }
    scale.update_property(&[gtk::accessible::Property::Label("Default zoom")]);
    let label = gtk::Label::new(Some(&format!("{:.0}%", cfg.default_zoom * 100.0)));
    label.add_css_class("settings-value");
    label.set_width_chars(5);
    let slider = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    slider.append(&scale);
    slider.append(&label);
    let r = gtk::Box::new(gtk::Orientation::Vertical, 8);
    r.add_css_class("settings-row");
    r.append(&text(
        "Default zoom",
        Some("For pages you open from now on. Ctrl+0 returns to this size."),
    ));
    r.append(&slider);
    group.append(&r);
    // Apply at once, save once the slider rests.
    let pending: Rc<Cell<Option<glib::SourceId>>> = Rc::default();
    let app = ctx.app.clone();
    scale.connect_value_changed(move |s| {
        let zoom = (s.value() / 10.0).round() / 10.0;
        label.set_text(&format!("{:.0}%", zoom * 100.0));
        app.config.borrow_mut().default_zoom = zoom;
        if let Some(id) = pending.take() {
            id.remove();
        }
        let (app, p) = (app.clone(), pending.clone());
        pending.set(Some(glib::timeout_add_local_once(
            std::time::Duration::from_millis(400),
            move || {
                p.set(None);
                app.config.borrow().save();
            },
        )));
    });
    page.upcast()
}

fn about(ctx: &Ctx) -> gtk::Widget {
    let page = page_box("About Mallow");
    let hero = gtk::Box::new(gtk::Orientation::Vertical, 8);
    hero.add_css_class("about-hero");
    let logo = gtk::Image::from_icon_name(crate::app::APP_ID);
    logo.set_pixel_size(88);
    logo.set_accessible_role(gtk::AccessibleRole::Presentation);
    let name = gtk::Label::new(Some("Mallow"));
    name.add_css_class("about-name");
    let version = gtk::Label::new(Some(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
    version.add_css_class("about-version");
    let desc = gtk::Label::builder()
        .label(env!("CARGO_PKG_DESCRIPTION"))
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    desc.add_css_class("settings-row-sub");
    for w in [
        logo.upcast_ref::<gtk::Widget>(),
        name.upcast_ref(),
        version.upcast_ref(),
        desc.upcast_ref(),
    ] {
        hero.append(w);
    }
    page.append(&hero);

    let group = card(&page, "Built with");
    let webkit = crate::engine::version();
    let gtk_v = format!(
        "{}.{}.{}",
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version()
    );
    row(
        &group,
        crate::engine::NAME,
        Some(&format!(
            "Web engine, version {webkit}. Updated by your system's package manager."
        )),
        None,
    );
    row(
        &group,
        "GTK",
        Some(&format!("Interface toolkit, version {gtk_v}")),
        None,
    );
    row(
        &group,
        "Rust",
        Some("Programming language, with the gtk-rs bindings"),
        None,
    );

    let group = card(&page, "Updates");
    let app = ctx.app.clone();
    switch_row(
        &group,
        "Check for updates on start",
        Some("Asks GitHub for the latest release each time Mallow opens. Nothing else is sent."),
        ctx.app.config.borrow().check_updates,
        move |on| app.update(|c| c.check_updates = on),
    );
    let check = button("Check now", "tonal-btn");
    let status = row(
        &group,
        concat!("Mallow ", env!("CARGO_PKG_VERSION")),
        Some("Press Check now to look for a newer release"),
        Some(check.upcast_ref()),
    );
    let sub = status
        .first_child()
        .and_then(|t| t.last_child())
        .and_downcast::<gtk::Label>();
    let open = button("View release", "filled");
    open.set_visible(false);
    status.append(&open);
    let target = Rc::new(std::cell::RefCell::new(String::new()));
    {
        let (w, target) = (ctx.win.clone(), target.clone());
        open.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.new_tab(Some(&target.borrow()), true);
            }
        });
    }
    check.connect_clicked(move |b| {
        b.set_sensitive(false);
        b.set_label("Checking…");
        if let Some(s) = &sub {
            s.set_text("Contacting GitHub…");
        }
        let (b, sub, open, target) = (b.downgrade(), sub.clone(), open.downgrade(), target.clone());
        crate::update::check(move |outcome| {
            use crate::update::Outcome;
            let msg = match outcome {
                Outcome::Newer { version, url } => {
                    *target.borrow_mut() = url;
                    if let Some(o) = open.upgrade() {
                        o.set_visible(true);
                    }
                    format!("Version {version} is available")
                }
                Outcome::UpToDate => "You're up to date".to_string(),
                Outcome::NoReleases => "No releases have been published yet".to_string(),
                Outcome::Failed(e) => format!("Couldn't check for updates: {e}"),
            };
            if let Some(s) = &sub {
                s.set_text(&msg);
            }
            if let Some(b) = b.upgrade() {
                b.set_sensitive(true);
                b.set_label("Check again");
            }
        });
    });

    let group = card(&page, "Project");
    let repo = env!("CARGO_PKG_REPOSITORY");
    let source = button("Open", "tonal-btn");
    let w = ctx.win.clone();
    source.connect_clicked(move |_| {
        if let Some(w) = w.upgrade() {
            w.new_tab(Some(repo), true);
        }
    });
    row(&group, "Source code", Some(repo), Some(source.upcast_ref()));
    row(
        &group,
        "License",
        Some(&format!("{} License", env!("CARGO_PKG_LICENSE"))),
        None,
    );
    row(
        &group,
        "Credits",
        Some(
            "Icons: Material Symbols by Google, Apache 2.0. Web engine: the WebKitGTK project. Toolkit: GTK and gtk-rs.",
        ),
        None,
    );
    page.upcast()
}

fn passwords(ctx: &Ctx) -> gtk::Widget {
    use crate::passwords::{self, Action, Preferences};
    let page = page_box("Passwords");
    note(&page, "Manage saved passwords and autofill settings.");
    if ctx.window().is_none_or(|w| w.private) {
        note(&page, "Open a normal window to manage passwords.");
        return page.upcast();
    }
    let prefs = ctx.app.config.borrow().passwords.clone();
    let toggle = |group: &gtk::Box, label: &str, on: bool, set: fn(&mut Preferences, bool)| {
        let app = ctx.app.clone();
        switch_row(group, label, None, on, move |active| {
            let mut cfg = app.config.borrow_mut();
            set(&mut cfg.passwords, active);
            cfg.save();
            drop(cfg);
            app.passwords.policy().activate(None);
        });
    };
    // Actions are ordinary rows with a compact button, like the other settings pages.
    let command = |group: &gtk::Box, title: &str, sub: &str, label: &str, action: Action| {
        let b = button(label, "tonal-btn");
        let ctx = ctx.clone();
        b.connect_clicked(move |_| {
            if let Some(parent) = ctx.parent() {
                passwords::action(ctx.app.clone(), parent, action)
            }
        });
        row(group, title, Some(sub), Some(b.upcast_ref()));
    };
    let autofill = card(&page, "Autofill");
    toggle(&autofill, "Offer to save passwords", prefs.save, |p, v| {
        p.save = v
    });
    toggle(&autofill, "Offer to fill passwords", prefs.fill, |p, v| {
        p.fill = v
    });
    let exclusions = button("Reset", "tonal-btn");
    let context = ctx.clone();
    exclusions.connect_clicked(move |_| {
        let app = context.app.clone();
        confirm(
            &context,
            "Reset excluded websites?",
            "Mallow can offer to save passwords on these sites again.",
            "Reset",
            move || {
                let mut c = app.config.borrow_mut();
                c.passwords.excluded.clear();
                c.save();
                drop(c);
                app.passwords.policy().activate(None)
            },
        )
    });
    row(
        &autofill,
        "Excluded websites",
        Some("Sites where you chose never to save."),
        Some(exclusions.upcast_ref()),
    );
    let saved = card(&page, "Saved passwords");
    command(
        &saved,
        "Saved accounts",
        "Search, view, edit, and delete.",
        "Manage…",
        Action::Accounts,
    );
    let security = card(&page, "Security");
    let status = gtk::Label::new(None);
    status.add_css_class("settings-row-sub");
    let toggle_lock = button("", "tonal-btn");
    let state = ctx.app.passwords.status();
    state
        .bind_property("label", &status, "label")
        .sync_create()
        .build();
    state
        .bind_property("label", &toggle_lock, "label")
        .transform_to(|_, s: &str| Some(if s == "Unlocked" { "Lock" } else { "Unlock…" }))
        .sync_create()
        .build();
    let context = ctx.clone();
    toggle_lock.connect_clicked(move |_| {
        if let Some(parent) = context.parent() {
            let action = if context.app.passwords.unlocked() {
                Action::Lock
            } else {
                Action::Unlock
            };
            passwords::action(context.app.clone(), parent, action)
        }
    });
    let vault = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    vault.append(&status);
    vault.append(&toggle_lock);
    row(
        &security,
        "Encrypted vault",
        Some("Revealing and copying a saved password requires your master password."),
        Some(vault.upcast_ref()),
    );
    command(
        &security,
        "Master password",
        "There is no recovery if you forget it.",
        "Change…",
        Action::Change,
    );
    // Presets, like "Unload inactive tabs after". A hand-edited value shows as the next preset up.
    let preset = |choices: &[u32], v: u32| choices.iter().position(|&c| c >= v).unwrap_or(choices.len() - 1);
    const LOCK: [u32; 7] = [0, 1, 5, 15, 30, 60, 120];
    let app = ctx.app.clone();
    dropdown_row(
        &security,
        "Auto-lock after inactivity",
        Some("Time since the vault was last used."),
        &[
            "Never",
            "1 minute",
            "5 minutes",
            "15 minutes",
            "30 minutes",
            "1 hour",
            "2 hours",
        ],
        preset(&LOCK, prefs.lock_minutes),
        move |i| {
            {
                let mut cfg = app.config.borrow_mut();
                cfg.passwords.lock_minutes = LOCK[i];
                cfg.save();
            }
            passwords::reset_timer(&app)
        },
    );
    let gen_group = card(&page, "Password generator");
    const LENGTH: [u32; 7] = [12, 16, 20, 24, 32, 48, 64];
    let app = ctx.app.clone();
    dropdown_row(
        &gen_group,
        "Length",
        Some("Characters per generated password."),
        &["12", "16", "20", "24", "32", "48", "64"],
        preset(&LENGTH, prefs.length),
        move |i| {
            let mut cfg = app.config.borrow_mut();
            cfg.passwords.length = LENGTH[i];
            cfg.save()
        },
    );
    toggle(&gen_group, "Uppercase letters", prefs.upper, |p, v| p.upper = v);
    toggle(&gen_group, "Lowercase letters", prefs.lower, |p, v| p.lower = v);
    toggle(&gen_group, "Digits", prefs.digits, |p, v| p.digits = v);
    toggle(&gen_group, "Symbols", prefs.symbols, |p, v| p.symbols = v);
    toggle(
        &gen_group,
        "Avoid ambiguous characters",
        prefs.avoid_ambiguous,
        |p, v| p.avoid_ambiguous = v,
    );
    command(
        &gen_group,
        "Generate a password",
        "Uses the settings above.",
        "Generate…",
        Action::Generate,
    );
    let data = card(&page, "Data");
    command(
        &data,
        "Delete all saved passwords",
        "Permanently removes every account from the vault.",
        "Delete all…",
        Action::Delete,
    );
    page.upcast()
}
