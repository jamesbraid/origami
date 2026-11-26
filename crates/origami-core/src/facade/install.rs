//! The install steps as operations: check the media, serve it, and drive
//! the installer on the guest's console.
//!
//! Instigator children are owned by the operation that started them: they
//! die when it is cancelled and when the core drops. The installer takes the
//! console's input lease through the runner (see [`super::lease`]), which
//! turns every other viewer read-only until the operation ends.

use super::child;
use super::core::Core;
use super::error::{CoreError, CoreResult, ErrorKind};
use super::lease::{self, InstallStart};
use super::operation::{Control as OpControl, InstallFailure, Operation};
use crate::Report;
use origami_machine::addressing::Addresses;
use origami_machine::{install, MachineFile};
use std::cell::Cell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A profile of an IRIX release: `name` is what the install takes, `label`
/// and `detail` are for showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallProfile {
    pub name: String,
    pub label: String,
    pub detail: String,
}

/// An IRIX release the assistant installs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallRelease {
    pub release: String,
    pub label: String,
    pub profiles: Vec<InstallProfile>,
}

/// How the assistant runs the install on a serial console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallMethod {
    /// Origami types the package commands, logs in and halts.
    Automatic,
    /// The person types them.
    Guided,
}

impl InstallMethod {
    fn name(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Guided => "guided",
        }
    }

    fn of(name: &str) -> Option<Self> {
        match name {
            "automatic" => Some(Self::Automatic),
            "guided" => Some(Self::Guided),
            _ => None,
        }
    }
}

/// What `install/media.toml` configures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallPlan {
    /// What it sets up, for the footer: "IRIX 6.5.30, Standard + MIPSpro".
    pub summary: String,
    /// How the person chose to run the install; a setup from before the choice was kept reads as Guided.
    pub method: InstallMethod,
    pub release: String,
    pub profile: String,
    pub local_media: bool,
    pub addons: Vec<String>,
}

/// A package to install after IRIX: one Origami knows by name, or a file
/// or HTTPS address with the selections to install from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallSetupAddon {
    /// Empty for a package from `source`: the core names it after the file.
    pub name: String,
    pub source: Option<String>,
    pub install: Vec<String>,
}

/// Everything the assistant asks before it prepares the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallSetup {
    pub method: Option<InstallMethod>,
    pub release: String,
    pub profile: String,
    /// A folder of disc images; without it the discs download.
    pub media_root: Option<PathBuf>,
    pub addons: Vec<InstallSetupAddon>,
}

/// An add-on the assistant lists, with whether the user can change it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallAddonChoice {
    pub name: String,
    pub label: String,
    pub note: Option<String>,
    pub checked: bool,
    /// The choice is made for the user: needed, or not offered here.
    pub locked: bool,
}

/// What a folder holds of an install's discs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFolder {
    pub found: u32,
    pub total: u32,
    pub missing: Vec<String>,
    /// The sentence to show under the chooser.
    pub verdict: String,
    pub complete: bool,
}

/// One thing to type at a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typed {
    /// The prompt it answers, or none when it is typed anywhere.
    pub prompt: Option<String>,
    /// What to type; `Return` for the key alone.
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepNote {
    pub text: String,
    pub warning: bool,
}

/// One thing to do at the machine while the install runs. `text` and `command`
/// repeat `title` and the first typed text for apps that have not moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuideStep {
    pub text: String,
    /// What to type or paste, for a Copy button.
    pub command: Option<String>,
    /// The exact bytes a serial console takes for it, line ends included;
    /// None for a step with nothing to send.
    pub send: Option<String>,
    pub title: String,
    /// The console text to wait for before typing.
    pub watch: Option<String>,
    pub typed: Vec<Typed>,
    pub note: Option<StepNote>,
    /// Console text that shows the step is done.
    pub confirm: Option<String>,
}

/// The steps that follow the machine's start, with this machine's release
/// and addresses in them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallGuide {
    /// The media server's address.
    pub server: String,
    pub prepare: Vec<GuideStep>,
    pub installer: Vec<GuideStep>,
    pub packages: Vec<GuideStep>,
    /// Quit and restart: the end of the package steps, and all that is left
    /// when the person fixed an install by hand at `Inst>`.
    pub restart: Vec<GuideStep>,
    pub finish: Vec<GuideStep>,
    /// Said under the package steps.
    pub package_note: String,
    /// Every step from the PROM menu to the halt, in order.
    pub steps: Vec<GuideStep>,
}

/// A guide step. `send` is the bytes a serial console takes for all of
/// `typed` at once; `confirm` is the console text that shows it is done.
fn guide_step(
    title: &str,
    watch: Option<&str>,
    typed: &[(Option<&str>, &str)],
    note: Option<(&str, bool)>,
    send: Option<String>,
    confirm: Option<&str>,
) -> GuideStep {
    GuideStep {
        title: title.into(),
        watch: watch.map(Into::into),
        typed: typed
            .iter()
            .map(|(prompt, text)| Typed {
                prompt: prompt.map(Into::into),
                text: (*text).into(),
            })
            .collect(),
        note: note.map(|(text, warning)| StepNote {
            text: text.into(),
            warning,
        }),
        confirm: confirm.map(Into::into),
        text: title.into(),
        command: typed.first().map(|t| t.1.into()),
        send,
    }
}

/// The footer while the install files are served: what they are, with no address.
pub fn install_footer(summary: &str) -> String {
    format!("Install files ready: {summary}")
}

/// What a guide step waits for, for the line under it.
pub fn waiting_for(prompt: &str) -> String {
    format!("Waiting for {prompt}")
}

/// The guide's header, such as `Install IRIX, step 3 of 22`.
pub fn guide_header(step: usize, total: usize) -> String {
    format!("Install IRIX, step {step} of {total}")
}

/// An add-on's full name and the short one the server line uses.
fn addon_names(name: &str) -> (&str, &str) {
    match name {
        "mipspro" => ("MIPSpro 7.4.4 compiler", "MIPSpro"),
        "rad4" => ("RAD4 driver", "RAD4"),
        other => (other, other),
    }
}

fn profile_label(profile: &str) -> &'static str {
    match profile {
        "minimal" => "Minimal",
        _ => "Standard",
    }
}

/// The releases the assistant offers, newest first.
pub fn install_releases() -> Vec<InstallRelease> {
    install::RELEASES
        .iter()
        .map(|(release, profiles)| InstallRelease {
            release: (*release).into(),
            label: format!("IRIX {release}"),
            profiles: profiles
                .iter()
                .map(|profile| {
                    let discs = install::local_media_paths(release, profile).map_or(0, |p| p.len());
                    let detail = if *profile == "minimal" {
                        format!("{discs} discs: overlay, foundation and NFS only")
                    } else {
                        format!("All {discs} discs")
                    };
                    InstallProfile {
                        name: (*profile).into(),
                        label: profile_label(profile).into(),
                        detail,
                    }
                })
                .collect(),
        })
        .collect()
}

/// What `folder` holds of the discs the release and profile install, as one
/// sentence and a count. Reads only directory entries.
pub fn install_media_folder(
    folder: &Path,
    release: &str,
    profile: &str,
) -> CoreResult<MediaFolder> {
    let paths = install::local_media_paths(release, profile).map_err(CoreError::machine)?;
    let mut roots: Vec<&str> = Vec::new();
    for root in paths.iter().filter_map(|path| path.split('/').next()) {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let total = paths.len() as u32;
    let missing: Vec<String> = paths
        .iter()
        .filter(|path| !folder.join(path).is_file())
        .cloned()
        .collect();
    let found = total - missing.len() as u32;
    let (verdict, complete) = if roots.iter().all(|root| !folder.join(root).is_dir()) {
        (
            format!("That folder has no {} folder.", roots.join(" or ")),
            false,
        )
    } else if missing.is_empty() {
        (format!("Found all {total} discs."), true)
    } else {
        (
            format!(
                "Found {found} of {total} discs. Missing: {}.",
                missing.join(", ")
            ),
            false,
        )
    };
    Ok(MediaFolder {
        found,
        total,
        missing,
        verdict,
        complete,
    })
}

/// A name for a package from `source`: the file's name without its
/// extensions, cut to what an add-on name allows.
fn addon_name_from(source: &str) -> String {
    let file = source
        .trim_end_matches('/')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("addon");
    let stem = file.split('.').next().unwrap_or(file);
    let name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() {
        "addon".into()
    } else {
        name
    }
}

/// The footer line of the media server: who serves what.
fn serving_line(dir: &Path, file: &MachineFile) -> origami_machine::Result<String> {
    let media = install::read_media(dir)?;
    let mut line = format!(
        "{} serving {}, {}",
        Addresses::of(file)?.host(),
        media.release,
        profile_label(&media.profile)
    );
    for addon in &media.addons {
        line.push_str(" + ");
        line.push_str(addon_names(&addon.name).1);
    }
    Ok(line)
}

fn cancelled(what: &str) -> CoreError {
    CoreError::new(ErrorKind::Cancelled, format!("{what} was cancelled"))
}

/// Turns the installer's console echo into progress lines.
struct ProgressSink<'a> {
    control: &'a OpControl,
    line: Vec<u8>,
}

impl Write for ProgressSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for &byte in bytes {
            match byte {
                b'\n' | b'\r' => {
                    let text = String::from_utf8_lossy(&self.line);
                    if !text.trim().is_empty() {
                        self.control.progress(text.trim());
                    }
                    self.line.clear();
                }
                _ if self.line.len() < 512 => self.line.push(byte),
                _ => {}
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Core {
    /// The add-ons to offer for an install of `profile` on the machine at
    /// `key`, with those the machine needs already ticked.
    pub fn install_addon_choices(
        &self,
        key: &str,
        profile: &str,
    ) -> CoreResult<Vec<InstallAddonChoice>> {
        let machine = self.open_machine(key)?;
        let file =
            origami_machine::read_machine(Path::new(machine.key())).map_err(CoreError::machine)?;
        let mut choices = Vec::new();
        if install::needs_rad4_driver(&file) {
            choices.push(InstallAddonChoice {
                name: "rad4".into(),
                label: addon_names("rad4").0.into(),
                note: Some("Needed by this machine. Downloaded.".into()),
                checked: true,
                locked: true,
            });
        }
        let standard = profile != "minimal";
        choices.push(InstallAddonChoice {
            name: "mipspro".into(),
            label: addon_names("mipspro").0.into(),
            note: (!standard).then(|| "Not in the Minimal install.".into()),
            checked: false,
            locked: !standard,
        });
        Ok(choices)
    }

    /// What the machine's `install/media.toml` configures; None before the
    /// machine is prepared.
    pub fn install_plan(&self, key: &str) -> CoreResult<Option<InstallPlan>> {
        let machine = self.open_machine(key)?;
        let dir = Path::new(machine.key());
        if !dir.join("install/media.toml").is_file() {
            return Ok(None);
        }
        let media = install::read_media(dir).map_err(CoreError::machine)?;
        let releases = install_releases();
        let release = releases.iter().find(|r| r.release == media.release);
        let profile = release
            .and_then(|r| r.profiles.iter().find(|p| p.name == media.profile))
            .map_or(media.profile.clone(), |p| p.label.clone());
        let summary = std::iter::once(format!(
            "{}, {profile}",
            release.map_or(format!("IRIX {}", media.release), |r| r.label.clone())
        ))
        .chain(
            media
                .addons
                .iter()
                .map(|a| addon_names(&a.name).0.to_string()),
        )
        .collect::<Vec<_>>()
        .join(" + ");
        Ok(Some(InstallPlan {
            summary,
            method: media
                .method
                .as_deref()
                .and_then(InstallMethod::of)
                .unwrap_or(InstallMethod::Guided),
            release: media.release,
            profile: media.profile,
            local_media: media.media.values().any(|source| !source.contains("://")),
            addons: media.addons.into_iter().map(|addon| addon.name).collect(),
        }))
    }

    /// Set the stopped machine up for an install: write its media list and
    /// add-ons and switch it to the installation network, replacing any
    /// earlier setup. Blocking, off the UI thread; `Busy` unless stopped.
    pub fn install_prepare(&self, key: &str, setup: &InstallSetup) -> CoreResult<Report> {
        let machine = self.open_machine(key)?;
        self.while_no_install(machine.key(), || {
            machine.edit_files(|engine, dir| {
                let before = origami_machine::read_machine(dir)?;
                if !install::runs_irix(
                    &before.machine.model,
                    before.machine.topology.as_deref().unwrap_or_default(),
                ) {
                    return Err("this machine does not run IRIX".into());
                }
                let saved = Saved::of(dir);
                let made = (|| -> origami_machine::Result<Report> {
                    remember_network(dir, &before)?;
                    remove_setup(dir)?;
                    let mut report = crate::install_init(
                        engine,
                        dir,
                        &crate::InstallInit {
                            media_root: setup.media_root.clone(),
                            mac: None,
                            release: setup.release.clone(),
                            profile: setup.profile.clone(),
                        },
                    )?;
                    for addon in &setup.addons {
                        let name = if addon.name.is_empty() {
                            addon_name_from(addon.source.as_deref().unwrap_or_default())
                        } else {
                            addon.name.clone()
                        };
                        // A named add-on the machine needs is configured by the init.
                        let have = install::read_media(dir)?
                            .addons
                            .iter()
                            .any(|a| a.name == name);
                        if have && addon.source.is_none() {
                            report.push(
                                crate::Kind::Note,
                                format!("add-on {name} was already set up"),
                            );
                            continue;
                        }
                        let request = crate::InstallAddon {
                            name,
                            source: addon.source.clone(),
                            install: addon.install.clone(),
                            base: None,
                            dist: None,
                            media_root: setup.media_root.clone(),
                        };
                        for line in crate::install_addon(engine, dir, &request)?.lines {
                            report.lines.push(line);
                        }
                    }
                    install::set_method(dir, setup.method.map(InstallMethod::name))?;
                    Ok(report)
                })();
                if made.is_err() {
                    saved.restore(dir);
                }
                made
            })
        })
    }

    /// Put the network back as it was before the setup moved the machine to
    /// the installation network: its mode, shared-network address, endpoint
    /// and interface. A machine with no record goes back to Shared (NAT).
    pub fn install_restore_network(&self, key: &str) -> CoreResult<Report> {
        let machine = self.open_machine(key)?;
        let dir = Path::new(machine.key());
        let saved = std::fs::read_to_string(dir.join(NETWORK_BEFORE))
            .ok()
            .and_then(|text| serde_json::from_str::<crate::NetworkChange>(&text).ok());
        let _ = std::fs::remove_file(dir.join(NETWORK_BEFORE));
        // Only the installation network is put back: a change the person
        // made meanwhile stays.
        let file = origami_machine::read_machine(dir).map_err(CoreError::machine)?;
        if file.network.mode != "private" {
            return Ok(Report::default());
        }
        let change = saved.unwrap_or(crate::NetworkChange {
            mode: crate::NetworkMode::User,
            endpoint: None,
            interface: None,
            share_address: None,
            subnet: None,
            mac: None,
        });
        machine.apply_change(&crate::Change::SetNetwork(change))
    }

    /// Whether a setup moved this machine to the installation network and
    /// has not been put back, so the assistant offers to set it back.
    pub fn install_restore_pending(&self, key: &str) -> CoreResult<bool> {
        let machine = self.open_machine(key)?;
        Ok(Path::new(machine.key()).join(NETWORK_BEFORE).is_file())
    }

    /// Forget the machine's install setup so it can be prepared again. The
    /// network stays as the setup left it.
    pub fn install_reset(&self, key: &str) -> CoreResult<()> {
        let machine = self.open_machine(key)?;
        machine.edit_files(|_, dir| remove_setup(dir))
    }

    /// The system disk the install erases: the machine's first disk, named
    /// the same in every app. None when it has no disk.
    pub fn install_disk(&self, key: &str) -> CoreResult<Option<crate::facade::InstallDisk>> {
        let machine = self.open_machine(key)?;
        let dir = Path::new(machine.key());
        let file = origami_machine::read_machine(dir).map_err(CoreError::machine)?;
        Ok(super::core::system_drive(&file).map(|d| {
            let image = origami_machine::resolve(dir, &d.image);
            let size = (!origami_machine::is_chd(&image))
                .then(|| std::fs::metadata(&image).ok())
                .flatten()
                .map(|m| format!(" ({})", super::size_label(m.len())))
                .unwrap_or_default();
            let file_name = image
                .file_name()
                .map_or(d.name.clone(), |n| n.to_string_lossy().into_owned());
            let others = file.drive.iter().filter(|o| o.kind == "disk").count() - 1;
            let more = if others > 0 {
                format!(" The machine has {others} more disk, which the formatter lists too: choose only this one.")
            } else {
                String::new()
            };
            crate::facade::InstallDisk {
                name: file_name.clone(),
                bus: d.bus,
                target: d.target,
                text: format!(
                    "{file_name}{size} on SCSI bus {}, target {}. The disk formatter erases everything on it.{more}",
                    d.bus, d.target
                ),
            }
        }))
    }

    /// The steps to follow at the machine, from its install setup.
    pub fn install_guide(&self, key: &str) -> CoreResult<InstallGuide> {
        let machine = self.open_machine(key)?;
        let dir = Path::new(machine.key());
        let file = origami_machine::read_machine(dir).map_err(CoreError::machine)?;
        let media = install::read_media(dir).map_err(CoreError::machine)?;
        let addresses = Addresses::of(&file).map_err(CoreError::machine)?;
        let disk = self.install_disk(key)?.ok_or_else(|| {
            CoreError::new(
                ErrorKind::Invalid,
                "the machine has no disk to install onto",
            )
        })?;
        let loaded = crate::open(machine.engine().as_ref(), dir).map_err(CoreError::machine)?;
        let offering = loaded.offering().map_err(CoreError::machine)?;
        let (fx_name, source_choice) =
            install::installer_for(&offering.product, &offering.topology);
        let (host, guest, release) = (addresses.host(), addresses.guest(), media.release);
        let fx = format!("boot -f bootp():/{release}/stand/{fx_name}");
        let dist = format!("/{release}/dist");
        let source = format!("admin source {host}:/irix.cmds");
        let host = host.to_string();
        let size = super::core::system_drive(&file).and_then(|d| {
            let image = origami_machine::resolve(dir, &d.image);
            (!origami_machine::is_chd(&image))
                .then(|| std::fs::metadata(&image).ok())
                .flatten()
                .map(|m| super::size_label(m.len()))
        });
        let serial = machine.boot_console()? == super::BootConsole::Serial;
        let name = origami_machine::addressing::dhcp_hostname(dir).unwrap_or("irix");
        let guest = guest.to_string();
        let go = |keys: String| serial.then_some(keys);
        // IRIX numbers controllers by probe order; only bus 0 is known to be 0.
        let (ctlr, ctlr_send, drive_note) = if disk.bus == 0 {
            ("0", "0", None)
        } else {
            (
                "Return",
                "",
                Some((
                    format!(
                        "fx looks at controller 0, which may not be this disk. Compare the size fx reports with {} for {}. If they differ, stop: this is the wrong disk.",
                        size.as_deref().unwrap_or("this disk's size"),
                        disk.name
                    ),
                    true,
                )),
            )
        };
        let target = disk.target.to_string();
        let steps = vec![
            guide_step(
                "Get past \"Unable to boot\"",
                Some("Unable to boot"),
                &[],
                Some(("Press any key. If the PROM does not say this, go on.", false)),
                go("\r".into()),
                Some("Option? "),
            ),
            guide_step(
                "Open the command monitor",
                Some("Option? "),
                &[(Some("Option?"), "5")],
                None,
                go("5\r".into()),
                Some(">> "),
            ),
            guide_step(
                "Start the disk tool",
                Some(">> "),
                &[(None, &fx)],
                None,
                go(format!("{fx}\r")),
                Some("extended mode with all options available?"),
            ),
            guide_step(
                "Choose the disk",
                Some("extended mode with all options available?"),
                &[
                    (Some("extended mode"), "yes"),
                    (Some("device name"), "Return"),
                    (Some("ctlr#"), ctlr),
                    (Some("drive#"), &target),
                ],
                drive_note.as_ref().map(|(t, w)| (t.as_str(), *w)),
                go(format!("yes\r\r{ctlr_send}\r{target}\r")),
                Some("fx> "),
            ),
            guide_step(
                "Label the disk",
                Some("fx> "),
                &[(Some("fx>"), "/l/sy")],
                None,
                go("/l/sy\r".into()),
                Some("writing label info"),
            ),
            guide_step(
                "Leave the disk tool",
                Some("writing label info"),
                &[(None, "/exi")],
                Some(("This erases the disk.", true)),
                go("/exi\r".into()),
                Some("Option? "),
            ),
            guide_step(
                "Open the installer",
                Some("Option? "),
                &[(Some("Option?"), "2")],
                None,
                go("2\r".into()),
                Some("or <enter> to start:"),
            ),
            guide_step(
                "Choose Remote Directory",
                Some("or <enter> to start:"),
                &[(None, source_choice)],
                None,
                go(format!("{source_choice}\r")),
                Some("name of the remote host"),
            ),
            guide_step(
                "Enter the host",
                Some("name of the remote host"),
                &[(Some("name of the remote host"), &host)],
                None,
                go(format!("{host}\r")),
                Some("directory"),
            ),
            guide_step(
                "Enter the directory",
                Some("directory"),
                &[(Some("directory"), &dist)],
                None,
                go(format!("{dist}\r")),
                Some("<enter> to start:"),
            ),
            guide_step(
                "Start the installer",
                Some("<enter> to start:"),
                &[(Some("<enter> to start"), "Return")],
                None,
                go("\r".into()),
                Some("Make new file system"),
            ),
            guide_step(
                "Make the file system",
                Some("Make new file system"),
                &[(Some("Make new file system"), "yes")],
                None,
                go("yes\r".into()),
                Some("Are you sure?"),
            ),
            guide_step(
                "Confirm it",
                Some("Are you sure?"),
                &[(Some("Are you sure?"), "y")],
                None,
                go("y\r".into()),
                Some("Block size of filesystem"),
            ),
            guide_step(
                "Choose the block size",
                Some("Block size of filesystem"),
                &[(Some("block size"), "4096")],
                None,
                go("4096\r".into()),
                Some("hostname (system name)"),
            ),
            guide_step(
                "Name the machine",
                Some("hostname (system name)"),
                &[(Some("hostname"), name)],
                None,
                go(format!("{name}\r")),
                Some("network address of"),
            ),
            guide_step(
                "Give it an address",
                Some("network address of"),
                &[(Some("network address"), &guest)],
                None,
                go(format!("{guest}\r")),
                Some("IP class default"),
            ),
            guide_step(
                "Set the netmask",
                Some("IP class default"),
                &[(Some("IP class default"), "255.255.255.0")],
                Some(("The default is wrong for this network.", true)),
                go("255.255.255.0\r".into()),
                Some("Inst> "),
            ),
            guide_step(
                "Point the installer at the packages",
                Some("Inst> "),
                &[(Some("Inst>"), &source)],
                None,
                go(format!("{source}\r")),
                Some("Install software from:"),
            ),
            guide_step(
                "Finish choosing",
                Some("Install software from:"),
                &[(Some("Install software from:"), "done")],
                None,
                go("done\r".into()),
                Some("Reading fileset information"),
            ),
            guide_step(
                "Wait for the packages",
                Some("instigator: post-install commands finished"),
                &[],
                Some((
                    "If a conflict or a failed command stops it, the line never appears. Read the console and fix it at Inst>.",
                    false,
                )),
                None,
                Some("instigator: post-install commands finished"),
            ),
            guide_step(
                "Quit the installer",
                Some("Inst> "),
                &[(Some("Inst>"), "quit")],
                Some(("Relinking the kernel takes a few minutes.", false)),
                go("quit\r".into()),
                Some("Restart?"),
            ),
            guide_step(
                "Restart",
                Some("Restart?"),
                &[(Some("Restart?"), "y")],
                None,
                go("y\r".into()),
                Some("login:"),
            ),
            guide_step(
                "Log in and halt",
                Some("login:"),
                &[(Some("login:"), "root"), (None, "halt -p")],
                Some(("A new install has no password.", false)),
                None,
                None,
            ),
        ];
        // The older per-page lists are slices of the one list.
        let page = |range: std::ops::Range<usize>| steps[range].to_vec();
        Ok(InstallGuide {
            prepare: page(0..6),
            installer: page(6..17),
            packages: page(17..20),
            restart: page(20..22),
            finish: page(22..23),
            package_note: "If a conflict or a failed command stops it, the line above never appears. Read the console and fix it at Inst>.".into(),
            server: host,
            steps,
        })
    }

    /// Check the install media of the machine at `key`. Cancelling ends the
    /// check and its child.
    pub fn install_check(&self, key: &str, full: bool) -> CoreResult<Operation> {
        let instigator = self.instigator()?;
        let machine = self.open_machine(key)?;
        let (dir, engine) = (PathBuf::from(machine.key()), machine.engine());
        self.install_operation(&machine, CHECK_TITLE, move |control| {
            let loaded = crate::open(engine.as_ref(), &dir).map_err(CoreError::machine)?;
            control.progress("Checking the install media");
            let (mut command, document) =
                install::check_command(|| Ok(instigator), &dir, &loaded.file, full)
                    .map_err(CoreError::machine)?;
            match child::run(&mut command, Some(document), control)? {
                None => Err(cancelled("the check")),
                Some(done) if done.status.success() => Ok(None),
                Some(done) => Err(CoreError::new(
                    ErrorKind::Invalid,
                    install::check_rejected(&done.stderr),
                )),
            }
        })
    }

    /// Serve the install media of the machine at `key` until the operation
    /// is cancelled or the server exits. The server is the operation's
    /// child; cancelling ends it and anything it started.
    pub fn install_serve(&self, key: &str) -> CoreResult<Operation> {
        let instigator = self.instigator()?;
        let machine = self.open_machine(key)?;
        let (dir, engine) = (PathBuf::from(machine.key()), machine.engine());
        let (halted, name) = (self.halted(), machine.key().to_owned());
        self.install_operation(&machine, SERVE_TITLE, move |control| {
            let loaded = crate::open(engine.as_ref(), &dir).map_err(CoreError::machine)?;
            let line = serving_line(&dir, &loaded.file).map_err(CoreError::machine)?;
            let install::ServeSetup { mut command, .. } =
                install::serve_setup(|| Ok(instigator), &dir, &loaded.file)
                    .map_err(CoreError::machine)?;
            control.progress(line);
            match child::run(&mut command, None, control)? {
                None => Err(cancelled("the media server")),
                Some(done) if done.status.success() || server_ended_normally(&halted, &name) => {
                    Ok(None)
                }
                Some(done) => Err(CoreError::new(
                    ErrorKind::Io,
                    format!("Instigator exited with {}", done.status),
                )),
            }
        })
    }

    /// Drive the installer on the console of the machine at `key` from
    /// `Inst>` to the installed system's login prompt. The operation holds
    /// the console's input lease and releases it when it ends, is cancelled
    /// or loses its connection; cancelling leaves the machine running.
    pub fn install_apply(&self, key: &str, start: InstallStart) -> CoreResult<Operation> {
        self.apply_operation(key, start, false)
    }

    /// As [`Core::install_apply`] for an app: the guest must be sitting at
    /// `Inst>` on a serial console, and after the packages the machine is
    /// logged into as root and halted. The operation ends when it powers
    /// off. A failure says how far it got through [`Operation::install_failure`].
    pub fn install_apply_from(&self, key: &str) -> CoreResult<Operation> {
        let machine = self.open_machine(key)?;
        if machine.boot_console()? != super::BootConsole::Serial {
            return Err(CoreError::new(
                ErrorKind::Unsupported,
                "this machine's PROM prints on the screen, so the installer cannot be typed for",
            ));
        }
        // A console this core is not reading (the machine runs under another
        // process) cannot be checked; the install then waits for the prompt.
        let end = machine.console_read(u64::MAX, 0).next;
        let tail = machine.console_read(end.saturating_sub(64), 64).bytes;
        if machine.view().console.connected && !tail.ends_with(b"Inst> ") {
            return Err(CoreError::new(
                ErrorKind::Invalid,
                "the guest is not at Inst>. Get it there in the Console first",
            ));
        }
        self.apply_operation(key, InstallStart::Attached, true)
    }

    fn apply_operation(
        &self,
        key: &str,
        start: InstallStart,
        shutdown: bool,
    ) -> CoreResult<Operation> {
        let machine = self.open_machine(key)?;
        let (dir, engine) = (PathBuf::from(machine.key()), machine.engine());
        let (halted, name) = (self.halted(), machine.key().to_owned());
        halted.lock().unwrap().remove(&name);
        self.install_operation(&machine, "Install IRIX", move |control| {
            let loaded = crate::open(engine.as_ref(), &dir).map_err(CoreError::machine)?;
            let start = Cell::new(Some(start));
            let cancel = control.canceller();
            let mut sink = ProgressSink {
                control,
                line: Vec::new(),
            };
            let result = install::apply(
                &dir,
                &loaded.file,
                &|dir| {
                    let start = start.take().expect("the installer connects once");
                    lease::connect(dir, start, cancel.clone())
                },
                &mut sink,
                shutdown,
            );
            match result {
                Ok(()) => {
                    if shutdown {
                        halted.lock().unwrap().insert(name);
                    }
                    Ok(None)
                }
                Err(error) => {
                    let cancel = control.is_cancelled();
                    control.install_failure(failure_kind(error.stage, error.stopped, cancel));
                    if cancel {
                        Err(cancelled("the install"))
                    } else {
                        Err(CoreError::machine(Box::new(error)))
                    }
                }
            }
        })
    }
}

/// The server also exits (status 1, "private network disconnected") when
/// the guest powers off after a finished automated install.
pub(super) fn server_ended_normally(
    halted: &Mutex<std::collections::HashSet<String>>,
    key: &str,
) -> bool {
    halted.lock().unwrap().contains(key)
}

const NETWORK_BEFORE: &str = "install/network-before.json";

/// Keep the network the machine had before its first setup, so Finish and
/// the close question can put exactly that back.
fn remember_network(dir: &Path, file: &MachineFile) -> origami_machine::Result<()> {
    let path = dir.join(NETWORK_BEFORE);
    // Whatever the person set since the last setup is the new "before".
    if file.network.mode == "private" {
        return Ok(());
    }
    let mode = match file.network.mode.as_str() {
        "none" => crate::NetworkMode::None,
        "tap" => crate::NetworkMode::Tap,
        "vmnet" => crate::NetworkMode::Vmnet,
        _ => crate::NetworkMode::User,
    };
    let before = crate::NetworkChange {
        mode,
        endpoint: file.network.endpoint.clone(),
        interface: file.network.interface.clone(),
        share_address: file.network.share_address.clone(),
        subnet: None,
        mac: None,
    };
    std::fs::create_dir_all(dir.join("install"))?;
    std::fs::write(path, serde_json::to_vec(&before)?)?;
    Ok(())
}

/// What an automated install that stopped at `stage` offers the person. A
/// cancel keeps its stage once the packages are in, so the person is told
/// to halt in the Console, not to finish at `Inst>`.
pub(super) fn failure_kind(
    stage: install::ApplyStage,
    stopped: bool,
    cancelled: bool,
) -> InstallFailure {
    use install::ApplyStage::*;
    match (stage, stopped, cancelled) {
        (Shutdown, _, _) => InstallFailure::Shutdown,
        (Login, true, false) => InstallFailure::Conflict,
        (Login, _, _) => InstallFailure::Login,
        (_, _, true) => InstallFailure::Cancelled,
        (Prompt, _, _) => InstallFailure::Setup,
        (_, true, _) => InstallFailure::Conflict,
        (Packages, false, _) => InstallFailure::Lost,
    }
}

/// The files `install_prepare` rewrites, so a failed setup puts them back.
struct Saved {
    network: Option<String>,
    machine: Option<String>,
    media: Option<String>,
    addons: Vec<(PathBuf, String)>,
}

impl Saved {
    fn of(dir: &Path) -> Self {
        let addons = std::fs::read_dir(dir.join("install/addons"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Some((e.path(), std::fs::read_to_string(e.path()).ok()?)))
            .collect();
        Self {
            network: std::fs::read_to_string(dir.join(NETWORK_BEFORE)).ok(),
            machine: std::fs::read_to_string(dir.join("machine.toml")).ok(),
            media: std::fs::read_to_string(dir.join("install/media.toml")).ok(),
            addons,
        }
    }

    fn restore(self, dir: &Path) {
        let _ = remove_setup(dir);
        match self.network {
            Some(text) => {
                let _ = std::fs::write(dir.join(NETWORK_BEFORE), text);
            }
            None => {
                let _ = std::fs::remove_file(dir.join(NETWORK_BEFORE));
            }
        }
        if let Some(text) = self.machine {
            let _ = std::fs::write(dir.join("machine.toml"), text);
        }
        if let Some(text) = self.media {
            let _ = std::fs::create_dir_all(dir.join("install"));
            let _ = std::fs::write(dir.join("install/media.toml"), text);
        }
        for (path, text) in self.addons {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(dir));
            let _ = std::fs::write(path, text);
        }
    }
}

const SERVE_TITLE: &str = "Serve install media";
const CHECK_TITLE: &str = "Check install media";

/// Removes the media list and the add-on files `install_prepare` wrote.
fn remove_setup(dir: &Path) -> origami_machine::Result<()> {
    let install = dir.join("install");
    for path in [install.join("media.toml"), install.join("addons")] {
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}
