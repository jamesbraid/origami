//! The install assistant's choices and the rules for moving between its
//! steps, so the three apps share one behaviour and keep only their widgets.

use super::install::{
    InstallAddonChoice, InstallPlan, InstallRelease, InstallSetup, InstallSetupAddon, MediaFolder,
};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstallStep {
    Release,
    Media,
    Software,
    Disk,
    Check,
    Prepare,
    Installer,
    Packages,
    Finish,
}

impl InstallStep {
    pub fn title(self) -> &'static str {
        match self {
            Self::Release => "Release",
            Self::Media => "Media",
            Self::Software => "Software",
            Self::Disk => "Disk",
            Self::Check => "Check",
            Self::Prepare => "Prepare the disk",
            Self::Installer => "Start the installer",
            Self::Packages => "Install packages",
            Self::Finish => "Finish",
        }
    }
}

const ALL: [InstallStep; 9] = [
    InstallStep::Release,
    InstallStep::Media,
    InstallStep::Software,
    InstallStep::Disk,
    InstallStep::Check,
    InstallStep::Prepare,
    InstallStep::Installer,
    InstallStep::Packages,
    InstallStep::Finish,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Ready,
    Running,
    Passed,
    Failed,
}

/// The system disk the install will erase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallDisk {
    pub name: String,
    pub bus: u32,
    pub target: u32,
    /// What the Disk step says about it, the same in every app.
    pub text: String,
}

/// What the machine's disks were when the assistant read them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiskState {
    Unknown,
    None,
    Present(InstallDisk),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseAction {
    /// Do not close.
    Stay,
    StopMachine,
    LeaveRunning,
    SetBack,
    Leave,
}

/// A question to ask before the assistant closes, with its buttons in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseQuestion {
    pub heading: String,
    pub text: String,
    pub buttons: Vec<(String, CloseAction)>,
    /// The button Return picks: the one that destroys nothing.
    pub default: usize,
}

/// The words the apps share.
pub mod texts {
    pub const CHECKING: &str = "Checking the install media…";
    pub const PASSED: &str = "All sources answered.";
    pub const FINISHED: &str = "IRIX is installed. Start the machine to use it.";
    pub const QUIT_HEADING: &str = "An IRIX install is running.";
    pub const QUIT_TEXT: &str =
        "Quitting stops the install server and the machine. The install will have to start again.";
    pub const TAB_TEXT: &str = "Closing this tab stops the install server and the machine. The install will have to start again.";
    pub const KEEP: &str = "Keep Installing";
    pub const STOP_AND_CLOSE: &str = "Stop and Close";
    pub const STOP_AND_QUIT: &str = "Stop and Quit";
    pub const CHECK_INTRO: &str = "Origami will switch this machine to the private installation network until the install is done. Then it checks every disc.";
    pub const MEDIA_DOWNLOAD: &str = "Fetched while the install runs, so the internet must stay connected until the install finishes.";
    pub const MEDIA_FOLDER: &str = "A folder of disc images I already have";
}

pub struct InstallFlow {
    releases: Vec<InstallRelease>,
    step: InstallStep,
    resumed: bool,
    prepared: bool,
    release: String,
    profile: String,
    use_folder: bool,
    folder: Option<PathBuf>,
    folder_complete: bool,
    addons: Vec<(InstallAddonChoice, bool)>,
    extras: Vec<InstallSetupAddon>,
    disk: DiskState,
    new_disk_gb: u32,
    erase: bool,
    check: CheckState,
}

impl InstallFlow {
    /// A new assistant, or one opened on a machine whose setup `plan` reads
    /// back: it starts at Disk, and Back stops there.
    pub fn new(releases: Vec<InstallRelease>, plan: Option<InstallPlan>, new_disk_gb: u32) -> Self {
        let mut flow = Self {
            step: InstallStep::Release,
            resumed: plan.is_some(),
            prepared: plan.is_some(),
            release: String::new(),
            profile: String::new(),
            use_folder: false,
            folder: None,
            folder_complete: false,
            addons: Vec::new(),
            extras: Vec::new(),
            disk: DiskState::Unknown,
            new_disk_gb,
            erase: false,
            check: CheckState::Ready,
            releases,
        };
        match plan {
            Some(plan) => {
                flow.release = plan.release;
                flow.profile = plan.profile;
                flow.step = InstallStep::Disk;
            }
            None => flow.select_release(&flow.releases[0].release.clone()),
        }
        flow
    }

    fn first(&self) -> InstallStep {
        if self.resumed {
            InstallStep::Disk
        } else {
            InstallStep::Release
        }
    }

    pub fn step(&self) -> InstallStep {
        self.step
    }

    pub fn steps(&self) -> Vec<InstallStep> {
        ALL.to_vec()
    }

    pub fn resumed(&self) -> bool {
        self.resumed
    }

    pub fn releases(&self) -> &[InstallRelease] {
        &self.releases
    }

    pub fn release(&self) -> &str {
        &self.release
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Choose a release; the profile stays when it has it, else Standard.
    pub fn select_release(&mut self, release: &str) {
        let Some(r) = self.releases.iter().find(|r| r.release == release) else {
            return;
        };
        self.release = release.into();
        if r.profiles.iter().all(|p| p.name != self.profile) {
            let pick = r
                .profiles
                .iter()
                .find(|p| p.name == "standard")
                .unwrap_or(&r.profiles[0]);
            self.profile = pick.name.clone();
        }
        self.folder_complete = false;
    }

    pub fn select_profile(&mut self, profile: &str) {
        self.profile = profile.into();
        self.folder_complete = false;
    }

    pub fn use_folder(&self) -> bool {
        self.use_folder
    }

    pub fn set_use_folder(&mut self, on: bool) {
        self.use_folder = on;
    }

    pub fn folder(&self) -> Option<&PathBuf> {
        self.folder.as_ref()
    }

    /// The folder chosen and what the core found in it.
    pub fn set_folder(&mut self, folder: PathBuf, found: &MediaFolder) {
        self.folder = Some(folder);
        self.folder_complete = found.complete;
    }

    /// Take the core's add-on choices, keeping what the person ticked.
    pub fn set_choices(&mut self, choices: Vec<InstallAddonChoice>) {
        let before: Vec<(String, bool)> = self
            .addons
            .iter()
            .map(|(c, on)| (c.name.clone(), *on))
            .collect();
        self.addons = choices
            .into_iter()
            .map(|c| {
                let on = match before.iter().find(|(n, _)| *n == c.name) {
                    Some((_, on)) if !c.locked => *on,
                    _ => c.checked,
                };
                (c, on)
            })
            .collect();
    }

    pub fn addons(&self) -> &[(InstallAddonChoice, bool)] {
        &self.addons
    }

    pub fn tick(&mut self, name: &str, on: bool) {
        if let Some((c, state)) = self.addons.iter_mut().find(|(c, _)| c.name == name) {
            if !c.locked {
                *state = on;
            }
        }
    }

    /// Add a package from a file or HTTPS address with its selections, one
    /// per line of `selections`. False when there is no source.
    pub fn add_extra(&mut self, source: &str, selections: &str) -> bool {
        let source = source.trim();
        if source.is_empty() {
            return false;
        }
        let install = selections
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        self.extras.push(InstallSetupAddon {
            name: String::new(),
            source: Some(source.into()),
            install,
        });
        true
    }

    pub fn remove_extra(&mut self, index: usize) {
        if index < self.extras.len() {
            self.extras.remove(index);
        }
    }

    pub fn extras(&self) -> &[InstallSetupAddon] {
        &self.extras
    }

    pub fn set_disk(&mut self, disk: DiskState) {
        self.disk = disk;
    }

    pub fn disk(&self) -> &DiskState {
        &self.disk
    }

    pub fn new_disk_gb(&self) -> u32 {
        self.new_disk_gb
    }

    pub fn set_new_disk_gb(&mut self, gb: u32) {
        self.new_disk_gb = gb;
    }

    pub fn erase(&self) -> bool {
        self.erase
    }

    pub fn set_erase(&mut self, on: bool) {
        self.erase = on;
    }

    pub fn check(&self) -> CheckState {
        self.check
    }

    pub fn set_check(&mut self, state: CheckState) {
        self.check = state;
    }

    /// The setup has been written to the machine.
    pub fn set_prepared(&mut self) {
        self.prepared = true;
    }

    pub fn can_back(&self) -> bool {
        self.step > self.first() && self.step <= InstallStep::Check
    }

    /// Whether Continue (Finish on the last step) is on. Only a stopped
    /// machine can finish.
    pub fn can_continue(&self, machine_up: bool) -> bool {
        match self.step {
            InstallStep::Media => !self.use_folder || self.folder_complete,
            InstallStep::Disk => match self.disk {
                DiskState::Unknown => false,
                DiskState::Present(_) => self.erase,
                DiskState::None => self.new_disk_gb >= 1,
            },
            InstallStep::Check => self.check == CheckState::Passed,
            InstallStep::Finish => !machine_up,
            _ => true,
        }
    }

    pub fn continue_label(&self) -> &'static str {
        if self.step == InstallStep::Finish {
            "Finish"
        } else {
            "Continue"
        }
    }

    pub fn next(&mut self) {
        if let Some(next) = ALL.get(self.step as usize + 1) {
            self.step = *next;
        }
    }

    pub fn back(&mut self) {
        if !self.can_back() {
            return;
        }
        if self.step == InstallStep::Check {
            // The choices may change before the check is asked again.
            self.check = CheckState::Ready;
        }
        self.step = ALL[self.step as usize - 1];
    }

    /// Start over, after the machine's setup was forgotten.
    pub fn restart(&mut self, new_disk_gb: u32) {
        let releases = std::mem::take(&mut self.releases);
        let prepared = self.prepared;
        *self = Self::new(releases, None, new_disk_gb);
        self.prepared = prepared;
    }

    /// What `install_prepare` takes.
    pub fn setup(&self) -> InstallSetup {
        let known = self
            .addons
            .iter()
            .filter(|(_, on)| *on)
            .map(|(c, _)| InstallSetupAddon {
                name: c.name.clone(),
                source: None,
                install: vec![],
            });
        let extras = self.extras.iter().cloned();
        InstallSetup {
            release: self.release.clone(),
            profile: self.profile.clone(),
            media_root: self.use_folder.then(|| self.folder.clone()).flatten(),
            addons: known.chain(extras).collect(),
        }
    }

    /// Asked first when the assistant closes: what to do with the machine.
    /// None when there is nothing to ask. The server always stops on close.
    pub fn close_machine_question(&self, machine_up: bool) -> Option<CloseQuestion> {
        if !machine_up {
            return None;
        }
        let (heading, text) = match self.step {
            InstallStep::Prepare | InstallStep::Installer | InstallStep::Packages => (
                "Stop the machine?",
                "The install is partway through. Stopping the machine ends it, and the disk will have to be erased and installed again.",
            ),
            InstallStep::Finish => (
                "Stop the machine?",
                "IRIX may still be running. Stopping it now may leave the disk in need of repair. Shut IRIX down first with halt -p.",
            ),
            _ => return None,
        };
        Some(CloseQuestion {
            heading: heading.into(),
            text: text.into(),
            buttons: vec![
                ("Stop Machine".into(), CloseAction::StopMachine),
                ("Leave Running".into(), CloseAction::LeaveRunning),
                ("Back".into(), CloseAction::Stay),
            ],
            default: 2,
        })
    }

    /// Asked after the machine question: the network is still the
    /// installation network and the machine is stopped, so it can be set
    /// back. A running machine's network cannot change.
    pub fn close_network_question(
        &self,
        machine_up: bool,
        network_private: bool,
    ) -> Option<CloseQuestion> {
        (self.prepared && network_private && !machine_up).then(|| CloseQuestion {
            heading: "Set the network back?".into(),
            text: "The machine is still on the private installation network. Set the network back to what it was before the install?".into(),
            buttons: vec![
                ("Set the Network Back".into(), CloseAction::SetBack),
                ("Leave".into(), CloseAction::Leave),
            ],
            default: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade::install::install_releases;

    fn flow() -> InstallFlow {
        InstallFlow::new(install_releases(), None, 4)
    }

    #[test]
    fn a_new_assistant_walks_the_steps_and_gates_each_continue() {
        let mut f = flow();
        assert_eq!((f.release(), f.profile()), ("6.5.30", "standard"));
        assert!(!f.can_back());
        f.next();
        f.set_use_folder(true);
        assert!(!f.can_continue(false));
        f.set_use_folder(false);
        assert!(f.can_continue(false));
        f.next();
        f.next();
        assert_eq!(f.step(), InstallStep::Disk);
        assert!(!f.can_continue(false), "the disk is not read yet");
        f.set_disk(DiskState::None);
        assert!(f.can_continue(false));
        f.set_disk(DiskState::Present(InstallDisk {
            name: "system".into(),
            bus: 0,
            target: 1,
            text: String::new(),
        }));
        assert!(!f.can_continue(false));
        f.set_erase(true);
        assert!(f.can_continue(false));
        f.next();
        assert!(!f.can_continue(false), "the check has not passed");
        f.set_check(CheckState::Passed);
        assert!(f.can_continue(false) && f.can_back());
        f.back();
        assert_eq!(
            (f.step(), f.check()),
            (InstallStep::Disk, CheckState::Ready)
        );
        f.next();
        f.back();
        f.next();
        f.set_check(CheckState::Passed);
        f.next();
        assert_eq!(f.step(), InstallStep::Prepare);
        assert!(!f.can_back(), "no Back once the machine is started");
        f.next();
        f.next();
        f.next();
        assert_eq!(
            (f.step(), f.continue_label()),
            (InstallStep::Finish, "Finish")
        );
        assert!(!f.can_continue(true) && f.can_continue(false));
    }

    #[test]
    fn a_release_keeps_its_profile_only_when_it_has_it() {
        let mut f = flow();
        f.select_profile("minimal");
        f.select_release("6.5.22");
        assert_eq!(f.profile(), "standard");
        f.select_release("6.5.30");
        f.select_profile("minimal");
        assert_eq!(f.setup().profile, "minimal");
    }

    #[test]
    fn a_resumed_assistant_starts_at_disk_and_cannot_go_back() {
        let plan = InstallPlan {
            summary: String::new(),
            release: "6.5.30".into(),
            profile: "standard".into(),
            local_media: false,
            addons: vec!["mipspro".into()],
        };
        let f = InstallFlow::new(install_releases(), Some(plan.clone()), 4);
        assert_eq!(
            (f.step(), f.can_back(), f.resumed()),
            (InstallStep::Disk, false, true)
        );
        assert_eq!(plan.release, "6.5.30");
    }

    #[test]
    fn closing_asks_about_the_machine_then_the_network() {
        let mut f = flow();
        assert!(f.close_machine_question(true).is_none());
        assert!(
            f.close_network_question(false, true).is_none(),
            "nothing prepared yet"
        );
        f.set_prepared();
        assert!(f.close_network_question(false, true).is_some());
        f.restart(4);
        assert!(
            f.close_network_question(false, true).is_some(),
            "Start Over keeps it"
        );
        assert!(
            f.close_network_question(true, true).is_none(),
            "a running machine's network cannot change"
        );
        assert!(f.close_network_question(false, false).is_none());
        for _ in 0..5 {
            f.next();
        }
        let q = f.close_machine_question(true).unwrap();
        let actions: Vec<_> = q.buttons.iter().map(|b| b.1).collect();
        assert_eq!(
            actions,
            [
                CloseAction::StopMachine,
                CloseAction::LeaveRunning,
                CloseAction::Stay
            ]
        );
        assert!(f.close_machine_question(false).is_none());
    }

    #[test]
    fn added_packages_ride_with_the_setup() {
        let mut f = flow();
        assert!(!f.add_extra("  ", ""));
        assert!(f.add_extra("tablet.tardist", "TABLET.sw\n\n  TABLET.man \n"));
        let setup = f.setup();
        assert_eq!(setup.addons[0].install, ["TABLET.sw", "TABLET.man"]);
        f.remove_extra(0);
        assert!(f.setup().addons.is_empty());
    }
}
