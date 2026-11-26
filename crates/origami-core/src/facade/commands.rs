//! Menu and toolbar commands as data: what a front end builds its menus from.
//! Titles are English; a front end localises by `id`.

use super::machine::MachineState;
use super::ui_text::UiString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}

impl Platform {
    /// The platform this build runs on.
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Macos
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// When a command is available, in terms of the selected machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnabledWhen {
    Always,
    /// A machine is selected, in any state.
    MachineSelected,
    /// Selected and this core is not running it: `NotRunning`, `Exited` or
    /// `LeftRunning` (where start attaches).
    MachineStopped,
    /// Selected and running, paused or stopping.
    MachineLive,
    /// Selected and `Running`.
    MachineRunning,
    MachinePaused,
}

impl EnabledWhen {
    /// Whether the command is available when the selected machine is in
    /// `state`, or no machine is selected (None).
    pub fn holds(self, state: Option<MachineState>) -> bool {
        use MachineState::*;
        match (self, state) {
            (EnabledWhen::Always, _) => true,
            (_, None) => false,
            (EnabledWhen::MachineSelected, Some(_)) => true,
            (EnabledWhen::MachineStopped, Some(s)) => {
                matches!(s, NotRunning | Exited | LeftRunning)
            }
            (EnabledWhen::MachineLive, Some(s)) => matches!(s, Running | Paused | Stopping),
            (EnabledWhen::MachineRunning, Some(s)) => s == Running,
            (EnabledWhen::MachinePaused, Some(s)) => s == Paused,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub platform: Platform,
    /// `Cmd+N`, `Ctrl+Shift+Q`: modifiers, then one key, joined by `+`.
    pub accelerator: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// Stable: `machine.start`.
    pub id: String,
    pub title: String,
    /// What it does, in a few words; [`Command::tooltip`] adds its shortcut.
    pub help: String,
    pub enabled_when: EnabledWhen,
    pub shortcuts: Vec<Shortcut>,
}

impl Command {
    /// The tooltip, ending with the shortcut where `platform` has one.
    pub fn tooltip(&self, platform: Platform) -> String {
        match self.shortcuts.iter().find(|s| s.platform == platform) {
            Some(s) => format!("{} ({})", self.help, display_keys(platform, &s.accelerator)),
            None => self.help.clone(),
        }
    }
}

/// An accelerator as the platform writes it: `⌥⌘P` on macOS, as given elsewhere.
pub fn display_keys(platform: Platform, accelerator: &str) -> String {
    if platform != Platform::Macos {
        return accelerator.to_string();
    }
    let parts: Vec<&str> = accelerator.split('+').collect();
    let (key, mods) = parts.split_last().expect("split gives one part");
    let key = if *key == "Backspace" { "⌫" } else { key };
    let mut out = String::new();
    for (name, glyph) in [("Ctrl", "⌃"), ("Alt", "⌥"), ("Shift", "⇧"), ("Cmd", "⌘")] {
        if mods.contains(&name) {
            out.push_str(glyph);
        }
    }
    out + key
}

fn command(
    id: &str,
    title: &str,
    help: &str,
    enabled_when: EnabledWhen,
    keys: [&str; 3],
) -> Command {
    let platforms = [Platform::Macos, Platform::Windows, Platform::Linux];
    Command {
        id: id.into(),
        title: title.into(),
        help: help.into(),
        enabled_when,
        shortcuts: platforms
            .into_iter()
            .zip(keys)
            .filter(|(_, accelerator)| !accelerator.is_empty())
            .map(|(platform, accelerator)| Shortcut {
                platform,
                accelerator: accelerator.into(),
            })
            .collect(),
    }
}

pub(crate) fn table() -> Vec<Command> {
    commands_for(Platform::host())
}

/// The command table as `platform` words it, which differs only in Remove.
pub fn commands_for(platform: Platform) -> Vec<Command> {
    use EnabledWhen::*;
    let remove = if platform == Platform::Windows {
        ("Delete…", "Delete this machine")
    } else {
        ("Move to Trash…", "Move this machine to the Trash")
    };
    vec![
        command(
            "machine.new",
            "New machine",
            "Make a new machine",
            Always,
            ["Cmd+N", "Ctrl+N", "Ctrl+N"],
        ),
        command(
            "machine.start",
            "Start",
            "Start the machine",
            MachineStopped,
            ["Cmd+R", "Ctrl+Shift+R", "Ctrl+Shift+R"],
        ),
        // Not enabled while Starting: the engine's launch cannot be aborted and
        // there is no session to stop until it returns. Cancel the Start instead.
        command(
            "machine.stop",
            "Power off",
            "Power off the machine",
            MachineLive,
            ["Alt+Cmd+P", "Ctrl+Shift+P", "Ctrl+Shift+P"],
        ),
        // Force quit and reset have no shortcut: they always confirm.
        command(
            "machine.force-quit",
            "Force quit",
            "End the machine at once",
            MachineLive,
            ["", "", ""],
        ),
        command(
            "machine.reset",
            "Reset",
            "Restart the machine as if its reset button was pressed",
            MachineRunning,
            ["", "", ""],
        ),
        command(
            "machine.eject-cd",
            "Eject CD",
            "Eject the CD",
            MachineSelected,
            ["Cmd+E", "Ctrl+Shift+E", "Ctrl+Shift+E"],
        ),
        command(
            "window.console",
            "Console",
            "Open the console",
            MachineSelected,
            ["Alt+Cmd+C", "Ctrl+Shift+O", "Ctrl+Shift+O"],
        ),
        command(
            "window.diagnostics",
            "Diagnostics",
            "Show how the machine is doing",
            MachineSelected,
            ["Alt+Cmd+L", "Ctrl+Shift+L", "Ctrl+Shift+L"],
        ),
        command(
            "machine.settings",
            "Settings",
            "Change this machine's settings",
            MachineSelected,
            ["Cmd+I", "Ctrl+Shift+I", "Ctrl+Shift+I"],
        ),
        command(
            "machine.install-irix",
            "Install IRIX…",
            "Install IRIX from discs",
            MachineStopped,
            ["", "", ""],
        ),
        command(
            "machine.pause",
            "Pause",
            "Pause the machine",
            MachineRunning,
            ["Cmd+P", "Ctrl+P", "Ctrl+P"],
        ),
        command(
            "machine.resume",
            "Resume",
            "Resume the machine",
            MachinePaused,
            ["Cmd+P", "Ctrl+P", "Ctrl+P"],
        ),
        command(
            "machine.release-keys",
            "Release keys",
            "Let go of every key the machine thinks is held",
            MachineLive,
            ["Cmd+Shift+K", "Ctrl+Shift+K", "Ctrl+Shift+K"],
        ),
        command(
            "machine.capture-mouse",
            UiString::CaptureMouse.text(),
            "Send the mouse to the machine",
            MachineRunning,
            ["", "", ""],
        ),
        command(
            "machine.screenshot",
            "Screenshot",
            "Save a picture of the screen",
            MachineRunning,
            ["Cmd+Shift+3", "", ""],
        ),
        command(
            "machine.remove",
            remove.0,
            remove.1,
            MachineStopped,
            ["Cmd+Backspace", "Delete", "Delete"],
        ),
        command(
            "window.next-machine",
            "Show next machine",
            "Show the next open machine",
            Always,
            ["Ctrl+Tab", "", ""],
        ),
        command(
            "window.close-machine",
            "Close machine",
            "Close this machine's tab",
            MachineSelected,
            ["Cmd+W", "", ""],
        ),
        command(
            "view.shortcuts",
            UiString::KeyboardShortcuts.text(),
            "Show the keyboard shortcuts",
            Always,
            ["", "", "Ctrl+?"],
        ),
        command(
            "app.quit",
            "Quit",
            "Quit Origami",
            Always,
            ["Cmd+Q", "Alt+F4", "Ctrl+Q"],
        ),
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutItem {
    pub label: String,
    pub keys: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutGroup {
    pub title: String,
    pub items: Vec<ShortcutItem>,
}

/// The keyboard shortcuts window's content: the release keys first, then the
/// commands that have a shortcut on `platform`, then the console's keys.
pub fn shortcut_groups(platform: Platform) -> Vec<ShortcutGroup> {
    let table = commands_for(platform);
    let item = |label: &str, keys: &str| ShortcutItem {
        label: label.into(),
        keys: keys.into(),
    };
    let from = |ids: &[&str]| -> Vec<ShortcutItem> {
        ids.iter()
            .filter_map(|id| {
                let c = table.iter().find(|c| c.id == *id)?;
                let s = c.shortcuts.iter().find(|s| s.platform == platform)?;
                Some(item(
                    c.title.trim_end_matches('…'),
                    &display_keys(platform, &s.accelerator),
                ))
            })
            .collect()
    };
    let release = if platform == Platform::Macos {
        "⌃⌥"
    } else {
        "Ctrl+Alt"
    };
    let (copy, paste) = if platform == Platform::Macos {
        ("⌘C", "⌘V")
    } else {
        ("Ctrl+Shift+C", "Ctrl+Shift+V")
    };
    let group = |title: &str, items| ShortcutGroup {
        title: title.into(),
        items,
    };
    vec![
        group("Mouse", vec![item("Release the mouse", release)]),
        group(
            "Machine",
            from(&[
                "machine.start",
                "machine.stop",
                "machine.pause",
                "machine.eject-cd",
                "machine.screenshot",
                "machine.settings",
                "machine.remove",
            ]),
        ),
        group("View", from(&["view.shortcuts"])),
        group(
            "Window",
            from(&[
                "window.console",
                "window.diagnostics",
                "window.next-machine",
                "window.close-machine",
                "machine.new",
                "app.quit",
            ]),
        ),
        group(
            "Console",
            vec![
                item(UiString::Copy.text(), copy),
                item("Paste", paste),
                item("Scroll back", "Shift+Page Up"),
            ],
        ),
    ]
    .into_iter()
    .filter(|g| !g.items.is_empty())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_help_and_the_tooltip_ends_with_its_shortcut() {
        for c in commands_for(Platform::Macos) {
            assert!(!c.help.is_empty(), "{}", c.id);
        }
        let start = commands_for(Platform::Macos)
            .into_iter()
            .find(|c| c.id == "machine.start")
            .unwrap();
        assert_eq!(start.tooltip(Platform::Macos), "Start the machine (⌘R)");
        assert_eq!(
            start.tooltip(Platform::Linux),
            "Start the machine (Ctrl+Shift+R)"
        );
        let capture = commands_for(Platform::Linux)
            .into_iter()
            .find(|c| c.id == "machine.capture-mouse")
            .unwrap();
        assert_eq!(
            capture.tooltip(Platform::Linux),
            "Send the mouse to the machine"
        );
    }

    #[test]
    fn remove_is_move_to_trash_except_on_windows() {
        let title = |p| {
            commands_for(p)
                .into_iter()
                .find(|c| c.id == "machine.remove")
                .map(|c| (c.title, c.enabled_when))
                .unwrap()
        };
        assert_eq!(title(Platform::Macos).0, "Move to Trash…");
        assert_eq!(title(Platform::Linux).0, "Move to Trash…");
        assert_eq!(title(Platform::Windows).0, "Delete…");
        assert!(!title(Platform::Linux).1.holds(Some(MachineState::Running)));
        assert!(title(Platform::Linux)
            .1
            .holds(Some(MachineState::NotRunning)));
    }

    #[test]
    fn the_shortcuts_window_leads_with_the_release_keys() {
        let mac = shortcut_groups(Platform::Macos);
        assert_eq!(mac[0].title, "Mouse");
        assert_eq!(mac[0].items[0].keys, "⌃⌥");
        let titles: Vec<_> = mac.iter().map(|g| g.title.as_str()).collect();
        // The shortcuts command has no key on macOS, so View has nothing to list.
        assert_eq!(titles, ["Mouse", "Machine", "Window", "Console"]);
        let power = mac[1]
            .items
            .iter()
            .find(|i| i.label == "Power off")
            .unwrap();
        assert_eq!(power.keys, "⌥⌘P");
        let linux = shortcut_groups(Platform::Linux);
        let titles: Vec<_> = linux.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Mouse", "Machine", "View", "Window", "Console"]);
        assert_eq!(linux[2].items[0].keys, "Ctrl+?");
        assert_eq!(linux[0].items[0].keys, "Ctrl+Alt");
    }

    #[test]
    fn no_command_repeats_a_shortcut_on_one_platform_but_pause_and_resume() {
        for p in [Platform::Macos, Platform::Windows, Platform::Linux] {
            let mut seen = std::collections::HashMap::new();
            for c in commands_for(p) {
                for s in c.shortcuts.iter().filter(|s| s.platform == p) {
                    if let Some(other) = seen.insert(s.accelerator.clone(), c.id.clone()) {
                        let pair = [other.as_str(), c.id.as_str()];
                        assert!(
                            pair.contains(&"machine.pause") && pair.contains(&"machine.resume"),
                            "{p:?} {}: {other} and {}",
                            s.accelerator,
                            c.id
                        );
                    }
                }
            }
        }
    }
}
