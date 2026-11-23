//! A settings dialog with a sidebar of pages, the way GNOME's own settings
//! windows are laid out. `AdwPreferencesDialog` puts every page in a bottom
//! switcher that cuts long names ("Processor & Memory") to fit; a sidebar gives
//! each name its own row, and collapses to a list that opens a page when narrow.

use adw::prelude::*;
use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;

#[derive(Clone)]
pub struct Prefs {
    pub dialog: adw::Dialog,
    split: adw::NavigationSplitView,
    stack: adw::ViewStack,
    list: gtk::ListBox,
    toasts: adw::ToastOverlay,
    pages: Rc<RefCell<Vec<(gtk::ListBoxRow, adw::PreferencesPage)>>>,
}

impl Deref for Prefs {
    type Target = adw::Dialog;
    fn deref(&self) -> &adw::Dialog {
        &self.dialog
    }
}

/// A widget name from a page title: "Startup & Firmware" is "startup-firmware".
pub fn slug(title: &str) -> String {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

impl Prefs {
    pub fn new(title: &str) -> Self {
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(760)
            .content_height(560)
            .build();
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        let side = adw::ToolbarView::new();
        let side_head = adw::HeaderBar::new();
        side_head.set_show_end_title_buttons(false);
        side.add_top_bar(&side_head);
        side.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&list)
                .build(),
        ));
        let side_page = adw::NavigationPage::builder()
            .title(title)
            .child(&side)
            .build();

        let stack = adw::ViewStack::new();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        let body = adw::ToolbarView::new();
        body.add_top_bar(&adw::HeaderBar::new());
        body.set_content(Some(&toasts));
        let content = adw::NavigationPage::builder()
            .title(title)
            .child(&body)
            .build();

        let split = adw::NavigationSplitView::builder()
            .sidebar(&side_page)
            .content(&content)
            .min_sidebar_width(230.0)
            .max_sidebar_width(260.0)
            .build();
        // Narrow: the sidebar becomes the first screen and a page opens over it.
        let bin = adw::BreakpointBin::new();
        bin.set_width_request(360);
        bin.set_height_request(300);
        bin.set_child(Some(&split));
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            560.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        bin.add_breakpoint(narrow);
        dialog.set_child(Some(&bin));

        let pages: Rc<RefCell<Vec<(gtk::ListBoxRow, adw::PreferencesPage)>>> = Rc::default();
        {
            let (stack, content, pages) = (stack.clone(), content.clone(), pages.clone());
            list.connect_row_selected(move |_, row| {
                let Some(row) = row else { return };
                if let Some((_, page)) = pages.borrow().iter().find(|(r, _)| r == row) {
                    stack.set_visible_child(page);
                    content.set_title(&page.title());
                }
            });
        }
        {
            let split = split.clone();
            list.connect_row_activated(move |_, _| split.set_show_content(true));
        }
        Prefs {
            dialog,
            split,
            stack,
            list,
            toasts,
            pages,
        }
    }

    pub fn add(&self, page: &adw::PreferencesPage) {
        let title = page.title().to_string();
        self.stack.add_titled(page, None, &title);
        let line = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(6)
            .margin_end(6)
            .build();
        if let Some(icon) = page.icon_name() {
            line.append(&gtk::Image::from_icon_name(&icon));
        }
        line.append(
            &gtk::Label::builder()
                .label(&title)
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .build(),
        );
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&line));
        row.set_widget_name(&format!("prefs.page.{}", slug(&title)));
        self.list.append(&row);
        let first = self.pages.borrow().is_empty();
        self.pages.borrow_mut().push((row.clone(), page.clone()));
        if first {
            self.list.select_row(Some(&row));
        }
    }

    pub fn add_toast(&self, toast: adw::Toast) {
        self.toasts.add_toast(toast);
    }

    pub fn set_visible_page(&self, page: &adw::PreferencesPage) {
        let row = self
            .pages
            .borrow()
            .iter()
            .find(|(_, p)| p == page)
            .map(|(r, _)| r.clone());
        if let Some(row) = row {
            self.list.select_row(Some(&row));
            self.split.set_show_content(true);
        }
    }
}
