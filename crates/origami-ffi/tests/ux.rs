//! The 0.6.2 words and cards through the control plane.
#![cfg(feature = "fake")]

mod common;

use common::*;
use origami_ffi::*;

#[test]
fn state_words_cards_and_alerts_come_from_the_core() {
    let lab = Lab::new(None);
    let summary = lab.core.list_machines().unwrap().remove(0);
    assert_eq!(
        origami_state_label(OrigamiShownState::Saving).word,
        "Saving"
    );
    let row = origami_row(summary.clone(), false);
    assert_eq!(
        (row.state, row.word.as_str()),
        (OrigamiShownState::Stopped, "Stopped")
    );
    assert_eq!(row.line.tone, OrigamiTone::None);
    assert!(row.description.starts_with(&summary.name));
    // A stopped machine is not installing, whatever the guide says.
    assert_eq!(origami_row(summary, true).line.text, row.line.text);
    let no_picture = origami_library_line_for(OrigamiShownState::NoPicture, "Fuel".into());
    assert_eq!(
        (no_picture.text.as_str(), no_picture.tone),
        ("No picture", OrigamiTone::Critical)
    );

    let error = OrigamiErrorInfo {
        kind: OrigamiErrorKind::Invalid,
        message: "Fuel has no disk.".into(),
        detail: Some("drive 1".into()),
        fix: OrigamiErrorFix::ChooseDisk,
    };
    let card = origami_machine_card(
        OrigamiShownState::Stopped,
        Some(error),
        "Fuel".into(),
        OrigamiPlatform::Linux,
    )
    .unwrap();
    assert_eq!(card.title, "Fuel couldn't start");
    assert_eq!(card.actions[0].kind, OrigamiActionKind::ChooseDisk);

    let alert = origami_alert(
        OrigamiAlertKind::Remove {
            name: "Fuel".into(),
            disk_left: vec![],
        },
        OrigamiPlatform::Windows,
    );
    assert_eq!(alert.title, "Delete Fuel?");
    assert_eq!(alert.buttons[0].role, OrigamiButtonRole::Cancel);
    let detach = origami_alert(
        OrigamiAlertKind::DetachDrive {
            name: "Disk 2".into(),
        },
        OrigamiPlatform::Linux,
    );
    assert_eq!(detach.title, "Detach Disk 2?");
    assert_eq!(detach.buttons[1].role, OrigamiButtonRole::Destructive);
    assert_eq!(
        origami_toast(OrigamiToastKind::DiscEjected, OrigamiPlatform::Macos).text,
        "Disc ejected"
    );
    assert_eq!(
        origami_command_tooltip("machine.start".into(), OrigamiPlatform::Macos).as_deref(),
        Some("Start the machine (⌘R)")
    );
    assert_eq!(
        origami_shortcut_groups(OrigamiPlatform::Linux)[0].items[0].keys,
        "Ctrl+Alt"
    );
}

#[test]
fn a_running_machine_has_a_status_bar_and_is_not_removed() {
    let lab = Lab::new(None);
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    assert_eq!(machine.shown_state(false), OrigamiShownState::Running);
    let bar = machine
        .status_bar(false, true, OrigamiPlatform::Windows)
        .unwrap();
    assert_eq!(
        bar.pointer.as_deref(),
        Some("Captured, press Ctrl+Alt to release")
    );
    assert!(matches!(
        lab.core.remove_machine(lab.key.clone()),
        Err(CoreError::Busy { .. })
    ));
}

#[test]
fn a_drop_is_classified() {
    let lab = Lab::new(None);
    let iso = lab.root.path().join("disc.iso");
    std::fs::write(&iso, [0u8; 16]).unwrap();
    let kind = origami_classify_drop(iso.to_str().unwrap().into());
    assert_eq!(kind, OrigamiDropKind::CdImage);
    assert_eq!(origami_drop_caption(kind).as_deref(), Some("Change CD"));
    assert_eq!(
        origami_disk_options(true)[0].choice,
        OrigamiDiskChoice::Ready
    );
}

#[test]
fn any_error_reads_as_message_detail_and_fix() {
    let lab = Lab::new(None);
    let error = lab
        .core
        .open_machine("no-such-machine".into())
        .err()
        .unwrap();
    let info = origami_error_info(error);
    assert_eq!(info.kind, OrigamiErrorKind::NotFound);
    assert!(info.message.contains("no-such-machine"));
    assert_eq!(info.fix, OrigamiErrorFix::None);
}

#[test]
fn the_console_marks_a_guide_step_seen() {
    let lab = Lab::new(None);
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    let step = |confirm: &str| OrigamiGuideStep {
        send: None,
        title: "Open the command monitor".into(),
        watch: None,
        typed: vec![],
        note: None,
        confirm: Some(confirm.into()),
    };
    assert_eq!(machine.guide_seen(vec![step("Option? ")]), 0);
    lab.core
        .fake_say(lab.key.clone(), "Option? ".into())
        .unwrap();
    for _ in 0..500 {
        if machine.guide_seen(vec![step("Option? ")]) == 1 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the prompt was never seen");
}

#[test]
fn a_fixture_machine_is_removed_without_the_real_trash() {
    let lab = Lab::new(None);
    let plan = lab.core.remove_plan(lab.key.clone()).unwrap();
    assert!(plan.disk_left.is_empty() && std::path::Path::new(&lab.key).exists());
    let removed = lab.core.remove_machine(lab.key.clone()).unwrap();
    assert_eq!(removed.name, plan.name);
    assert!(lab.core.list_machines().unwrap().is_empty());
    assert!(lab
        .core
        .fake_trashed()
        .iter()
        .any(|p| p.ends_with(&removed.name)));
}

#[test]
fn the_apps_ask_the_core_for_the_words_they_wrote_themselves() {
    let flow = OrigamiInstallFlow::new(None);
    assert_eq!(flow.continue_reason(false), None);
    assert_eq!(flow.check_status(), None);
    assert_eq!(
        origami_ui_string(OrigamiUiString::SelectAMachineFirst),
        "Select a machine first."
    );
    assert_eq!(
        origami_capture_hint(OrigamiPlatform::Macos),
        "Mouse captured. Press ⌃⌥ to release."
    );
    assert_eq!(
        origami_power_off_progress("Fuel".into(), false),
        "Powering off Fuel…"
    );
    assert_eq!(
        origami_show_in_label(OrigamiPlatform::Windows),
        "Show in Explorer"
    );
    let failed = origami_alert(
        OrigamiAlertKind::PowerOffFailed {
            names: vec!["Fuel".into()],
        },
        OrigamiPlatform::Linux,
    );
    assert_eq!(failed.buttons[1].role, OrigamiButtonRole::Destructive);
    let toast = origami_toast(
        OrigamiToastKind::PictureSize { name: "Fit".into() },
        OrigamiPlatform::Linux,
    );
    assert_eq!(toast.text, "Picture size: Fit");
    let close = origami_alert(
        OrigamiAlertKind::CloseWindow {
            names: vec!["a".into(), "b".into()],
        },
        OrigamiPlatform::Linux,
    );
    assert_eq!(close.buttons[1].label, "Power off all");
    assert_eq!(
        origami_row_description_for(
            OrigamiShownState::NoPicture,
            "fuel".into(),
            "Fuel".into(),
            None
        ),
        "fuel, Fuel, No picture"
    );
    let cancel = origami_ui_string(OrigamiUiString::CancelStart);
    assert_eq!(cancel, "Cancel start");
}
