//! The terminal over a fake machine's console: output arrives without the
//! app shuttling bytes, and what the app sends comes back echoed.
#![cfg(all(unix, feature = "fake"))]

mod common;

use common::*;
use origami_ffi::*;
use std::time::{Duration, Instant};

fn wait(mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn line(t: &OrigamiTerminal, row: u16) -> String {
    t.row(row).unwrap().iter().map(|c| c.text.clone()).collect()
}

#[test]
fn guest_output_and_sent_input_reach_the_screen() {
    let lab = Lab::new(None);
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    let t = OrigamiTerminal::new(machine.clone(), 10, 40).unwrap();
    wait(|| line(&t, 0).starts_with("fake console"));
    let seen = t.changed_since(0).sequence;
    t.send_key(OrigamiTerminalKey::Text { text: "x".into() }, false, false)
        .unwrap();
    t.paste("ls".into()).unwrap();
    wait(|| line(&t, 1).starts_with("login: xls"));
    let damage = t.changed_since(seen);
    assert!(!damage.full && damage.rows == [1]);
    // Bracketed paste reaches the console wrapped.
    lab.core
        .fake_say(lab.key.clone(), "\x1b[?2004hZ".into())
        .unwrap();
    wait(|| line(&t, 1).contains('Z'));
    t.paste("a\nb".into()).unwrap();
    wait(|| {
        let raw = machine.console_read(0, 4096).bytes;
        raw.windows(15).any(|w| w == b"\x1b[200~a\rb\x1b[201~")
    });
    assert!(matches!(t.row(10), Err(CoreError::Invalid { .. })));
    lab.core.shutdown();
}

#[test]
fn a_restart_clears_the_screen_instead_of_appending() {
    let lab = Lab::new(None);
    let machine = lab.core.start_machine(lab.key.clone()).unwrap();
    let t = OrigamiTerminal::new(machine.clone(), 10, 40).unwrap();
    wait(|| line(&t, 0).starts_with("fake console"));
    lab.core.fake_say(lab.key.clone(), "old".into()).unwrap();
    wait(|| line(&t, 1).contains("old"));
    machine.stop(OrigamiStopKind::Force).unwrap();
    wait(|| {
        machine.view().state == OrigamiMachineState::Exited
            || machine.view().state == OrigamiMachineState::NotRunning
    });
    machine.start().unwrap();
    wait(|| line(&t, 0).starts_with("fake console") && !line(&t, 1).contains("old"));
    assert_eq!(t.scrollback_count(), 0);
    lab.core.shutdown();
}
