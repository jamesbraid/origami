//! Menu and toolbar commands as data: what a front end builds its menus from.
//! Titles are English; a front end localises by `id`.

use super::machine::MachineState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}

/// When a command is available, in terms of the selected machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnabledWhen {
    Always,
    /// A machine is selected, in any state.
    MachineSelected,
    /// Selected and not running: `NotRunning` or `Exited`.
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
            (EnabledWhen::MachineStopped, Some(s)) => matches!(s, NotRunning | Exited),
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
    pub enabled_when: EnabledWhen,
    pub shortcuts: Vec<Shortcut>,
}

fn command(id: &str, title: &str, enabled_when: EnabledWhen, keys: [&str; 3]) -> Command {
    let platforms = [Platform::Macos, Platform::Windows, Platform::Linux];
    Command {
        id: id.into(),
        title: title.into(),
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
    use EnabledWhen::*;
    vec![
        command(
            "machine.new",
            "New Machine",
            Always,
            ["Cmd+N", "Ctrl+N", "Ctrl+N"],
        ),
        command(
            "machine.start",
            "Start",
            MachineStopped,
            ["Cmd+R", "Ctrl+R", "Ctrl+R"],
        ),
        command(
            "machine.stop",
            "Stop",
            MachineLive,
            ["Cmd+.", "Ctrl+Shift+S", "Ctrl+Shift+S"],
        ),
        command(
            "machine.reset",
            "Reset",
            MachineRunning,
            ["Cmd+Shift+R", "Ctrl+Shift+R", "Ctrl+Shift+R"],
        ),
        command(
            "machine.pause",
            "Pause",
            MachineRunning,
            ["Cmd+P", "Ctrl+P", "Ctrl+P"],
        ),
        command(
            "machine.resume",
            "Resume",
            MachinePaused,
            ["Cmd+P", "Ctrl+P", "Ctrl+P"],
        ),
        command(
            "machine.release-keys",
            "Release Keys",
            MachineLive,
            ["Cmd+Shift+K", "Ctrl+Shift+K", "Ctrl+Shift+K"],
        ),
        command(
            "machine.screenshot",
            "Screenshot",
            MachineRunning,
            ["Cmd+Shift+3", "Ctrl+Shift+P", "Ctrl+Shift+P"],
        ),
        command("app.quit", "Quit", Always, ["Cmd+Q", "Alt+F4", "Ctrl+Q"]),
    ]
}
