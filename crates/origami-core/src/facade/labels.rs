//! Text every app shows the same way: names, summary cards, the pointer
//! status and the Diagnostics report.

use super::MachineView;

/// The engine as a person knows it; the id stays `qemu` or `iris` in files
/// and on the command line.
pub fn engine_name(id: &str) -> String {
    match id {
        "qemu" => "QEMU (Origami)".into(),
        "iris" => "Iris".into(),
        other => other.into(),
    }
}

/// Memory as `256 MB`, or `2 GB` where it is a whole number of them.
pub fn memory_label(mib: u32) -> String {
    if mib >= 1024 && mib.is_multiple_of(1024) {
        format!("{} GB", mib / 1024)
    } else {
        format!("{mib} MB")
    }
}

/// One tile of a machine's summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryCard {
    pub title: String,
    pub value: String,
}

/// The tiles under a machine's name: its memory, its nodes where it has
/// several, which model it is, and what its system disk holds once read.
pub fn summary_cards(
    model: &str,
    nodes: u32,
    memory_per_node_mib: u32,
    system: Option<&str>,
) -> Vec<SummaryCard> {
    let card = |title: &str, value: String| SummaryCard {
        title: title.into(),
        value,
    };
    let mut cards = vec![card(
        "Memory",
        memory_label(memory_per_node_mib * nodes.max(1)),
    )];
    if nodes > 1 {
        cards.push(card("Nodes", nodes.to_string()));
    }
    cards.push(card("Model", model.into()));
    cards.extend(system.map(|os| card("System", os.into())));
    cards
}

/// The pointer mode's status line, such as `Follows mouse`.
pub fn pointer_status(follows: bool, captured: bool) -> &'static str {
    match (follows, captured) {
        (true, _) => "Follows mouse",
        (false, true) => "Captured",
        (false, false) => "Click to capture",
    }
}

/// The Diagnostics report: one `Label: value` line per fact, for the window
/// and its Copy report button. `status` is the word on the status pill, and
/// `updates` and `shown` are the app's counts of frame updates it received
/// and pictures it drew.
pub fn diagnostics_report(
    v: &MachineView,
    model: &str,
    status: &str,
    updates: u64,
    shown: u64,
) -> String {
    let mut rows: Vec<(String, String)> = vec![
        ("Machine".into(), v.name.clone()),
        ("Model".into(), model.into()),
        ("Engine".into(), engine_name(&v.engine)),
        ("Status".into(), status.into()),
        ("Details".into(), v.reason.clone()),
        (
            "Last error".into(),
            v.last_error.clone().unwrap_or_else(|| "none".into()),
        ),
    ];
    if v.heads.is_empty() {
        rows.push(("Display".into(), "none".into()));
    }
    for h in &v.heads {
        rows.push((
            format!("Display {}", h.index),
            format!(
                "{} x {}, picture {}, frame {}",
                h.width,
                h.height,
                if h.active { "yes" } else { "no" },
                h.seq
            ),
        ));
    }
    let c = &v.console;
    let link = match (c.connected, c.down_reason.as_deref()) {
        (true, _) => "connected".to_string(),
        (false, Some(why)) => format!("not connected ({why})"),
        (false, None) => "not connected".to_string(),
    };
    rows.push(("Console".into(), format!("{link}, {} bytes", c.total)));
    rows.push((
        "Frame updates".into(),
        format!("{updates} received, {shown} shown"),
    ));
    let width = rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0) + 3;
    rows.iter()
        .map(|(label, value)| format!("{:<width$}{value}\n", format!("{label}:")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade::{ConsoleView, HeadView, MachineState};

    fn view() -> MachineView {
        MachineView {
            key: "k".into(),
            name: "octane2".into(),
            product: "octane2".into(),
            engine: "qemu".into(),
            has_display: true,
            state: MachineState::Running,
            exit_code: None,
            exit_reason: None,
            exit_detail: None,
            last_error: None,
            start_error: None,
            reason: "The machine is running.".into(),
            heads: vec![HeadView {
                index: 0,
                width: 1280,
                height: 1024,
                seq: 47,
                active: true,
            }],
            console: ConsoleView {
                connected: true,
                down_reason: None,
                total: 143,
            },
            operation: None,
        }
    }

    #[test]
    fn the_report_is_one_set_of_plain_lines() {
        let report = diagnostics_report(&view(), "Octane2", "Running", 48, 47);
        assert_eq!(
            report,
            "Machine:        octane2\n\
             Model:          Octane2\n\
             Engine:         QEMU (Origami)\n\
             Status:         Running\n\
             Details:        The machine is running.\n\
             Last error:     none\n\
             Display 0:      1280 x 1024, picture yes, frame 47\n\
             Console:        connected, 143 bytes\n\
             Frame updates:  48 received, 47 shown\n"
        );
    }

    #[test]
    fn a_machine_without_a_display_or_console_says_so() {
        let mut v = view();
        v.heads.clear();
        v.console.connected = false;
        v.console.down_reason = Some("no serial port".into());
        let report = diagnostics_report(&v, "Octane2", "Stopped", 0, 0);
        assert!(report.contains("Display:        none\n"), "{report}");
        assert!(
            report.contains("not connected (no serial port), 143 bytes"),
            "{report}"
        );
    }

    #[test]
    fn nodes_show_only_where_there_are_several() {
        let titles = |nodes| {
            summary_cards("Octane2", nodes, 256, None)
                .into_iter()
                .map(|c| c.title)
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(1), ["Memory", "Model"]);
        assert_eq!(titles(2), ["Memory", "Nodes", "Model"]);
        assert_eq!(summary_cards("O", 2, 512, None)[0].value, "1 GB");
    }

    #[test]
    fn no_card_or_name_shows_an_id() {
        use origami_machine::listing::{looks_like_id, machine_name};
        let machines: origami_machine::Machines =
            serde_json::from_str(include_str!("../../../../tests/fixtures/sgi-machines.json"))
                .unwrap();
        for o in &machines.offerings {
            let model = machine_name(&o.topology);
            let cards = summary_cards(model, o.nodes, 256, None);
            for card in cards
                .iter()
                .map(|c| c.value.as_str())
                .chain([origami_machine::product_label(&o.product, Some(&o.topology)).as_str()])
            {
                assert!(!looks_like_id(card), "{card}");
                assert!(
                    ![&o.id, &o.product, &o.topology].contains(&&card.to_string()),
                    "{card}"
                );
            }
            assert_eq!(
                cards.iter().any(|c| c.title == "Nodes"),
                o.nodes > 1,
                "{}",
                o.id
            );
        }
        for id in ["qemu", "iris"] {
            assert!(![id].contains(&engine_name(id).as_str()) && !looks_like_id(&engine_name(id)));
        }
    }

    #[test]
    fn the_pointer_status_has_three_words() {
        assert_eq!(pointer_status(true, false), "Follows mouse");
        assert_eq!(pointer_status(false, true), "Captured");
        assert_eq!(pointer_status(false, false), "Click to capture");
    }

    #[test]
    fn a_reason_that_repeats_the_pill_is_no_note() {
        let mut v = view();
        assert_eq!(v.note(), None);
        v.reason = "The machine is running, with no picture: no head.".into();
        assert!(v.note().is_some());
    }

    /// Words that keep their capital in the middle of a sentence.
    const PROPER: &[&str] = &[
        "IRIX", "CD", "CPU", "CPUs", "QEMU", "Origami", "PROM", "NFS", "ID", "MIPSpro", "RAD4",
        "GB", "MB", "I", "SGI", "OK", "Iris", "Trash", "Recycle", "Bin", "Finder", "Explorer",
        "Files", "Ethernet", "Settings", "Standard", "Fuel",
    ];
    /// Words that mean nothing to a person using the app.
    const JARGON: &[&str] = &["qmp", "emu-id", "runner", "wake-ups", "presents"];

    /// What is wrong with `text` as a label or sentence, if anything: a
    /// separator dot, a lowercase start, Title Case or jargon.
    pub(crate) fn problem(text: &str) -> Option<String> {
        if text.contains('·') {
            return Some("has a separator dot".into());
        }
        let lower = text.to_lowercase();
        if let Some(word) = JARGON.iter().find(|w| lower.contains(*w)) {
            return Some(format!("says {word}"));
        }
        let mut starts = true;
        for word in text.split_whitespace() {
            let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
            let bare = bare.strip_suffix("'s").unwrap_or(bare);
            let first = bare.chars().next();
            let capital = first.is_some_and(char::is_uppercase);
            if starts && first.is_some_and(char::is_alphabetic) && !capital {
                return Some(format!("starts with lowercase {bare:?}"));
            }
            if !starts && capital && !PROPER.contains(&bare) && bare.chars().any(char::is_lowercase)
            {
                return Some(format!("capitalises {bare:?}"));
            }
            starts = word.ends_with(['.', '?', '!', ':']);
        }
        None
    }

    #[test]
    fn the_rule_catches_what_it_is_for() {
        assert!(problem("Power Off").is_some());
        assert_eq!(engine_name("qemu"), "QEMU (Origami)");
        assert_eq!(engine_name("iris"), "Iris");
        assert_eq!(problem("QEMU (Origami)"), None);
        assert!(problem("power off").is_some());
        assert!(problem("Fuel · 1 CPU").is_some());
        assert!(problem("Wake-ups received").is_some());
        assert_eq!(problem("Install IRIX…"), None);
        assert_eq!(problem("IRIX is installed. Start the machine."), None);
    }

    /// The words the 0.6.2 build added, from every call that makes them.
    fn new_words() -> Vec<String> {
        use crate::facade::*;
        let mut all: Vec<String> = Vec::new();
        let platforms = [Platform::Macos, Platform::Windows, Platform::Linux];
        all.extend(ShownState::ALL.map(|s| state_label(s).word.to_string()));
        all.push(machine_id_caption(7));
        let error = CoreError::new(ErrorKind::Invalid, "Fuel has no disk.")
            .with_fix(ErrorFix::ChooseFirmware);
        for platform in platforms {
            for state in ShownState::ALL {
                for error in [None, Some(&error)] {
                    if let Some(c) = machine_card(state, error, "Fuel", platform) {
                        all.extend([c.title, c.body]);
                        all.extend(c.actions.into_iter().map(|a| a.label));
                    }
                }
            }
            let kinds = [
                AlertKind::QuitRunning {
                    names: vec!["Fuel".into()],
                },
                AlertKind::QuitRunning {
                    names: vec!["a".into(), "b".into()],
                },
                AlertKind::PowerOff {
                    name: "Fuel".into(),
                },
                AlertKind::Reset {
                    name: "Fuel".into(),
                },
                AlertKind::ForceQuit {
                    name: "Fuel".into(),
                },
                AlertKind::CloseTab {
                    name: "Fuel".into(),
                },
                AlertKind::PowerOffFailed {
                    names: vec!["Fuel".into()],
                },
                AlertKind::CloseWindow {
                    names: vec!["Fuel".into(), "Octane".into()],
                },
                AlertKind::Remove {
                    name: "Fuel".into(),
                    disk_left: vec![],
                },
            ];
            for kind in kinds {
                let a = alert(&kind, platform);
                all.extend([a.title, a.body]);
                all.extend(a.buttons.into_iter().map(|b| b.label));
            }
            for kind in [
                ToastKind::Upgraded {
                    name: "Fuel".into(),
                },
                ToastKind::Removed {
                    name: "Fuel".into(),
                },
                ToastKind::ScreenshotSaved,
                ToastKind::DiscEjected,
                ToastKind::PictureSize { name: "Fit".into() },
            ] {
                let t = toast(&kind, platform);
                all.extend(std::iter::once(t.text).chain(t.action));
            }
            for g in shortcut_groups(platform) {
                all.push(g.title);
                all.extend(g.items.into_iter().map(|i| i.label));
            }
            for c in commands_for(platform) {
                all.extend([c.title, c.help]);
            }
        }
        for kind in [
            EmptyKind::NoMachines,
            EmptyKind::NoneSelected,
            EmptyKind::NoSharedFolders,
            EmptyKind::NoDownloadedDisks,
        ] {
            let e = empty_state(kind);
            all.extend([e.title, e.sentence].map(String::from));
            all.extend(e.button.map(String::from));
        }
        for o in disk_options(true) {
            all.extend([o.title, o.caption]);
        }
        all.extend([ready_caption(835_000_000, false), ready_caption(0, true)]);
        all.extend([
            guide_header(3, 22),
            waiting_for("login:"),
            install_footer("IRIX 6.5.30, Standard"),
        ]);
        all.extend(
            host_network_modes()
                .into_iter()
                .flat_map(|c| [c.label, c.detail]),
        );
        all.extend(
            [
                startup_value(BootConsole::Screen),
                startup_value(BootConsole::Serial),
            ]
            .map(String::from),
        );
        all.extend(
            step_titles(&InstallFlow::new(install_releases(), None, 4))
                .into_iter()
                .map(|t| t.title.to_string()),
        );
        all.extend(
            [
                UiString::Custom,
                UiString::Disk,
                UiString::DiskSize,
                UiString::Done,
                UiString::DownloadThem,
                UiString::FromTheMachineId,
                UiString::KeyboardShortcuts,
                UiString::Machine,
                UiString::MachineId,
                UiString::More,
                UiString::NeedsAnInternetConnection,
                UiString::NewMachineFromThisDisk,
                UiString::PowerOffToChangeHardwareSettings,
                UiString::Recommended,
                UiString::Screen,
                UiString::Seen,
                UiString::ShowDetails,
                UiString::ChangeCd,
                UiString::CaptureMouse,
                UiString::ShowOrHideTheLibrary,
                UiString::StartupMessagesOn,
                UiString::TypeIt,
                UiString::TypeLabel,
                UiString::UseDiscsIAlreadyHave,
                UiString::WatchFor,
                UiString::WhereShouldTheDiscsComeFrom,
            ]
            .map(|s| s.text().to_string()),
        );
        all
    }

    #[test]
    fn every_string_the_core_owns_follows_the_rules() {
        use crate::facade::{disk_labels, flow, texts};
        let mut all: Vec<String> = crate::facade::commands::table()
            .into_iter()
            .map(|c| c.title)
            .collect();
        all.extend(flow::ALL.iter().map(|s| s.title().to_string()));
        all.extend(
            [
                texts::CHECKING,
                texts::PASSED,
                texts::FINISHED,
                texts::QUIT_HEADING,
                texts::QUIT_TEXT,
                texts::TAB_TEXT,
                texts::KEEP,
                texts::STOP_AND_CLOSE,
                texts::STOP_AND_QUIT,
                texts::CHECK_INTRO,
                texts::MEDIA_DOWNLOAD,
                texts::MEDIA_FOLDER,
            ]
            .map(String::from),
        );
        let disks = disk_labels();
        all.extend(
            disks
                .choices
                .iter()
                .chain(&disks.place)
                .chain(&disks.keep)
                .cloned(),
        );
        all.extend([disks.same_kind, disks.ready, disks.release]);
        all.extend(
            [(true, false), (false, true), (false, false)]
                .map(|(f, c)| pointer_status(f, c).to_string()),
        );
        all.extend(
            summary_cards("Octane2", 2, 512, Some("IRIX 6.5.30"))
                .into_iter()
                .map(|c| c.title),
        );
        all.extend(new_words());
        let report = diagnostics_report(&view(), "Octane2", "Running", 1, 1);
        all.extend(
            report
                .lines()
                .filter_map(|l| l.split_once(':').map(|(label, _)| label.to_string())),
        );
        let mut bad: Vec<_> = all
            .iter()
            .filter_map(|t| problem(t).map(|p| format!("{t:?} {p}")))
            .collect();
        bad.sort();
        bad.dedup();
        assert!(bad.is_empty(), "{bad:#?}");
    }
}
