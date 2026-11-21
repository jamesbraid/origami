//! The install assistant's calls through the control plane, against the fake
//! engine and its stand-in Instigator.
#![cfg(feature = "fake")]

mod common;

use common::*;
use origami_ffi::*;

#[test]
fn a_machine_is_prepared_checked_served_and_reset() {
    let lab = Lab::new(None);
    let key = lab.key.clone();
    assert!(lab.core.install_plan(key.clone()).unwrap().is_none());
    let machine = lab.core.open_machine(key.clone()).unwrap();
    machine
        .apply_change(OrigamiChange::CreateDisk { size_mib: 64 })
        .unwrap();
    assert_eq!(machine.settings().unwrap().drives.len(), 1);
    let setup = OrigamiInstallSetup {
        release: "6.5.30".into(),
        profile: "minimal".into(),
        media_root: None,
        addons: vec![],
    };
    lab.core.install_prepare(key.clone(), setup).unwrap();
    let plan = lab.core.install_plan(key.clone()).unwrap().unwrap();
    assert_eq!(
        (plan.release.as_str(), plan.profile.as_str()),
        ("6.5.30", "minimal")
    );
    assert_eq!(
        lab.core.install_guide(key.clone()).unwrap().server,
        "10.98.0.2"
    );

    let check = lab.core.install_check(key.clone(), false).unwrap();
    assert_eq!(check.wait(Some(10_000)), OrigamiOperationState::Succeeded);

    let serve = lab.core.install_serve(key.clone()).unwrap();
    assert_eq!(serve.wait(Some(300)), OrigamiOperationState::Running);
    assert!(
        serve.progress().ends_with("serving 6.5.30, Minimal"),
        "{}",
        serve.progress()
    );
    assert!(serve.cancel());
    assert_eq!(serve.wait(Some(10_000)), OrigamiOperationState::Cancelled);

    lab.core.install_reset(key.clone()).unwrap();
    assert!(lab.core.install_plan(key).unwrap().is_none());
    assert_eq!(origami_install_releases()[0].release, "6.5.30");
}

#[test]
fn the_flow_gates_continue_and_asks_what_to_do_on_close() {
    let flow = OrigamiInstallFlow::new(None);
    assert_eq!(flow.step(), OrigamiInstallStep::Release);
    assert_eq!(
        (flow.release().as_str(), flow.profile().as_str()),
        ("6.5.30", "standard")
    );
    flow.next();
    flow.next();
    flow.next();
    assert_eq!(flow.step(), OrigamiInstallStep::Disk);
    assert!(!flow.can_continue(false));
    flow.set_disk(OrigamiDiskState::None);
    assert!(flow.can_continue(false));
    assert!(flow.close_machine_question(true).is_none());
    assert!(origami_install_texts().quit_text.contains("install server"));
}

#[test]
fn the_boot_console_of_a_headless_machine_is_serial() {
    let lab = Lab::new(None);
    let machine = lab.core.open_machine(lab.key.clone()).unwrap();
    assert_eq!(machine.boot_console().unwrap(), OrigamiBootConsole::Serial);
}
