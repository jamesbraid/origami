//! The install assistant's step rules (`origami_core::facade::InstallFlow`)
//! for the apps, which keep only widgets.

use crate::control::*;
use origami_core::facade as f;
use std::path::PathBuf;
use std::sync::Mutex;

mirror!(
    OrigamiInstallStep,
    f::InstallStep,
    Release,
    Media,
    Software,
    Method,
    Disk,
    Check,
    Prepare,
    Installer,
    Packages,
    Finish
);
mirror!(OrigamiInstallMethod, f::InstallMethod, Automatic, Guided);
mirror!(
    OrigamiInstallFailure,
    f::InstallFailure,
    Setup,
    Conflict,
    Lost,
    Login,
    Shutdown,
    Cancelled
);
mirror!(
    OrigamiCheckState,
    f::CheckState,
    Ready,
    Running,
    Passed,
    Failed
);
mirror!(
    OrigamiCloseAction,
    f::CloseAction,
    Stay,
    StopMachine,
    LeaveRunning,
    SetBack,
    Leave
);

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallDisk {
    pub name: String,
    pub bus: u32,
    pub target: u32,
    pub text: String,
}

impl From<f::InstallDisk> for OrigamiInstallDisk {
    fn from(d: f::InstallDisk) -> Self {
        Self {
            name: d.name,
            bus: d.bus,
            target: d.target,
            text: d.text,
        }
    }
}

/// What the assistant knows of the machine's disks.
#[derive(Clone, Debug, uniffi::Enum)]
pub enum OrigamiDiskState {
    Unknown,
    None,
    Present { disk: OrigamiInstallDisk },
}

/// Where the automated install is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OrigamiApplyState {
    Idle,
    Running,
    Done,
    Failed { kind: OrigamiInstallFailure },
}

impl From<OrigamiApplyState> for f::ApplyState {
    fn from(s: OrigamiApplyState) -> Self {
        match s {
            OrigamiApplyState::Idle => Self::Idle,
            OrigamiApplyState::Running => Self::Running,
            OrigamiApplyState::Done => Self::Done,
            OrigamiApplyState::Failed { kind } => Self::Failed(kind.into()),
        }
    }
}

impl From<f::ApplyState> for OrigamiApplyState {
    fn from(s: f::ApplyState) -> Self {
        match s {
            f::ApplyState::Idle => Self::Idle,
            f::ApplyState::Running => Self::Running,
            f::ApplyState::Done => Self::Done,
            f::ApplyState::Failed(kind) => Self::Failed { kind: kind.into() },
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCloseButton {
    pub label: String,
    pub action: OrigamiCloseAction,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiCloseQuestion {
    pub heading: String,
    pub text: String,
    pub buttons: Vec<OrigamiCloseButton>,
    /// The button Return picks: the one that destroys nothing.
    pub default_button: u32,
}

fn question(q: Option<f::CloseQuestion>) -> Option<OrigamiCloseQuestion> {
    q.map(|q| OrigamiCloseQuestion {
        heading: q.heading,
        text: q.text,
        default_button: q.default as u32,
        buttons: q
            .buttons
            .into_iter()
            .map(|(label, action)| OrigamiCloseButton {
                label,
                action: action.into(),
            })
            .collect(),
    })
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiAddonTick {
    pub choice: OrigamiInstallAddonChoice,
    pub on: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OrigamiInstallTexts {
    pub checking: String,
    pub finished: String,
    pub quit_heading: String,
    pub quit_text: String,
    pub tab_text: String,
    pub keep: String,
    pub stop_and_close: String,
    pub stop_and_quit: String,
    pub check_intro: String,
    pub media_download: String,
    pub media_folder: String,
}

#[uniffi::export]
pub fn origami_install_texts() -> OrigamiInstallTexts {
    use f::texts as t;
    let s = String::from;
    OrigamiInstallTexts {
        checking: s(t::CHECKING),
        finished: s(t::FINISHED),
        quit_heading: s(t::QUIT_HEADING),
        quit_text: s(t::QUIT_TEXT),
        tab_text: s(t::TAB_TEXT),
        keep: s(t::KEEP),
        stop_and_close: s(t::STOP_AND_CLOSE),
        stop_and_quit: s(t::STOP_AND_QUIT),
        check_intro: s(t::CHECK_INTRO),
        media_download: s(t::MEDIA_DOWNLOAD),
        media_folder: s(t::MEDIA_FOLDER),
    }
}

#[uniffi::export]
pub fn origami_install_step_title(step: OrigamiInstallStep) -> String {
    f::InstallStep::from(step).title().into()
}

fn release(r: &f::InstallRelease) -> OrigamiInstallRelease {
    OrigamiInstallRelease {
        release: r.release.clone(),
        label: r.label.clone(),
        profiles: r
            .profiles
            .iter()
            .map(|p| OrigamiInstallProfile {
                name: p.name.clone(),
                label: p.label.clone(),
                detail: p.detail.clone(),
            })
            .collect(),
    }
}

fn choice_in(c: OrigamiInstallAddonChoice) -> f::InstallAddonChoice {
    f::InstallAddonChoice {
        name: c.name,
        label: c.label,
        note: c.note,
        checked: c.checked,
        locked: c.locked,
    }
}

fn choice_out(c: &f::InstallAddonChoice) -> OrigamiInstallAddonChoice {
    OrigamiInstallAddonChoice {
        name: c.name.clone(),
        label: c.label.clone(),
        note: c.note.clone(),
        checked: c.checked,
        locked: c.locked,
    }
}

fn plan_in(p: OrigamiInstallPlan) -> f::InstallPlan {
    f::InstallPlan {
        summary: p.summary,
        method: p.method.into(),
        release: p.release,
        profile: p.profile,
        local_media: p.local_media,
        addons: p.addons,
    }
}

/// The assistant's choices and the rules for moving between its steps.
#[derive(uniffi::Object)]
pub struct OrigamiInstallFlow(pub(crate) Mutex<f::InstallFlow>);

#[uniffi::export]
impl OrigamiInstallFlow {
    /// A new assistant, or one resumed on the machine's `plan`.
    #[uniffi::constructor]
    pub fn new(plan: Option<OrigamiInstallPlan>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self(Mutex::new(f::InstallFlow::new(
            f::install_releases(),
            plan.map(plan_in),
            f::disk_labels().default_gb,
        ))))
    }

    pub fn step(&self) -> OrigamiInstallStep {
        self.0.lock().unwrap().step().into()
    }
    pub fn steps(&self) -> Vec<OrigamiInstallStep> {
        self.0
            .lock()
            .unwrap()
            .steps()
            .into_iter()
            .map(Into::into)
            .collect()
    }
    /// Whether the PROM prints on the serial line: set once from `boot_console`.
    pub fn set_serial(&self, serial: bool) {
        self.0.lock().unwrap().set_serial(serial);
    }
    pub fn serial(&self) -> bool {
        self.0.lock().unwrap().serial()
    }
    pub fn method(&self) -> OrigamiInstallMethod {
        self.0.lock().unwrap().method().into()
    }
    pub fn set_method(&self, method: OrigamiInstallMethod) {
        self.0.lock().unwrap().set_method(method.into());
    }
    /// Origami types the package commands.
    pub fn automatic(&self) -> bool {
        self.0.lock().unwrap().automatic()
    }
    pub fn apply(&self) -> OrigamiApplyState {
        self.0.lock().unwrap().apply().into()
    }
    pub fn set_apply(&self, state: OrigamiApplyState) {
        self.0.lock().unwrap().set_apply(state.into());
    }
    pub fn can_install(&self, machine_up: bool) -> bool {
        self.0.lock().unwrap().can_install(machine_up)
    }
    /// "I fixed it at Inst>" is offered.
    pub fn can_fixed(&self) -> bool {
        self.0.lock().unwrap().can_fixed()
    }
    /// The person took over; show the guide's `restart` steps alone.
    pub fn fixed(&self) {
        self.0.lock().unwrap().fixed();
    }
    pub fn by_hand(&self) -> bool {
        self.0.lock().unwrap().by_hand()
    }
    pub fn apply_hint(&self) -> String {
        self.0.lock().unwrap().apply_hint()
    }
    pub fn resumed(&self) -> bool {
        self.0.lock().unwrap().resumed()
    }
    pub fn releases(&self) -> Vec<OrigamiInstallRelease> {
        self.0
            .lock()
            .unwrap()
            .releases()
            .iter()
            .map(release)
            .collect()
    }
    pub fn release(&self) -> String {
        self.0.lock().unwrap().release().into()
    }
    pub fn profile(&self) -> String {
        self.0.lock().unwrap().profile().into()
    }
    pub fn select_release(&self, release: String) {
        self.0.lock().unwrap().select_release(&release);
    }
    pub fn select_profile(&self, profile: String) {
        self.0.lock().unwrap().select_profile(&profile);
    }
    pub fn use_folder(&self) -> bool {
        self.0.lock().unwrap().use_folder()
    }
    pub fn set_use_folder(&self, on: bool) {
        self.0.lock().unwrap().set_use_folder(on);
    }
    pub fn folder(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap()
            .folder()
            .map(|p| p.to_string_lossy().into_owned())
    }
    /// The chosen folder and what `origami_install_media_folder` found in it.
    pub fn set_folder(&self, path: String, found: OrigamiMediaFolder) {
        let found = f::MediaFolder {
            found: found.found,
            total: found.total,
            missing: found.missing,
            verdict: found.verdict,
            complete: found.complete,
        };
        self.0
            .lock()
            .unwrap()
            .set_folder(PathBuf::from(path), &found);
    }
    pub fn set_choices(&self, choices: Vec<OrigamiInstallAddonChoice>) {
        self.0
            .lock()
            .unwrap()
            .set_choices(choices.into_iter().map(choice_in).collect());
    }
    pub fn addons(&self) -> Vec<OrigamiAddonTick> {
        self.0
            .lock()
            .unwrap()
            .addons()
            .iter()
            .map(|(c, on)| OrigamiAddonTick {
                choice: choice_out(c),
                on: *on,
            })
            .collect()
    }
    pub fn tick(&self, name: String, on: bool) {
        self.0.lock().unwrap().tick(&name, on);
    }
    /// False when `source` is empty. `selections` is one package per line.
    pub fn add_extra(&self, source: String, selections: String) -> bool {
        self.0.lock().unwrap().add_extra(&source, &selections)
    }
    pub fn remove_extra(&self, index: u32) {
        self.0.lock().unwrap().remove_extra(index as usize);
    }
    pub fn extras(&self) -> Vec<OrigamiInstallAddon> {
        self.0
            .lock()
            .unwrap()
            .extras()
            .iter()
            .map(|e| OrigamiInstallAddon {
                name: e.name.clone(),
                source: e.source.clone(),
                install: e.install.clone(),
            })
            .collect()
    }
    pub fn set_disk(&self, disk: OrigamiDiskState) {
        self.0.lock().unwrap().set_disk(match disk {
            OrigamiDiskState::Unknown => f::DiskState::Unknown,
            OrigamiDiskState::None => f::DiskState::None,
            OrigamiDiskState::Present { disk } => f::DiskState::Present(f::InstallDisk {
                name: disk.name,
                bus: disk.bus,
                target: disk.target,
                text: disk.text,
            }),
        });
    }
    pub fn new_disk_gb(&self) -> u32 {
        self.0.lock().unwrap().new_disk_gb()
    }
    pub fn set_new_disk_gb(&self, gb: u32) {
        self.0.lock().unwrap().set_new_disk_gb(gb);
    }
    pub fn erase(&self) -> bool {
        self.0.lock().unwrap().erase()
    }
    pub fn set_erase(&self, on: bool) {
        self.0.lock().unwrap().set_erase(on);
    }
    pub fn check(&self) -> OrigamiCheckState {
        self.0.lock().unwrap().check().into()
    }
    pub fn set_check(&self, state: OrigamiCheckState) {
        self.0.lock().unwrap().set_check(state.into());
    }
    /// The setup was written to the machine, so closing may offer to set
    /// the network back.
    pub fn set_prepared(&self) {
        self.0.lock().unwrap().set_prepared();
    }
    pub fn can_back(&self) -> bool {
        self.0.lock().unwrap().can_back()
    }
    pub fn can_continue(&self, machine_up: bool) -> bool {
        self.0.lock().unwrap().can_continue(machine_up)
    }
    pub fn continue_label(&self) -> String {
        self.0.lock().unwrap().continue_label().into()
    }
    pub fn next(&self) {
        self.0.lock().unwrap().next();
    }
    pub fn back(&self) {
        self.0.lock().unwrap().back();
    }
    pub fn restart(&self) {
        self.0.lock().unwrap().restart(f::disk_labels().default_gb);
    }
    pub fn setup(&self) -> OrigamiInstallSetup {
        let s = self.0.lock().unwrap().setup();
        OrigamiInstallSetup {
            method: s.method.map(Into::into),
            release: s.release,
            profile: s.profile,
            media_root: s.media_root.map(|p| p.to_string_lossy().into_owned()),
            addons: s
                .addons
                .into_iter()
                .map(|a| OrigamiInstallAddon {
                    name: a.name,
                    source: a.source,
                    install: a.install,
                })
                .collect(),
        }
    }
    /// Asked first when the assistant closes. The server always stops.
    pub fn close_machine_question(&self, machine_up: bool) -> Option<OrigamiCloseQuestion> {
        question(self.0.lock().unwrap().close_machine_question(machine_up))
    }
    /// Asked after that, with the machine's state and network as they are then.
    pub fn close_network_question(
        &self,
        machine_up: bool,
        network_private: bool,
    ) -> Option<OrigamiCloseQuestion> {
        question(
            self.0
                .lock()
                .unwrap()
                .close_network_question(machine_up, network_private),
        )
    }
}
