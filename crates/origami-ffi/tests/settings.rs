//! Settings through the control plane, against the fake engine.
#![cfg(feature = "fake")]

mod common;

use common::*;
use origami_ffi::*;

fn forward(name: &str) -> OrigamiForward {
    OrigamiForward {
        name: name.into(),
        protocol: "tcp".into(),
        host_port: 2222,
        guest_port: 22,
    }
}

#[test]
fn every_change_kind_edits_the_stopped_machine() {
    let lab = Lab::new(None);
    let machine = lab.core.open_machine(lab.key.clone()).unwrap();
    let share_dir = lab.root.path().join("shared");
    std::fs::create_dir(&share_dir).unwrap();
    let image = lab.root.path().join("disc.iso");
    std::fs::write(&image, [0u8; 512]).unwrap();
    machine
        .apply_change(OrigamiChange::SetMemory { mib: 256 })
        .unwrap();
    machine
        .apply_change(OrigamiChange::SetProcessor { cpus_per_node: 1 })
        .unwrap();
    machine
        .set_network(OrigamiNetworkMode::Private, String::new(), None, None)
        .unwrap();
    machine
        .apply_change(OrigamiChange::AddForward {
            forward: forward("ssh"),
        })
        .unwrap();
    machine
        .apply_change(OrigamiChange::AddShare {
            name: "src".into(),
            host_dir: share_dir.to_str().unwrap().into(),
        })
        .unwrap();
    let s = machine.settings().unwrap();
    assert_eq!(
        (s.memory_per_node_mib, s.network.mode.as_str()),
        (256, "private")
    );
    assert_eq!((s.forwards.len(), s.shares.len()), (1, 1));
    machine
        .apply_change(OrigamiChange::RemoveForward { name: "ssh".into() })
        .unwrap();
    machine
        .apply_change(OrigamiChange::RemoveShare { name: "src".into() })
        .unwrap();
    machine
        .set_network(OrigamiNetworkMode::None, String::new(), None, None)
        .unwrap();
    let s = machine.settings().unwrap();
    assert_eq!(s.network.mode, "none");
    assert!(s.forwards.is_empty() && s.shares.is_empty());
    // There is no change to attach with, so give the file a drive by hand.
    let toml = std::path::Path::new(&lab.key).join("machine.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    text += &format!(
        "\n[[drive]]\nname = \"cdrom2\"\ntype = \"cdrom\"\nbus = 0\ntarget = 2\nimage = {:?}\nread_only = true\n",
        image.to_str().unwrap()
    );
    std::fs::write(&toml, text).unwrap();
    assert_eq!(machine.settings().unwrap().drives.len(), 1);
    machine
        .apply_change(OrigamiChange::DetachDrive {
            name: "cdrom2".into(),
        })
        .unwrap();
    assert!(machine.settings().unwrap().drives.is_empty());
}

#[test]
fn the_console_line_is_listed_chosen_and_put_back() {
    let lab = Lab::new(None);
    let machine = lab.core.open_machine(lab.key.clone()).unwrap();
    let s = machine.settings().unwrap();
    assert_eq!(s.console_port, "ttyS0");
    assert_eq!(s.console_ports.len(), 2);
    assert!(s.console_ports[0].is_default && !s.console_ports[1].is_default);
    machine
        .apply_change(OrigamiChange::SetConsole {
            port: Some("ttyS1".into()),
        })
        .unwrap();
    assert_eq!(machine.settings().unwrap().console_port, "ttyS1");
    machine
        .apply_change(OrigamiChange::SetConsole { port: None })
        .unwrap();
    assert_eq!(machine.settings().unwrap().console_port, "ttyS0");
}

#[test]
fn a_running_machine_refuses_an_edit_with_busy() {
    let lab = Lab::new(None);
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    let error = machine
        .apply_change(OrigamiChange::SetMemory { mib: 256 })
        .unwrap_err();
    assert!(matches!(error, CoreError::Busy { .. }), "{error}");
}

#[test]
fn another_library_root_lists_at_once_and_the_old_one_comes_back() {
    let lab = Lab::new(None);
    let other = tempfile::tempdir().unwrap();
    let names = |lab: &Lab| -> Vec<String> {
        lab.core
            .list_machines()
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect()
    };
    assert_eq!(names(&lab), ["m"]);
    lab.core
        .set_library_root(other.path().to_str().unwrap().into());
    assert_eq!(lab.core.library_root(), other.path().to_str().unwrap());
    assert!(names(&lab).is_empty());
    lab.core
        .set_library_root(lab.root.path().to_str().unwrap().into());
    assert_eq!(names(&lab), ["m"]);
}

#[test]
fn disks_are_imported_swapped_and_named() {
    let lab = Lab::new(None);
    let machine = lab.core.open_machine(lab.key.clone()).unwrap();
    let image = lab.root.path().join("disc.iso");
    std::fs::write(&image, [0u8; 512]).unwrap();
    let path = image.to_str().unwrap().to_string();
    let check = lab.core.check_disk_image(path.clone(), None).unwrap();
    assert_eq!((check.bytes, check.used_by), (512, None));
    let op = machine
        .import_drive(path.clone(), OrigamiDriveKind::Disk, 2, false, true)
        .unwrap();
    assert_eq!(op.wait(Some(WAIT_MS)), OrigamiOperationState::Succeeded);
    assert_eq!(op.fraction(), Some(1.0));
    let s = machine.settings().unwrap();
    assert_eq!(
        (s.drives.len(), s.disk_target, s.cd_target),
        (1, Some(1), Some(4))
    );
    assert_eq!(machine.drive_info("disk2".into()).unwrap(), "Unknown");
    machine
        .apply_change(OrigamiChange::AttachDrive {
            path,
            kind: OrigamiDriveKind::Cdrom,
            target: 4,
            overlay: false,
        })
        .unwrap();
    machine.change_media("cdrom4".into(), None).unwrap();
    assert_eq!(machine.settings().unwrap().drives.len(), 1);
}

#[test]
fn a_bridged_network_is_set_by_the_core_and_returns_advice_to_copy() {
    let lab = Lab::new(None);
    let machine = lab.core.open_machine(lab.key.clone()).unwrap();
    let set = |mode, device: &str, subnet: Option<&str>| {
        machine.set_network(mode, device.into(), None, subnet.map(Into::into))
    };
    set(OrigamiNetworkMode::Tap, "tap0", None).unwrap();
    let n = machine.settings().unwrap().network;
    assert_eq!((n.mode.as_str(), n.device.as_str()), ("tap", "tap0"));
    let lines = set(OrigamiNetworkMode::User, "", Some("10.42.0.0/24")).unwrap();
    let advice = lines
        .iter()
        .find(|l| l.kind == OrigamiLineKind::Advice)
        .unwrap();
    assert!(advice.shown.is_some());
    assert_eq!(
        advice.copy.as_deref(),
        Some("rm /var/adm/proclaim.lease_info* /etc/resolv.conf")
    );
    let bad = lab
        .core
        .preview_new_identity("fake.1".into(), None, "x".into(), None)
        .unwrap();
    assert_eq!(
        bad.problem.as_deref(),
        Some("Choose a number from 1 to 2999.")
    );
}

#[test]
fn a_share_carries_its_mount_commands_and_names_are_checked() {
    let lab = Lab::new(None);
    let machine = lab.core.open_machine(lab.key.clone()).unwrap();
    let dir = lab.root.path().join("shared");
    std::fs::create_dir(&dir).unwrap();
    let name = origami_suggest_share_name(dir.to_str().unwrap().into());
    assert_eq!(name, "shared");
    machine
        .apply_change(OrigamiChange::AddShare {
            name,
            host_dir: dir.to_str().unwrap().into(),
        })
        .unwrap();
    let share = machine.settings().unwrap().shares.remove(0);
    assert_eq!(
        share.mount_lines,
        [
            "mkdir -p /mnt/shared",
            "mount 10.98.0.4:/shared /mnt/shared"
        ]
    );
    assert!(share.mount_problem.is_none() && !share.needs_address);
    assert!(origami_check_share_name("a/b".into()).is_err());
}
