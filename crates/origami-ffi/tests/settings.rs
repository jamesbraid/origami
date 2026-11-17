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
        .apply_change(OrigamiChange::SetNetwork {
            mode: OrigamiNetworkMode::Private,
        })
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
        (s.memory_per_node_mib, s.network_mode.as_str()),
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
        .apply_change(OrigamiChange::SetNetwork {
            mode: OrigamiNetworkMode::None,
        })
        .unwrap();
    let s = machine.settings().unwrap();
    assert_eq!(s.network_mode, "none");
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
fn a_running_machine_refuses_an_edit_with_busy() {
    let lab = Lab::new(None);
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    let error = machine
        .apply_change(OrigamiChange::SetMemory { mib: 256 })
        .unwrap_err();
    assert!(matches!(error, CoreError::Busy { .. }), "{error}");
}
