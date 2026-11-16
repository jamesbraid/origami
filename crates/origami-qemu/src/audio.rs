//! Guest audio: which host backend QEMU is given.
//!
//! QEMU lists the backends it was built with under `-audiodev help`. The
//! launch takes the first one this OS prefers; a QEMU without any of them,
//! or one that cannot be asked, gets `-audio none` and a note, because a
//! machine without sound is better than one that does not start.

use std::path::Path;
use std::process::Command;

/// The backends each OS offers, best first.
pub const PREFERRED: &[&str] = if cfg!(target_os = "macos") {
    &["coreaudio"]
} else if cfg!(windows) {
    &["wasapi", "dsound"]
} else {
    &["pipewire", "pa"]
};

/// The backend names `qemu` reports under `-audiodev help`, or None if it
/// could not be asked.
pub fn available(qemu: &Path) -> Option<Vec<String>> {
    let output = Command::new(qemu)
        .args(["-audiodev", "help"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.ends_with(':'))
            .map(str::to_owned)
            .collect(),
    )
}

/// The first preferred backend among `available`.
pub fn choose(available: &[String]) -> Option<&'static str> {
    PREFERRED
        .iter()
        .copied()
        .find(|name| available.iter().any(|a| a == name))
}

/// The `-audio` option for a launch with `qemu`, and the note to print when
/// the guest ends up without sound. `enabled` false is the user's choice,
/// which needs no note.
pub fn arguments(qemu: &Path, enabled: bool) -> (Vec<String>, Option<String>) {
    let none = |note: Option<String>| (vec!["-audio".into(), "none".into()], note);
    if !enabled {
        return none(None);
    }
    match available(qemu) {
        None => none(Some(
            "guest audio is off: could not ask QEMU which audio backends it has".into(),
        )),
        Some(list) => match choose(&list) {
            Some(driver) => (vec!["-audio".into(), format!("driver={driver}")], None),
            None => none(Some(format!(
                "guest audio is off: this QEMU has none of the {} backends ({})",
                std::env::consts::OS,
                PREFERRED.join(", ")
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_first_preferred_backend_wins() {
        let all = names(&[
            "none",
            "alsa",
            "pa",
            "pipewire",
            "coreaudio",
            "wasapi",
            "dsound",
        ]);
        assert_eq!(choose(&all), Some(PREFERRED[0]));
        assert_eq!(choose(&names(&["none", "alsa", "sdl"])), None);
    }

    #[test]
    fn a_backend_further_down_the_list_is_used_when_the_first_is_missing() {
        let second = names(&[PREFERRED[PREFERRED.len() - 1]]);
        assert_eq!(choose(&second), Some(PREFERRED[PREFERRED.len() - 1]));
    }

    #[test]
    fn turning_audio_off_asks_nothing_and_says_nothing() {
        let (args, note) = arguments(Path::new("/no/such/qemu"), false);
        assert_eq!(args, ["-audio", "none"]);
        assert_eq!(note, None);
    }

    #[test]
    fn a_qemu_that_cannot_be_asked_gets_no_audio_and_a_note() {
        let (args, note) = arguments(Path::new("/no/such/qemu"), true);
        assert_eq!(args, ["-audio", "none"]);
        assert!(note.unwrap().contains("guest audio is off"));
    }
}
