//! The words of alerts, toasts and empty states, which take a machine's name,
//! a count or the platform. The apps lay them out and write nothing of their own.

use super::commands::Platform;
use super::shown::show_in_label;
use super::ui_text::UiString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonRole {
    /// The answer that loses nothing; the default.
    Safe,
    Destructive,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlertButton {
    pub label: String,
    pub role: ButtonRole,
}

/// Buttons are in the order to show them, the safe one first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub title: String,
    pub body: String,
    pub buttons: Vec<AlertButton>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlertKind {
    /// Quit with these machines running.
    QuitRunning {
        names: Vec<String>,
    },
    PowerOff {
        name: String,
    },
    Reset {
        name: String,
    },
    ForceQuit {
        name: String,
    },
    CloseTab {
        name: String,
    },
    /// A clean stop that did not finish; the answer is Force quit.
    PowerOffFailed {
        names: Vec<String>,
    },
    /// Closing a window that holds these running machines.
    CloseWindow {
        names: Vec<String>,
    },
    /// `disk_left` are disk images that stay where they are.
    Remove {
        name: String,
        disk_left: Vec<String>,
    },
}

/// Where a removed machine goes, as the platform names it.
pub fn trash_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "Recycle Bin",
        _ => "Trash",
    }
}

pub fn alert(kind: &AlertKind, platform: Platform) -> Alert {
    use ButtonRole::*;
    let button = |label: &str, role| AlertButton {
        label: label.into(),
        role,
    };
    let cancel = || button(UiString::Cancel.text(), Cancel);
    let (title, mut body, buttons) = match kind {
        AlertKind::QuitRunning { names } if names.len() == 1 => (
            format!("Quit Origami while {} is running?", names[0]),
            format!(
                "{} can keep running without Origami, and you can reconnect later.",
                names[0]
            ),
            vec![
                button(UiString::KeepRunning.text(), Safe),
                button(UiString::PowerOff.text(), Destructive),
                cancel(),
            ],
        ),
        AlertKind::QuitRunning { names } => (
            format!("Quit Origami while {} machines are running?", names.len()),
            "They can keep running without Origami, and you can reconnect later.".into(),
            vec![
                button(UiString::KeepRunning.text(), Safe),
                button("Power off all", Destructive),
                cancel(),
            ],
        ),
        AlertKind::PowerOff { name } => (
            format!("Power off {name}?"),
            "This is like pulling the power cord, so shut down IRIX first if it is running."
                .into(),
            vec![cancel(), button(UiString::PowerOff.text(), Destructive)],
        ),
        AlertKind::Reset { name } => (
            format!("Reset {name}?"),
            format!("{name} restarts as if its reset button was pressed."),
            vec![cancel(), button("Reset", Destructive)],
        ),
        AlertKind::ForceQuit { name } => (
            format!("Force quit {name}?"),
            "Changes not yet saved to its disk may be lost.".into(),
            vec![cancel(), button("Force quit", Destructive)],
        ),
        AlertKind::PowerOffFailed { names } => (
            match names.as_slice() {
                [one] => format!("{one} did not power off"),
                many => format!("{} machines did not power off", many.len()),
            },
            "A clean stop can take a while when a disk is saving its changes. Force quit ends the machine at once, and changes not yet saved to its disk may be lost."
                .into(),
            vec![cancel(), button("Force quit", Destructive)],
        ),
        AlertKind::CloseWindow { names } if names.len() == 1 => {
            return alert(&AlertKind::CloseTab { name: names[0].clone() }, platform);
        }
        AlertKind::CloseWindow { names } => (
            format!("Close this window while {} machines are running?", names.len()),
            "They can keep running without a window, and you can reconnect later.".into(),
            vec![
                button(UiString::KeepRunning.text(), Safe),
                button("Power off all", Destructive),
                cancel(),
            ],
        ),
        AlertKind::CloseTab { name } => (
            format!("Close {name}'s tab?"),
            format!("{name} can keep running without a tab."),
            vec![
                button(UiString::KeepRunning.text(), Safe),
                button(UiString::PowerOff.text(), Destructive),
                cancel(),
            ],
        ),
        AlertKind::Remove { name, .. } if platform == Platform::Windows => (
            format!("Delete {name}?"),
            "Its disks, firmware settings and everything in its folder go to the Recycle Bin, where you can restore them."
                .into(),
            vec![cancel(), button("Delete", Destructive)],
        ),
        AlertKind::Remove { name, .. } => (
            format!("Move {name} to the Trash?"),
            "Its disks, firmware settings and everything in its folder go with it. You can put it back from the Trash."
                .into(),
            vec![cancel(), button("Move to Trash", Destructive)],
        ),
    };
    if let AlertKind::Remove { disk_left, .. } = kind {
        match disk_left.as_slice() {
            [] => {}
            [file] => body.push_str(&format!(" Its disk image stays where it is: {file}.")),
            files => body.push_str(&format!(
                " Its disk images stay where they are: {}.",
                files.join(", ")
            )),
        }
    }
    Alert {
        title,
        body,
        buttons,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Upgraded {
        name: String,
    },
    Removed {
        name: String,
    },
    ScreenshotSaved,
    DiscEjected,
    /// The picture was resized to `name`, such as `Fit`.
    PictureSize {
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    /// A button on the toast, such as `Show in Finder`.
    pub action: Option<String>,
}

pub fn toast(kind: &ToastKind, platform: Platform) -> Toast {
    let (text, action) = match kind {
        ToastKind::Upgraded { name } => (format!("{name} upgraded"), None),
        ToastKind::Removed { name } => (
            format!("{name} moved to the {}", trash_name(platform)),
            None,
        ),
        ToastKind::ScreenshotSaved => ("Screenshot saved".into(), Some(show_in_label(platform))),
        ToastKind::DiscEjected => ("Disc ejected".into(), None),
        ToastKind::PictureSize { name } => (format!("Picture size: {name}"), None),
    };
    Toast { text, action }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyKind {
    NoMachines,
    NoneSelected,
    NoSharedFolders,
    NoDownloadedDisks,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmptyState {
    pub title: &'static str,
    pub sentence: &'static str,
    pub button: Option<&'static str>,
}

pub fn empty_state(kind: EmptyKind) -> EmptyState {
    let (title, sentence, button) = match kind {
        EmptyKind::NoMachines => (
            "No machines yet",
            "Create one to start using Origami.",
            Some(UiString::NewMachine.text()),
        ),
        EmptyKind::NoneSelected => ("Choose a machine", "Pick one in the library.", None),
        EmptyKind::NoSharedFolders => (
            "No shared folders",
            "Share a folder to use it from IRIX.",
            Some(UiString::ShareAFolder.text()),
        ),
        EmptyKind::NoDownloadedDisks => (
            UiString::NoDownloadedDisks.text(),
            "Ready-made disks you download appear here.",
            None,
        ),
    };
    EmptyState {
        title,
        sentence,
        button,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(a: &Alert) -> Vec<&str> {
        a.buttons.iter().map(|b| b.label.as_str()).collect()
    }

    #[test]
    fn quitting_with_one_or_many_running_names_what_stays() {
        let one = alert(
            &AlertKind::QuitRunning {
                names: vec!["fuel".into()],
            },
            Platform::Macos,
        );
        assert_eq!(one.title, "Quit Origami while fuel is running?");
        assert_eq!(labels(&one), ["Keep running", "Power off", "Cancel"]);
        assert_eq!(one.buttons[0].role, ButtonRole::Safe);
        let many = alert(
            &AlertKind::QuitRunning {
                names: vec!["a".into(), "b".into()],
            },
            Platform::Macos,
        );
        assert_eq!(many.title, "Quit Origami while 2 machines are running?");
        assert_eq!(labels(&many)[1], "Power off all");
    }

    #[test]
    fn the_destructive_alerts_put_cancel_first() {
        let n = || "fuel".to_string();
        for kind in [
            AlertKind::PowerOff { name: n() },
            AlertKind::Reset { name: n() },
            AlertKind::ForceQuit { name: n() },
        ] {
            let a = alert(&kind, Platform::Linux);
            assert_eq!(a.buttons[0].role, ButtonRole::Cancel, "{kind:?}");
            assert_eq!(a.buttons[1].role, ButtonRole::Destructive, "{kind:?}");
        }
        let tab = alert(&AlertKind::CloseTab { name: n() }, Platform::Linux);
        assert_eq!(tab.title, "Close fuel's tab?");
    }

    #[test]
    fn removing_says_trash_or_recycle_bin_and_any_disk_left_behind() {
        let kind = AlertKind::Remove {
            name: "fuel".into(),
            disk_left: vec!["/disks/irix.chd".into()],
        };
        let mac = alert(&kind, Platform::Macos);
        assert_eq!(mac.title, "Move fuel to the Trash?");
        assert_eq!(labels(&mac), ["Cancel", "Move to Trash"]);
        assert!(mac.body.ends_with("stays where it is: /disks/irix.chd."));
        let two = AlertKind::Remove {
            name: "fuel".into(),
            disk_left: vec!["/a.img".into(), "/b.img".into()],
        };
        assert!(alert(&two, Platform::Linux)
            .body
            .ends_with("Its disk images stay where they are: /a.img, /b.img."));
        let win = alert(&kind, Platform::Windows);
        assert_eq!(win.title, "Delete fuel?");
        assert_eq!(labels(&win), ["Cancel", "Delete"]);
        assert!(win.body.contains("Recycle Bin"));
    }

    #[test]
    fn closing_a_window_with_several_running_machines_names_how_many() {
        let two = alert(
            &AlertKind::CloseWindow {
                names: vec!["a".into(), "b".into()],
            },
            Platform::Linux,
        );
        assert_eq!(two.title, "Close this window while 2 machines are running?");
        assert_eq!(labels(&two), ["Keep running", "Power off all", "Cancel"]);
        let one = alert(
            &AlertKind::CloseWindow {
                names: vec!["a".into()],
            },
            Platform::Linux,
        );
        assert_eq!(one.title, "Close a's tab?");
    }

    #[test]
    fn a_power_off_that_did_not_finish_offers_force_quit() {
        let one = alert(
            &AlertKind::PowerOffFailed {
                names: vec!["Fuel".into()],
            },
            Platform::Linux,
        );
        assert_eq!(one.title, "Fuel did not power off");
        assert_eq!(labels(&one), ["Cancel", "Force quit"]);
        let two = AlertKind::PowerOffFailed {
            names: vec!["a".into(), "b".into()],
        };
        assert_eq!(
            alert(&two, Platform::Linux).title,
            "2 machines did not power off"
        );
    }

    #[test]
    fn toasts_name_the_platforms_own_places() {
        assert_eq!(
            toast(
                &ToastKind::PictureSize { name: "Fit".into() },
                Platform::Linux
            )
            .text,
            "Picture size: Fit"
        );
        let removed = ToastKind::Removed {
            name: "fuel".into(),
        };
        assert_eq!(
            toast(&removed, Platform::Windows).text,
            "fuel moved to the Recycle Bin"
        );
        assert_eq!(
            toast(&ToastKind::ScreenshotSaved, Platform::Linux)
                .action
                .as_deref(),
            Some("Show in Files")
        );
        assert_eq!(toast(&ToastKind::DiscEjected, Platform::Linux).action, None);
    }

    #[test]
    fn an_empty_library_offers_a_new_machine() {
        assert_eq!(
            empty_state(EmptyKind::NoMachines).button,
            Some("New machine")
        );
        assert_eq!(empty_state(EmptyKind::NoneSelected).button, None);
    }
}
