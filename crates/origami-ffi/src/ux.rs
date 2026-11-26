//! The 0.6.2 words and cards over the facade: thin mirrors of its types and
//! calls. The core owns every word.

use crate::control::*;
use crate::flow::OrigamiInstallFlow;
use origami_core::facade as f;

mirror!(OrigamiTone, f::Tone, None, Success, Caution, Critical);
mirror!(
    OrigamiShownState,
    f::ShownState,
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
    CantOpen
);
mirror!(
    OrigamiActionKind,
    f::ActionKind,
    OpenConsole,
    PowerOff,
    Reconnect,
    Resume,
    Upgrade,
    ShowInFolder,
    TryAgain,
    OpenSettings,
    ChooseFirmware,
    ChooseDisk
);
mirror!(OrigamiButtonRole, f::ButtonRole, Safe, Destructive, Cancel);
mirror!(
    OrigamiEmptyKind,
    f::EmptyKind,
    NoMachines,
    NoneSelected,
    NoSharedFolders,
    NoDownloadedDisks
);
mirror!(
    OrigamiDiskChoice,
    f::DiskChoice,
    Ready,
    Install,
    Image,
    NoDisk
);
mirror!(OrigamiDropKind, f::DropKind, CdImage, DiskImage, Other);

impl From<OrigamiPlatform> for f::Platform {
    fn from(p: OrigamiPlatform) -> Self {
        match p {
            OrigamiPlatform::Macos => Self::Macos,
            OrigamiPlatform::Windows => Self::Windows,
            OrigamiPlatform::Linux => Self::Linux,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiStateLabel {
    pub word: String,
    pub tone: OrigamiTone,
}

#[uniffi::export]
pub fn origami_state_label(state: OrigamiShownState) -> OrigamiStateLabel {
    let l = f::state_label(state.into());
    OrigamiStateLabel {
        word: l.word.into(),
        tone: l.tone.into(),
    }
}

fn summary_in(m: OrigamiMachineSummary) -> f::MachineSummary {
    f::MachineSummary {
        key: m.key,
        name: m.name,
        product: m.product,
        product_label: m.product_label,
        topology: m.topology,
        topology_name: m.topology_name,
        cards: vec![],
        has_display: m.has_display,
        runs_irix: m.runs_irix,
        nodes: m.nodes,
        memory_per_node_mib: m.memory_per_node_mib,
        state: m.state.into(),
        problem: m.problem,
        needs_upgrade: m.needs_upgrade,
        system_disk: m.system_disk,
    }
}

/// Everything a library row shows: its state, the word and tone for it, the
/// second line and the tooltip. `installing` is whether its install guide runs.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiRow {
    pub state: OrigamiShownState,
    pub word: String,
    pub tone: OrigamiTone,
    pub line: OrigamiLibraryLine,
    pub description: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiLibraryLine {
    pub text: String,
    pub tone: OrigamiTone,
}

impl From<f::LibraryLine> for OrigamiLibraryLine {
    fn from(l: f::LibraryLine) -> Self {
        Self {
            text: l.text,
            tone: l.tone.into(),
        }
    }
}

#[uniffi::export]
pub fn origami_row(summary: OrigamiMachineSummary, installing: bool) -> OrigamiRow {
    let r = f::row(&summary_in(summary), installing);
    OrigamiRow {
        state: r.state.into(),
        word: r.label.word.into(),
        tone: r.label.tone.into(),
        line: r.line.into(),
        description: r.description,
    }
}

/// The second line for a row in `state`, such as No picture for a machine the
/// app has opened, which the summary alone cannot tell.
#[uniffi::export]
pub fn origami_library_line_for(state: OrigamiShownState, model: String) -> OrigamiLibraryLine {
    f::library_line_for(state.into(), &model).into()
}

/// What the Machine ID field says under it.
#[uniffi::export]
pub fn origami_machine_id_caption(next: u32) -> String {
    f::machine_id_caption(next)
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCardAction {
    pub kind: OrigamiActionKind,
    pub label: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCard {
    pub title: String,
    pub body: String,
    pub actions: Vec<OrigamiCardAction>,
    /// The error's detail, for Show details and Copy.
    pub details: Option<String>,
}

/// The card for a state, or for the error that kept the machine from starting.
#[uniffi::export]
pub fn origami_machine_card(
    state: OrigamiShownState,
    error: Option<OrigamiErrorInfo>,
    name: String,
    platform: OrigamiPlatform,
) -> Option<OrigamiCard> {
    let error = error.map(|e| {
        let mut error = f::CoreError::new(e.kind.into(), e.message).with_fix(e.fix.into());
        error.detail = e.detail;
        error
    });
    f::machine_card(state.into(), error.as_ref(), &name, platform.into()).map(|c| OrigamiCard {
        title: c.title,
        body: c.body,
        actions: c
            .actions
            .into_iter()
            .map(|a| OrigamiCardAction {
                kind: a.kind.into(),
                label: a.label,
            })
            .collect(),
        details: c.details,
    })
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiStatusBar {
    pub picture: Option<String>,
    pub pointer: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiAlertButton {
    pub label: String,
    pub role: OrigamiButtonRole,
}

/// Buttons are in the order to show them.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiAlert {
    pub title: String,
    pub body: String,
    pub buttons: Vec<OrigamiAlertButton>,
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum OrigamiAlertKind {
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
    Remove {
        name: String,
        disk_left: Vec<String>,
    },
}

#[uniffi::export]
pub fn origami_alert(kind: OrigamiAlertKind, platform: OrigamiPlatform) -> OrigamiAlert {
    use OrigamiAlertKind as K;
    let kind = match kind {
        K::QuitRunning { names } => f::AlertKind::QuitRunning { names },
        K::PowerOff { name } => f::AlertKind::PowerOff { name },
        K::Reset { name } => f::AlertKind::Reset { name },
        K::ForceQuit { name } => f::AlertKind::ForceQuit { name },
        K::CloseTab { name } => f::AlertKind::CloseTab { name },
        K::Remove { name, disk_left } => f::AlertKind::Remove { name, disk_left },
    };
    let a = f::alert(&kind, platform.into());
    OrigamiAlert {
        title: a.title,
        body: a.body,
        buttons: a
            .buttons
            .into_iter()
            .map(|b| OrigamiAlertButton {
                label: b.label,
                role: b.role.into(),
            })
            .collect(),
    }
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum OrigamiToastKind {
    Upgraded { name: String },
    Removed { name: String },
    ScreenshotSaved,
    DiscEjected,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiToast {
    pub text: String,
    pub action: Option<String>,
}

#[uniffi::export]
pub fn origami_toast(kind: OrigamiToastKind, platform: OrigamiPlatform) -> OrigamiToast {
    let kind = match kind {
        OrigamiToastKind::Upgraded { name } => f::ToastKind::Upgraded { name },
        OrigamiToastKind::Removed { name } => f::ToastKind::Removed { name },
        OrigamiToastKind::ScreenshotSaved => f::ToastKind::ScreenshotSaved,
        OrigamiToastKind::DiscEjected => f::ToastKind::DiscEjected,
    };
    let t = f::toast(&kind, platform.into());
    OrigamiToast {
        text: t.text,
        action: t.action,
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiEmptyState {
    pub title: String,
    pub sentence: String,
    pub button: Option<String>,
}

#[uniffi::export]
pub fn origami_empty_state(kind: OrigamiEmptyKind) -> OrigamiEmptyState {
    let e = f::empty_state(kind.into());
    OrigamiEmptyState {
        title: e.title.into(),
        sentence: e.sentence.into(),
        button: e.button.map(Into::into),
    }
}

/// `Finder`, `Explorer` or `Files`.
#[uniffi::export]
pub fn origami_file_manager(platform: OrigamiPlatform) -> String {
    f::file_manager(platform.into()).into()
}

/// `Trash` or `Recycle Bin`.
#[uniffi::export]
pub fn origami_trash_name(platform: OrigamiPlatform) -> String {
    f::trash_name(platform.into()).into()
}

/// A command's tooltip, ending with its shortcut on `platform`; none for an unknown id.
#[uniffi::export]
pub fn origami_command_tooltip(id: String, platform: OrigamiPlatform) -> Option<String> {
    let platform = platform.into();
    f::commands_for(platform)
        .into_iter()
        .find(|c| c.id == id)
        .map(|c| c.tooltip(platform))
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiShortcutItem {
    pub label: String,
    pub keys: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiShortcutGroup {
    pub title: String,
    pub items: Vec<OrigamiShortcutItem>,
}

/// The keyboard shortcuts window: the release keys first.
#[uniffi::export]
pub fn origami_shortcut_groups(platform: OrigamiPlatform) -> Vec<OrigamiShortcutGroup> {
    f::shortcut_groups(platform.into())
        .into_iter()
        .map(|g| OrigamiShortcutGroup {
            title: g.title,
            items: g
                .items
                .into_iter()
                .map(|i| OrigamiShortcutItem {
                    label: i.label,
                    keys: i.keys,
                })
                .collect(),
        })
        .collect()
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiDiskOption {
    pub choice: OrigamiDiskChoice,
    pub title: String,
    pub caption: String,
    /// Tagged "Recommended".
    pub recommended: bool,
}

/// The New machine disk choices in listing order; the first is the default.
#[uniffi::export]
pub fn origami_disk_options(ready_available: bool) -> Vec<OrigamiDiskOption> {
    f::disk_options(ready_available)
        .into_iter()
        .map(|o| OrigamiDiskOption {
            choice: o.choice.into(),
            title: o.title,
            caption: o.caption,
            recommended: o.recommended,
        })
        .collect()
}

/// Under a ready-made disk: its download size, or `Downloaded`.
#[uniffi::export]
pub fn origami_ready_caption(bytes: u64, cached: bool) -> String {
    f::ready_caption(bytes, cached)
}

#[uniffi::export]
pub fn origami_classify_drop(path: String) -> OrigamiDropKind {
    f::classify_drop(std::path::Path::new(&path)).into()
}

/// What dropping that kind of file does, as a caption; none for `Other`.
#[uniffi::export]
pub fn origami_drop_caption(kind: OrigamiDropKind) -> Option<String> {
    f::drop_caption(kind.into()).map(Into::into)
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiDriveRow {
    /// The drive's name in the machine file, for `DetachDrive`.
    pub name: String,
    pub title: String,
    pub caption: String,
}

/// `Screen` or `Console`, after "Startup messages on".
#[uniffi::export]
pub fn origami_startup_value(console: crate::control::OrigamiBootConsole) -> String {
    f::startup_value(match console {
        OrigamiBootConsole::Serial => f::BootConsole::Serial,
        OrigamiBootConsole::Screen => f::BootConsole::Screen,
    })
    .into()
}

/// The footer while the install files are served, from `OrigamiInstallPlan::summary`.
#[uniffi::export]
pub fn origami_install_footer(plan: OrigamiInstallPlan) -> String {
    f::install_footer(&plan.summary)
}

/// The line under a guide step that waits for `prompt`.
#[uniffi::export]
pub fn origami_waiting_for(prompt: String) -> String {
    f::waiting_for(&prompt)
}

/// The guide's header, such as `Install IRIX, step 3 of 22`.
#[uniffi::export]
pub fn origami_guide_header(step: u32, total: u32) -> String {
    f::guide_header(step as usize, total as usize)
}

mirror!(OrigamiStepState, f::StepState, Done, Current, Later);

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiStepTitle {
    pub step: crate::OrigamiInstallStep,
    pub title: String,
    /// From 1, counting only the steps this machine has.
    pub number: u32,
    pub state: OrigamiStepState,
}

#[uniffi::export]
impl OrigamiInstallFlow {
    /// The numbered step list, with the current page marked.
    pub fn step_titles(&self) -> Vec<OrigamiStepTitle> {
        f::step_titles(&self.0.lock().unwrap())
            .into_iter()
            .map(|t| OrigamiStepTitle {
                step: t.step.into(),
                title: t.title.into(),
                number: t.number,
                state: t.state.into(),
            })
            .collect()
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiRemoved {
    /// The library entry that was moved: what a toast names.
    pub name: String,
    /// Disk images the machine used in place, outside its folder.
    pub disk_left: Vec<String>,
}

#[uniffi::export]
impl OrigamiCore {
    /// The OS and release on a disk image, or none when it is not recognised.
    /// Off the UI thread.
    pub fn disk_os(&self, path: String) -> Result<Option<String>, CoreError> {
        Ok(self.core.disk_os(std::path::Path::new(&path))?)
    }

    /// The size the guest sees for a disk image: the virtual size of a qcow2 or CHD.
    pub fn disk_size(&self, path: String) -> Result<u64, CoreError> {
        Ok(self.core.disk_size(std::path::Path::new(&path))?)
    }

    /// Upgrade with firmware files the person chose, instead of a download.
    /// May take a while: off the UI thread.
    pub fn upgrade_machine_with(
        &self,
        key: String,
        prom: Option<String>,
        io_prom: Option<String>,
    ) -> Result<Vec<OrigamiReportLine>, CoreError> {
        let report = self.core.upgrade_machine_with(
            &key,
            prom.as_deref().map(std::path::Path::new),
            io_prom.as_deref().map(std::path::Path::new),
        )?;
        Ok(crate::control::report_lines(report))
    }

    /// Move a stopped machine's folder to the trash. `Busy` while it runs.
    pub fn remove_machine(&self, key: String) -> Result<OrigamiRemoved, CoreError> {
        let r = self.core.remove_machine(&key)?;
        Ok(OrigamiRemoved {
            name: r.name,
            disk_left: r.disk_left,
        })
    }
}

#[uniffi::export]
impl OrigamiMachine {
    /// What the machine shows as its state; `installing` is whether its install guide runs.
    pub fn shown_state(&self, installing: bool) -> OrigamiShownState {
        self.0.view().shown(installing).into()
    }

    /// The bar under the picture, or none unless the machine runs.
    pub fn status_bar(
        &self,
        follows: bool,
        captured: bool,
        platform: OrigamiPlatform,
    ) -> Option<OrigamiStatusBar> {
        f::status_bar(&self.0.view(), follows, captured, platform.into()).map(|b| {
            OrigamiStatusBar {
                picture: b.picture,
                pointer: b.pointer,
            }
        })
    }

    /// How many leading steps of the install guide the console shows done.
    /// Ask again on each console event while the guide shows.
    pub fn guide_seen(&self, steps: Vec<OrigamiGuideStep>) -> u32 {
        let steps: Vec<_> = steps
            .into_iter()
            .map(|s| f::GuideStep {
                text: s.text,
                command: s.command,
                send: s.send,
                title: s.title,
                watch: s.watch,
                typed: vec![],
                note: None,
                confirm: s.confirm,
            })
            .collect();
        self.0.guide_seen(&steps) as u32
    }

    /// The OS on the disk in `drive`, or none when it is not recognised.
    pub fn drive_os(&self, drive: String) -> Result<Option<String>, CoreError> {
        Ok(self.0.drive_os(&drive)?)
    }

    /// The Drives list: a title and caption for each drive.
    pub fn drive_rows(&self) -> Result<Vec<OrigamiDriveRow>, CoreError> {
        Ok(self
            .0
            .drive_rows()?
            .into_iter()
            .map(|r| OrigamiDriveRow {
                name: r.name,
                title: r.title,
                caption: r.caption,
            })
            .collect())
    }
}

#[uniffi::export]
impl OrigamiOperation {
    /// Why a finished operation failed, with what a person can do about it.
    pub fn failure(&self) -> Option<OrigamiErrorInfo> {
        match self.0.state() {
            f::OperationState::Failed(e) => Some(e.into()),
            _ => None,
        }
    }
}
