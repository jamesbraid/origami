//! The Acknowledgements and Licences sheets About opens, from the shared
//! credits document (`origami-credits`).

use adw::prelude::*;
use gtk::glib;
use origami_credits::credits;
use std::cell::Cell;
use std::rc::Rc;

fn failed(parent: &impl IsA<gtk::Widget>, title: &str, why: &str) {
    adw::Dialog::builder()
        .title(title)
        .child(&gtk::Label::new(Some(why)))
        .build()
        .present(Some(parent));
}

fn link(url: &str, label: &str) -> gtk::LinkButton {
    let b = gtk::LinkButton::with_label(url, label);
    b.set_valign(gtk::Align::Center);
    // Every link label is as wide as the longest word, left aligned, so "Source" lines up with "Website".
    if let Some(l) = b.first_child().and_downcast::<gtk::Label>() {
        l.set_width_chars(8);
        l.set_xalign(0.0);
    }
    b
}

fn row(title: &str, subtitle: &str, links: &[(&str, &str)]) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(title))
        .subtitle(glib::markup_escape_text(subtitle))
        .subtitle_selectable(true)
        .build();
    for (url, label) in links.iter().filter(|(url, _)| !url.is_empty()) {
        row.add_suffix(&link(url, label));
    }
    row
}

fn group(title: &str, rows: Vec<adw::ActionRow>) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    for r in rows {
        g.add(&r);
    }
    g
}

fn sheet(title: &str, width: i32, content: &impl IsA<gtk::Widget>) -> adw::Dialog {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(content));
    adw::Dialog::builder()
        .title(title)
        .content_width(width)
        .content_height(620)
        .child(&toolbar)
        .build()
}

/// Built on, thanks, photographs and artwork.
pub fn acknowledgements(parent: &impl IsA<gtk::Widget>) {
    let c = match credits() {
        Ok(c) => c,
        Err(e) => return failed(parent, "Acknowledgements", &e),
    };
    let page = adw::PreferencesPage::new();
    let mut built: Vec<_> = c
        .built_on
        .iter()
        .map(|p| {
            row(
                &p.name,
                &format!("{}. {}", p.by, p.note),
                &[(&p.url, "Website")],
            )
        })
        .collect();
    built.insert(
        1,
        row(
            &format!("QEMU source, {}", c.qemu.tag),
            "The exact source of the bundled build.",
            &[(&c.qemu.url, "Source")],
        ),
    );
    page.add(&group("Built on", built));
    page.add(&group(
        "Thanks",
        c.thanks
            .iter()
            .map(|t| row(&t.name, &t.reason, &[(&t.url, "Website")]))
            .collect(),
    ));
    page.add(&group(
        "Photographs",
        c.photographs
            .iter()
            .map(|p| {
                let by = format!("{}, {}. {}.", p.author, p.year, p.changes);
                row(
                    &p.title,
                    &by,
                    &[(&p.licence_url, &p.licence), (&p.source, "Source")],
                )
            })
            .collect(),
    ));
    let art = group(
        "Artwork",
        c.artwork
            .iter()
            .map(|a| row(&a.name, &a.from, &[(&a.url, "Source")]))
            .collect(),
    );
    art.set_description(Some(&glib::markup_escape_text(&c.trademark)));
    page.add(&art);
    sheet("Acknowledgements", 520, &page).present(Some(parent));
}

fn truncated(components: &[String]) -> String {
    let mut out = String::new();
    for (i, c) in components.iter().enumerate() {
        if out.len() + c.len() > 70 && i > 0 {
            return format!("{out} and {} more", components.len() - i);
        }
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(c);
    }
    out
}

/// One licence text, built when its row first opens.
fn text_row(components: &[String], text: &str) -> gtk::Box {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    let label = |text: &str, css: &[&str]| {
        gtk::Label::builder()
            .label(text)
            .css_classes(css.to_vec())
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .xalign(0.0)
            .build()
    };
    if components.len() > 1 {
        body.append(&label(&components.join(", "), &["caption", "dim-label"]));
    }
    body.append(&label(text, &["caption", "monospace"]));
    body
}

/// The full text of every bundled licence, grouped, with a search box.
pub fn licences(parent: &impl IsA<gtk::Widget>) {
    let c = match credits() {
        Ok(c) => c,
        Err(e) => return failed(parent, "Licences", &e),
    };
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search components and licences")
        .build();
    page.append(&search);
    let qemu = adw::PreferencesGroup::builder()
        .title("QEMU")
        .description(glib::markup_escape_text(&c.qemu.note))
        .build();
    qemu.add(&row(
        &format!("Source at {}", c.qemu.tag),
        &c.qemu.url,
        &[(&c.qemu.url, "Open")],
    ));
    page.append(&qemu);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    // Each licence's row, and under it its entries with the text they hold lower-cased to search.
    let mut groups: Vec<(adw::ExpanderRow, Vec<(adw::ExpanderRow, String)>)> = Vec::new();
    for licence in &c.licences {
        let outer = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(&licence.name))
            .subtitle(
                match licence
                    .entries
                    .iter()
                    .map(|e| e.components.len())
                    .sum::<usize>()
                {
                    1 => "1 component".to_string(),
                    n => format!("{n} components"),
                },
            )
            .build();
        let mut entries = Vec::new();
        for entry in &licence.entries {
            let inner = adw::ExpanderRow::builder()
                .title(glib::markup_escape_text(&truncated(&entry.components)))
                .build();
            let built = Rc::new(Cell::new(false));
            let (components, text) = (entry.components.clone(), entry.text.clone());
            inner.connect_expanded_notify(move |r| {
                if r.is_expanded() && !built.replace(true) {
                    let holder = adw::ActionRow::new();
                    holder.set_activatable(false);
                    holder.set_child(Some(&text_row(&components, &text)));
                    r.add_row(&holder);
                }
            });
            outer.add_row(&inner);
            let haystack =
                format!("{} {}", licence.name, entry.components.join(" ")).to_lowercase();
            entries.push((inner, haystack));
        }
        list.append(&outer);
        groups.push((outer, entries));
    }
    search.connect_search_changed(move |s| {
        let query = s.text().to_lowercase();
        for (outer, entries) in &groups {
            let mut any = false;
            for (inner, haystack) in entries {
                let hit = haystack.contains(&query);
                inner.set_visible(hit);
                any |= hit;
            }
            outer.set_visible(any);
            outer.set_expanded(any && !query.is_empty());
        }
    });
    page.append(&list);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build();
    sheet("Licences", 560, &scroll).present(Some(parent));
}
