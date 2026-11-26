//! Disk images in New Machine and Settings: the file picker, how an image is
//! kept, the OS badge and the progress of a copy.

use crate::app::App;
use adw::prelude::*;
use gtk::glib;
use origami_core::facade::UiString;
use origami_core::facade::{
    disk_labels, image_verdict, size_label, DiskImageCheck, Operation, OperationState,
};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    /// Answers the self-test queued for the picker; a native dialog cannot be driven headless.
    static PICKS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

pub fn queue_pick(path: PathBuf) {
    PICKS.with(|p| p.borrow_mut().push(path));
}

/// The one place a disk image is chosen.
pub fn pick_path(parent: &impl IsA<gtk::Window>, title: &str, then: impl Fn(PathBuf) + 'static) {
    if let Some(path) = PICKS.with(|p| p.borrow_mut().pop()) {
        return then(path);
    }
    let chooser = gtk::FileDialog::builder().title(title).build();
    chooser.open(Some(parent), gtk::gio::Cancellable::NONE, move |r| {
        if let Some(path) = r.ok().and_then(|f| f.path()) {
            then(path);
        }
    });
}

/// The one place a folder is chosen; the self-test answers it through the same queue.
pub fn pick_folder(parent: &impl IsA<gtk::Window>, title: &str, then: impl Fn(PathBuf) + 'static) {
    if let Some(path) = PICKS.with(|p| p.borrow_mut().pop()) {
        return then(path);
    }
    let chooser = gtk::FileDialog::builder().title(title).build();
    chooser.select_folder(Some(parent), gtk::gio::Cancellable::NONE, move |r| {
        if let Some(path) = r.ok().and_then(|f| f.path()) {
            then(path);
        }
    });
}

/// A small grey label naming the OS on a disk.
pub fn badge(name: &str) -> gtk::Label {
    let b = gtk::Label::builder()
        .label(UiString::Reading.text())
        .css_classes(["caption", "dim-label"])
        .valign(gtk::Align::Center)
        .build();
    b.set_widget_name(name);
    b
}

/// Show a long operation's progress in `bar` and `text` until it ends, with a Cancel button.
/// `done` runs on the UI thread with the final state.
pub fn follow(
    op: Operation,
    bar: &gtk::ProgressBar,
    text: &gtk::Label,
    cancel: &gtk::Button,
    done: impl Fn(OperationState) + 'static,
) {
    let (bar, text, cancel) = (bar.clone(), text.clone(), cancel.clone());
    for w in [
        bar.upcast_ref::<gtk::Widget>(),
        text.upcast_ref(),
        cancel.upcast_ref(),
    ] {
        w.set_visible(true);
    }
    let stop = op.clone();
    cancel.connect_clicked(move |_| {
        stop.cancel();
    });
    glib::timeout_add_local(Duration::from_millis(200), move || {
        let state = op.state();
        if state.is_finished() {
            done(state);
            return glib::ControlFlow::Break;
        }
        bar.set_fraction(op.fraction().unwrap_or(0.0) as f64);
        text.set_label(&op.progress());
        glib::ControlFlow::Continue
    });
}

#[derive(Default)]
struct Picked {
    path: Option<PathBuf>,
    check: Option<DiskImageCheck>,
    copy: bool,
    apart: bool,
}

/// The questions about one image: where it lives and how a CHD keeps changes.
pub struct ImageForm {
    pub group: adw::PreferencesGroup,
    file: adw::ActionRow,
    badge: gtk::Label,
    keep: adw::ComboRow,
    note: gtk::Label,
    picked: Rc<RefCell<Picked>>,
    app: Rc<App>,
    machine: Option<String>,
    changed: Rc<dyn Fn(bool)>,
}

impl ImageForm {
    /// `machine` is the machine the image is for, once it exists. `changed`
    /// hears whether `request` has an answer, each time that may change.
    pub fn new(
        app: &Rc<App>,
        machine: Option<String>,
        changed: impl Fn(bool) + 'static,
    ) -> Rc<ImageForm> {
        let labels = disk_labels();
        let group = adw::PreferencesGroup::builder()
            .description(&labels.same_kind)
            .build();
        let file = adw::ActionRow::builder().title("No image chosen").build();
        let badge = badge("disk.badge");
        badge.set_visible(false);
        file.add_suffix(&badge);
        let place = adw::ComboRow::builder()
            .title(UiString::WhereShouldTheDiskLive.text())
            .model(&gtk::StringList::new(
                &labels.place.iter().map(String::as_str).collect::<Vec<_>>(),
            ))
            .build();
        place.set_widget_name("disk.place");
        let keep = adw::ComboRow::builder()
            .title(UiString::HowShouldChangesBeKept.text())
            .model(&gtk::StringList::new(
                &labels.keep.iter().map(String::as_str).collect::<Vec<_>>(),
            ))
            .visible(false)
            .build();
        keep.set_widget_name("disk.keep");
        let note = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        note.set_widget_name("disk.note");
        for w in [
            file.upcast_ref::<gtk::Widget>(),
            place.upcast_ref(),
            keep.upcast_ref(),
            note.upcast_ref(),
        ] {
            group.add(w);
        }
        let form = Rc::new(ImageForm {
            group,
            file,
            badge,
            keep,
            note,
            picked: Rc::new(RefCell::new(Picked {
                copy: true,
                apart: true,
                ..Default::default()
            })),
            app: app.clone(),
            machine,
            changed: Rc::new(changed),
        });
        let f = form.clone();
        place.connect_selected_notify(move |p| {
            f.picked.borrow_mut().copy = p.selected() == 0;
            f.explain();
        });
        let f = form.clone();
        form.keep.connect_selected_notify(move |k| {
            f.picked.borrow_mut().apart = k.selected() == 0;
        });
        form
    }

    /// Choose a file, then read it.
    pub fn choose(self: &Rc<Self>, parent: &impl IsA<gtk::Window>) {
        let f = self.clone();
        pick_path(parent, "Choose a disk image", move |path| f.set_path(path));
    }

    fn set_path(self: &Rc<Self>, path: PathBuf) {
        self.file.set_title(
            &path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        );
        self.file.set_subtitle("");
        self.badge.set_label(UiString::Reading.text());
        self.badge.set_visible(true);
        {
            let mut picked = self.picked.borrow_mut();
            (picked.path, picked.check) = (Some(path.clone()), None);
        }
        let (core, machine, p) = (self.app.core.clone(), self.machine.clone(), path);
        let chosen = p.clone();
        let f = self.clone();
        glib::spawn_future_local(async move {
            let r = gtk::gio::spawn_blocking(move || {
                let check = core
                    .check_disk_image(&p, machine.as_deref())
                    .map_err(|e| e.to_string())?;
                let os = core.disk_os(&p).ok().flatten();
                Ok::<_, String>((check, os))
            })
            .await;
            // Another file was chosen while this one was read.
            if f.picked.borrow().path.as_ref() != Some(&chosen) {
                return;
            }
            match r {
                Ok(Ok((check, os))) => {
                    f.file.set_subtitle(&size_label(check.bytes));
                    f.badge.set_visible(os.is_some());
                    f.badge.set_label(os.as_deref().unwrap_or_default());
                    f.keep.set_visible(check.is_chd);
                    f.picked.borrow_mut().check = Some(check);
                }
                Ok(Err(e)) => {
                    f.badge.set_visible(false);
                    f.note.set_label(&e);
                    f.note.set_css_classes(&["error"]);
                    f.note.set_visible(true);
                    return (f.changed)(false);
                }
                Err(_) => return,
            }
            f.explain();
        });
    }

    /// The line under the choices, and whether Add may go ahead.
    fn explain(&self) {
        let p = self.picked.borrow();
        let Some(c) = &p.check else { return };
        let v = image_verdict(c, p.copy);
        let text = v.note.unwrap_or_default();
        let class = match (v.warning, v.ready) {
            (false, _) => "dim-label",
            (true, true) => "warning",
            (true, false) => "error",
        };
        self.note.set_label(&text);
        self.note.set_css_classes(&[class]);
        self.note.set_visible(!text.is_empty());
        drop(p);
        (self.changed)(self.request().is_some());
    }

    /// The image to add, whether to copy it and whether a CHD keeps changes apart; None
    /// until a file that can be added is chosen.
    pub fn request(&self) -> Option<(PathBuf, bool, bool)> {
        let p = self.picked.borrow();
        let c = p.check.as_ref()?;
        image_verdict(c, p.copy)
            .ready
            .then(|| (p.path.clone().unwrap(), p.copy, p.apart && c.is_chd))
    }
}
