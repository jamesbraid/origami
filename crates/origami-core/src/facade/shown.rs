//! What the library and a machine's page say about a machine: state words,
//! the library line, cards and the status bar. The apps write none of them.

use super::commands::Platform;
use super::core::MachineSummary;
use super::error::{CoreError, ErrorFix};
use super::machine::{ExitReason, MachineState, MachineView};
use super::ui_text::UiString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    None,
    Success,
    Caution,
    Critical,
}

/// The states a person tells apart: `MachineState` plus what only the library
/// or a view knows (the install guide, a blank screen, an unreadable machine).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShownState {
    Stopped,
    Starting,
    Running,
    Paused,
    Saving,
    LeftRunning,
    Installing,
    NoPicture,
    Exited,
    NeedsUpgrade,
    CantOpen,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateLabel {
    pub word: &'static str,
    pub tone: Tone,
}

pub fn state_label(state: ShownState) -> StateLabel {
    use ShownState::*;
    let (word, tone) = match state {
        Stopped => ("Stopped", Tone::None),
        Starting => ("Starting", Tone::Caution),
        Running => ("Running", Tone::Success),
        Paused => ("Paused", Tone::Caution),
        Saving => ("Saving", Tone::Caution),
        LeftRunning => ("Left running", Tone::Caution),
        Installing => ("Installing", Tone::Caution),
        NoPicture => ("No picture", Tone::Critical),
        Exited => ("Stopped unexpectedly", Tone::Critical),
        NeedsUpgrade => ("Needs upgrade", Tone::Caution),
        CantOpen => ("Can't open", Tone::Critical),
    };
    StateLabel { word, tone }
}

/// The engine state as shown; `installing` is whether the install guide runs.
fn shown(state: MachineState, installing: bool) -> ShownState {
    match state {
        MachineState::NotRunning => ShownState::Stopped,
        MachineState::Starting | MachineState::Running if installing => ShownState::Installing,
        MachineState::Starting => ShownState::Starting,
        MachineState::Running => ShownState::Running,
        MachineState::Paused => ShownState::Paused,
        MachineState::Stopping => ShownState::Saving,
        MachineState::Exited => ShownState::Exited,
        MachineState::LeftRunning => ShownState::LeftRunning,
    }
}

impl ShownState {
    pub const ALL: [ShownState; 11] = [
        ShownState::Stopped,
        ShownState::Starting,
        ShownState::Running,
        ShownState::Paused,
        ShownState::Saving,
        ShownState::LeftRunning,
        ShownState::Installing,
        ShownState::NoPicture,
        ShownState::Exited,
        ShownState::NeedsUpgrade,
        ShownState::CantOpen,
    ];
}

impl MachineSummary {
    pub fn shown(&self, installing: bool) -> ShownState {
        if self.needs_upgrade {
            ShownState::NeedsUpgrade
        } else if self.problem.is_some() {
            ShownState::CantOpen
        } else {
            shown(self.state, installing)
        }
    }

    /// What a person calls the model: its topology, else its product.
    fn model(&self) -> &str {
        self.topology_name.as_deref().unwrap_or(&self.product_label)
    }
}

impl MachineView {
    /// A machine with a screen whose picture has not come is `NoPicture`; one
    /// that ended on its own accord is `Stopped`, not `Exited`.
    pub fn shown(&self, installing: bool) -> ShownState {
        let blank = self.has_display && !self.heads.iter().any(|h| h.active);
        match self.state {
            MachineState::Running if blank && !installing => ShownState::NoPicture,
            MachineState::Exited
                if matches!(
                    self.exit_reason,
                    Some(ExitReason::GuestPoweredOff | ExitReason::HostStopped)
                ) =>
            {
                ShownState::Stopped
            }
            state => shown(state, installing),
        }
    }
}

/// The second line of a library row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryLine {
    pub text: String,
    pub tone: Tone,
}

/// The model for a stopped machine, otherwise the state word.
pub fn library_line_for(state: ShownState, model: &str) -> LibraryLine {
    if state == ShownState::Stopped {
        LibraryLine {
            text: model.into(),
            tone: Tone::None,
        }
    } else {
        let label = state_label(state);
        LibraryLine {
            text: label.word.into(),
            tone: label.tone,
        }
    }
}

/// [`library_line_for`] a summary's own state.
pub fn library_line(summary: &MachineSummary, installing: bool) -> LibraryLine {
    library_line_for(summary.shown(installing), summary.model())
}

/// A row's tooltip and accessible label: name, model, state and the OS when known.
pub fn row_description(summary: &MachineSummary, installing: bool) -> String {
    let state = state_label(summary.shown(installing)).word;
    [
        Some(summary.name.as_str()),
        Some(summary.model()),
        Some(state),
        summary.system_disk.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(", ")
}

/// Everything a library row shows, from one call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub state: ShownState,
    pub label: StateLabel,
    pub line: LibraryLine,
    pub description: String,
}

pub fn row(summary: &MachineSummary, installing: bool) -> Row {
    let state = summary.shown(installing);
    Row {
        state,
        label: state_label(state),
        line: library_line(summary, installing),
        description: row_description(summary, installing),
    }
}

/// The file manager's name, for "Show in …".
pub fn file_manager(platform: Platform) -> &'static str {
    match platform {
        Platform::Macos => "Finder",
        Platform::Windows => "Explorer",
        Platform::Linux => "Files",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    OpenConsole,
    PowerOff,
    Reconnect,
    Resume,
    Upgrade,
    ShowInFolder,
    TryAgain,
    OpenSettings,
    ChooseFirmware,
    ChooseDisk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardAction {
    pub kind: ActionKind,
    pub label: String,
}

/// What the machine page shows in place of the picture. Empty `title` and
/// `body` are for a state whose card is only its buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub title: String,
    pub body: String,
    pub actions: Vec<CardAction>,
    /// The error's detail, for Show details and Copy.
    pub details: Option<String>,
}

/// The card for `state`, or the "couldn't start" card when `error` is the
/// reason the machine is not running. None where the state has no card.
pub fn machine_card(
    state: ShownState,
    error: Option<&CoreError>,
    name: &str,
    platform: Platform,
) -> Option<Card> {
    use ActionKind::*;
    let act = |kind, label: &str| CardAction {
        kind,
        label: label.into(),
    };
    let card = |title: String, body: &str, actions| Card {
        title,
        body: body.into(),
        actions,
        details: None,
    };
    if let (Some(error), ShownState::Stopped | ShownState::Exited) = (error, state) {
        let fix = match error.fix {
            ErrorFix::None => None,
            ErrorFix::OpenSettings => Some(act(OpenSettings, "Open settings")),
            ErrorFix::ChooseFirmware => Some(act(ChooseFirmware, "Choose firmware files…")),
            ErrorFix::ChooseDisk => Some(act(ChooseDisk, "Choose a disk…")),
        };
        return Some(Card {
            details: error.detail.clone(),
            ..card(
                format!("{name} couldn't start"),
                &error.message,
                fix.into_iter()
                    .chain([act(TryAgain, "Try again")])
                    .collect(),
            )
        });
    }
    Some(match state {
        ShownState::NoPicture => card(
            "No picture yet".into(),
            "The machine is running, but nothing has appeared on its screen.",
            vec![
                act(OpenConsole, UiString::OpenConsole.text()),
                act(PowerOff, UiString::PowerOff.text()),
            ],
        ),
        ShownState::Saving => card(
            "Saving the disk".into(),
            "Writing this machine's changes to its disk.",
            vec![],
        ),
        ShownState::LeftRunning => card(
            "Running without Origami".into(),
            &format!("{name} kept running after Origami quit."),
            vec![
                act(Reconnect, "Reconnect"),
                act(PowerOff, UiString::PowerOff.text()),
            ],
        ),
        ShownState::NeedsUpgrade => card(
            "Made by an older Origami".into(),
            "Upgrade this machine once to use it. Its disks, firmware settings, network and Ethernet address stay the same.",
            vec![
                act(Upgrade, UiString::Upgrade.text()),
                act(ShowInFolder, &format!("Show in {}", file_manager(platform))),
            ],
        ),
        ShownState::Paused => card(String::new(), "", vec![act(Resume, "Resume")]),
        // Proposed words, not yet approved: the engine's problem goes under details.
        ShownState::CantOpen => Card {
            details: error.map(|e| e.message.clone()),
            ..card(
                format!("{name} can't be opened"),
                "Origami couldn't read this machine's settings.",
                vec![act(ShowInFolder, &format!("Show in {}", file_manager(platform)))],
            )
        },
        _ => return None,
    })
}

/// The bar under a running machine's picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusBar {
    /// The guest's picture size, such as `1280 × 1024`; none without a picture.
    pub picture: Option<String>,
    /// `Follows mouse` or the release words; none when the pointer is neither.
    pub pointer: Option<String>,
}

/// None unless the machine runs. `follows` and `captured` are the app's pointer mode.
pub fn status_bar(
    view: &MachineView,
    follows: bool,
    captured: bool,
    platform: Platform,
) -> Option<StatusBar> {
    if !matches!(view.state, MachineState::Running | MachineState::Paused) {
        return None;
    }
    let release = match platform {
        Platform::Macos => "⌃⌥",
        _ => "Ctrl+Alt",
    };
    let pointer = match (follows, captured) {
        (true, _) => Some("Follows mouse".to_string()),
        (false, true) => Some(format!("Captured, press {release} to release")),
        (false, false) => None,
    };
    Some(StatusBar {
        picture: view
            .heads
            .iter()
            .find(|h| h.active)
            .map(|h| format!("{} × {}", h.width, h.height)),
        pointer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade::{ConsoleView, ErrorKind, HeadView};

    fn summary(state: MachineState) -> MachineSummary {
        MachineSummary {
            key: "/lib/fuel".into(),
            name: "fuel".into(),
            product: "fuel".into(),
            product_label: "Fuel".into(),
            topology: None,
            topology_name: None,
            cards: vec![],
            has_display: true,
            runs_irix: true,
            nodes: 1,
            memory_per_node_mib: 256,
            state,
            problem: None,
            needs_upgrade: false,
            system_disk: Some("IRIX 6.5.30".into()),
        }
    }

    fn view(state: MachineState, active: bool) -> MachineView {
        MachineView {
            key: "k".into(),
            name: "fuel".into(),
            product: "fuel".into(),
            engine: "qemu".into(),
            has_display: true,
            state,
            exit_code: None,
            exit_reason: None,
            exit_detail: None,
            last_error: None,
            start_error: None,
            reason: String::new(),
            heads: vec![HeadView {
                index: 0,
                width: 1280,
                height: 1024,
                seq: 1,
                active,
            }],
            console: ConsoleView {
                connected: true,
                down_reason: None,
                total: 0,
            },
            operation: None,
        }
    }

    #[test]
    fn every_state_has_a_word_and_a_tone() {
        let words: Vec<_> = ShownState::ALL
            .iter()
            .map(|s| state_label(*s).word)
            .collect();
        let mut unique = words.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ShownState::ALL.len(), "{words:?}");
        assert_eq!(state_label(ShownState::Stopped).tone, Tone::None);
        assert_eq!(state_label(ShownState::Running).tone, Tone::Success);
        assert_eq!(state_label(ShownState::Exited).word, "Stopped unexpectedly");
        assert_eq!(state_label(ShownState::CantOpen).tone, Tone::Critical);
    }

    #[test]
    fn a_stopped_row_says_its_model_and_any_other_its_state() {
        let mut s = summary(MachineState::NotRunning);
        assert_eq!(
            library_line(&s, false),
            LibraryLine {
                text: "Fuel".into(),
                tone: Tone::None
            }
        );
        s.topology_name = Some("Fuel, 1 CPU".into());
        assert_eq!(library_line(&s, false).text, "Fuel, 1 CPU");
        s.state = MachineState::Running;
        assert_eq!(library_line(&s, true).text, "Installing");
        assert_eq!(library_line(&s, false).tone, Tone::Success);
        s.problem = Some("bad".into());
        assert_eq!(library_line(&s, false).text, "Can't open");
        s.needs_upgrade = true;
        assert_eq!(library_line(&s, false).text, "Needs upgrade");
    }

    #[test]
    fn a_row_is_described_in_one_line() {
        let mut s = summary(MachineState::NotRunning);
        assert_eq!(
            row_description(&s, false),
            "fuel, Fuel, Stopped, IRIX 6.5.30"
        );
        s.system_disk = None;
        assert_eq!(row_description(&s, false), "fuel, Fuel, Stopped");
    }

    #[test]
    fn a_view_tells_a_blank_screen_and_a_clean_end() {
        assert_eq!(
            view(MachineState::Running, false).shown(false),
            ShownState::NoPicture
        );
        assert_eq!(
            view(MachineState::Running, true).shown(false),
            ShownState::Running
        );
        let mut ended = view(MachineState::Exited, false);
        assert_eq!(ended.shown(false), ShownState::Exited);
        ended.exit_reason = Some(ExitReason::GuestPoweredOff);
        assert_eq!(ended.shown(false), ShownState::Stopped);
    }

    #[test]
    fn a_failed_start_offers_its_fix_then_try_again() {
        let error = CoreError::new(ErrorKind::Invalid, "Fuel has no disk.")
            .with_detail("drive 1 missing")
            .with_fix(ErrorFix::ChooseDisk);
        let card =
            machine_card(ShownState::Stopped, Some(&error), "fuel", Platform::Linux).unwrap();
        assert_eq!(card.title, "fuel couldn't start");
        assert_eq!(card.body, "Fuel has no disk.");
        assert_eq!(card.details.as_deref(), Some("drive 1 missing"));
        let kinds: Vec<_> = card.actions.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, [ActionKind::ChooseDisk, ActionKind::TryAgain]);
    }

    #[test]
    fn the_other_cards_follow_the_words_list() {
        let card = |s| machine_card(s, None, "fuel", Platform::Windows);
        assert_eq!(card(ShownState::NoPicture).unwrap().actions.len(), 2);
        assert_eq!(card(ShownState::Saving).unwrap().title, "Saving the disk");
        assert_eq!(
            card(ShownState::LeftRunning).unwrap().body,
            "fuel kept running after Origami quit."
        );
        let upgrade = card(ShownState::NeedsUpgrade).unwrap();
        assert_eq!(upgrade.actions[1].label, "Show in Explorer");
        assert_eq!(card(ShownState::Paused).unwrap().actions[0].label, "Resume");
        assert_eq!(card(ShownState::Running), None);
    }

    #[test]
    fn the_status_bar_shows_only_while_running() {
        assert_eq!(
            status_bar(
                &view(MachineState::Stopping, true),
                true,
                false,
                Platform::Linux
            ),
            None
        );
        let bar = status_bar(
            &view(MachineState::Running, true),
            false,
            true,
            Platform::Macos,
        )
        .unwrap();
        assert_eq!(bar.picture.as_deref(), Some("1280 × 1024"));
        assert_eq!(
            bar.pointer.as_deref(),
            Some("Captured, press ⌃⌥ to release")
        );
        let bar = status_bar(
            &view(MachineState::Running, false),
            true,
            false,
            Platform::Linux,
        )
        .unwrap();
        assert_eq!(
            (bar.picture, bar.pointer.as_deref()),
            (None, Some("Follows mouse"))
        );
        let bar = status_bar(
            &view(MachineState::Running, true),
            false,
            true,
            Platform::Windows,
        )
        .unwrap();
        assert_eq!(
            bar.pointer.as_deref(),
            Some("Captured, press Ctrl+Alt to release")
        );
        // Neither following nor captured has no words in the approved list.
        let idle = status_bar(
            &view(MachineState::Running, true),
            false,
            false,
            Platform::Linux,
        );
        assert_eq!(idle.unwrap().pointer, None);
    }

    #[test]
    fn a_row_comes_whole_and_a_summary_that_cannot_open_has_its_own_card() {
        let mut s = summary(MachineState::NotRunning);
        s.problem = Some("format 1 is too old".into());
        let r = row(&s, false);
        assert_eq!(
            (r.state, r.label.word),
            (ShownState::CantOpen, "Can't open")
        );
        assert_eq!(r.line.text, "Can't open");
        assert_eq!(r.description, "fuel, Fuel, Can't open, IRIX 6.5.30");
        assert_eq!(
            library_line_for(ShownState::NoPicture, "Fuel").tone,
            Tone::Critical
        );
        assert_eq!(library_line_for(ShownState::Stopped, "Fuel").text, "Fuel");
        let problem = CoreError::new(ErrorKind::Invalid, "format 1 is too old");
        let card = machine_card(
            ShownState::CantOpen,
            Some(&problem),
            "fuel",
            Platform::Macos,
        )
        .unwrap();
        assert_eq!(card.title, "fuel can't be opened");
        assert_eq!(card.details.as_deref(), Some("format 1 is too old"));
        assert_eq!(card.actions[0].label, "Show in Finder");
    }
}
