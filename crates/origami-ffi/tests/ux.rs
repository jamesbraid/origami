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
