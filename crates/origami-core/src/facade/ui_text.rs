//! Words the three apps show in the same places: dialog titles, buttons,
//! labels and tooltips. An app asks for one by variant, so they cannot drift.

/// Defines [`UiString`] from one list of variants and their text.
macro_rules! ui_strings {
    ($($variant:ident => $text:expr,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum UiString {
            $($variant),*
        }

        impl UiString {
            pub const ALL: &'static [UiString] = &[$(UiString::$variant),*];

            pub fn text(self) -> &'static str {
                match self {
                    $(UiString::$variant => $text),*
                }
            }
        }
    };
}

ui_strings! {
    Origami => "Origami",
    AboutOrigami => "About Origami",
    Acknowledgements => "Acknowledgements",
    Add => "Add",
    AddADiskImage => "Add a disk image…",
    AddAPackage => "Add a package",
    AddAPackage2 => "Add a package…",
    AddAPortForward => "Add a port forward…",
    Address => "Address",
    AddressForSharedFolders => "Address for shared folders",
    Advanced => "Advanced",
    Artwork => "Artwork",
    Back => "Back",
    Bridged => "Bridged",
    BuiltOn => "Built on",
    Cancel => "Cancel",
    CancelInstall => "Cancel install",
    CancelQuit => "Cancel quit",
    CaptureMouse => "Capture mouse",
    CaptureOnClick => "Capture on click",
    ChangeCd => "Change CD",
    CheckAgain => "Check again",
    CheckNow => "Check now",
    CheckThoroughlyAlsoReadsThe => "Check thoroughly (also reads the downloaded discs)",
    Choose => "Choose…",
    ChooseAnImage => "Choose an image…",
    ChooseFile => "Choose file…",
    ChooseFolder => "Choose folder…",
    ChooseTheFolderThatHoldsYourMachines => "Choose the folder that holds your machines",
    Console => "Console",
    ConsoleSerialLine => "Console serial line",
    Copy => "Copy",
    CopyReport => "Copy report",
    Cpu => "CPU",
    Create => "Create",
    Custom => "Custom",
    Detach => "Detach",
    Diagnostics => "Diagnostics",
    Disk => "Disk",
    DiskSize => "Disk size",
    DiskSizeGb => "Disk size (GB)",
    Done => "Done",
    DownloadThem => "Download them",
    DownloadedDisks => "Downloaded disks",
    Drives => "Drives",
    Eject => "Eject",
    EraseThisDiskAndInstall => "Erase this disk and install onto it",
    EthernetAddress => "Ethernet address",
    Folder => "Folder",
    FollowMyMouse => "Follow my mouse",
    ForceQuitInstead => "Force quit instead",
    FromTheMachineId => "From the machine ID",
    Gb => "GB",
    General => "General",
    Graphics => "Graphics",
    GuestPort => "Guest port",
    HostPort => "Host port",
    HowShouldChangesBeKept => "How should changes be kept?",
    HowToSetThisUp => "How to set this up",
    IFixedItAtInst => "I fixed it at Inst>",
    Input => "Input",
    InsertACdImage => "Insert a CD image…",
    Install => "Install",
    InstallIrix => "Install IRIX",
    InstallIrixAfterCreating => "Install IRIX after creating",
    KeepRunning => "Keep running",
    KeyboardShortcuts => "Keyboard shortcuts",
    Licences => "Licences",
    Machine => "Machine",
    MachineId => "Machine ID",
    MachineIdRange => "Machine ID (1 to 2999)",
    MemoryPerNode => "Memory per node",
    Mode => "Mode",
    Model => "Model",
    More => "More",
    Name => "Name",
    NameInIrix => "Name in IRIX",
    NeedsAnInternetConnection => "Needs an internet connection until the install ends.",
    NeedsUpgrade => "Needs upgrade",
    NewMachine => "New machine",
    NewMachineFromThisDisk => "New machine from this disk",
    NoDownloadedDisks => "No downloaded disks",
    Nodes => "Nodes",
    OpenConsole => "Open console",
    OpenInNewTab => "Open in new tab",
    OpenTheMainMenu => "Open the main menu",
    PackagesToInstallOnePerLine => "Packages to install, one per line",
    Pause => "Pause",
    Photographs => "Photographs",
    Pointer => "Pointer",
    PortForwards => "Port forwards",
    PowerOff => "Power off",
    PowerOffToChangeHardwareSettings => "Power off to change hardware settings.",
    PoweringOff => "Powering off",
    Preferences => "Preferences",
    Processor => "Processor",
    ProcessorsPerNode => "Processors per node",
    QuitAndLeaveRunning => "Quit and leave running",
    Reading => "Reading…",
    Recommended => "Recommended",
    Remove => "Remove",
    Restart => "Restart",
    RunTheseAsRootIn => "Run these as root in IRIX once it is up with nfs.sw.nfs installed. Files you write land on this computer as you.",
    Screen => "Screen",
    SearchComponentsAndLicences => "Search components and licences",
    Seen => "Seen",
    SelectAMachineFirst => "Select a machine first.",
    SetTheAddressInNetwork => "Set the address in Network",
    Settings => "Settings",
    ShareAFolder => "Share a folder…",
    ShowDetails => "Show details",
    ShowMachine => "Show machine",
    ShowOrHideTheEmulated => "Show or hide the emulated CPU load",
    ShowOrHideTheLibrary => "Show or hide the library",
    SourceAFileOrAn => "Source: a file or an HTTPS address",
    Start => "Start",
    StartAgain => "Start again",
    StartOver => "Start over",
    StartingUpTheSystem => "Starting up the system…",
    StartupFirmware => "Startup & firmware",
    StartupMessagesOn => "Startup messages on",
    StopSharing => "Stop sharing",
    Storage => "Storage",
    Thanks => "Thanks",
    TheConsoleWindowAndA => "The Console window and a headless machine's tab use this line. On a machine with graphics, the PROM may print on the screen instead.",
    TheMachineStopped => "The machine stopped.",
    ThisMachineIsRunningHardware => "This machine is running. Hardware settings are locked until it is powered off.",
    TypeIt => "Type it",
    TypeLabel => "Type",
    Upgrade => "Upgrade",
    UseDiscsIAlreadyHave => "Use discs I already have",
    UseThisAddressInstead => "Use this address instead",
    WatchFor => "Watch for",
    WhereShouldTheDiscsComeFrom => "Where should the discs come from?",
    WhereShouldTheDiskLive => "Where should the disk live?",
}

/// What the Machine ID field says under it; `next` is the next free ID.
pub fn machine_id_caption(next: u32) -> String {
    format!("Sets the machine's serial number, host ID and Ethernet address. {next} is the next free ID.")
}

/// The pointer hint over the picture while the mouse is captured.
pub fn capture_hint(platform: super::Platform) -> String {
    let keys = if platform == super::Platform::Macos {
        "⌃⌥"
    } else {
        "Ctrl+Alt"
    };
    format!("Mouse captured. Press {keys} to release.")
}

/// The line shown while a machine powers off; `force` is a force quit.
pub fn power_off_progress(name: &str, force: bool) -> String {
    if force {
        format!("Force quitting {name}…")
    } else {
        format!("Powering off {name}…")
    }
}

/// The About version line, such as `Version 0.6.2`, from a bare version or a
/// tag (`v0.6.2`).
pub fn version_line(version: &str) -> String {
    format!("Version {}", version.trim().trim_start_matches('v'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_line_is_the_same_for_a_tag_and_a_number() {
        assert_eq!(version_line("0.6.2"), "Version 0.6.2");
        assert_eq!(version_line("v0.6.2"), "Version 0.6.2");
    }

    #[test]
    fn the_hints_name_the_platforms_release_keys() {
        use super::super::Platform;
        assert_eq!(
            capture_hint(Platform::Macos),
            "Mouse captured. Press ⌃⌥ to release."
        );
        assert_eq!(
            capture_hint(Platform::Linux),
            "Mouse captured. Press Ctrl+Alt to release."
        );
        assert_eq!(power_off_progress("Fuel", false), "Powering off Fuel…");
        assert_eq!(power_off_progress("Fuel", true), "Force quitting Fuel…");
    }

    #[test]
    fn the_machine_id_caption_names_the_next_free_id() {
        assert!(machine_id_caption(7).ends_with("7 is the next free ID."));
    }

    #[test]
    fn no_two_variants_say_the_same_thing() {
        let mut all: Vec<_> = UiString::ALL.iter().map(|s| s.text()).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), UiString::ALL.len());
    }
}
