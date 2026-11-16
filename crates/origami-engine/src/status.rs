//! Why a machine's view shows what it does.
//!
//! A blank view that only says "waiting" leaves the user guessing among a
//! QEMU that never started, one that started without the shared-memory
//! display, one whose display socket accepts but passes nothing, and a guest
//! that is up on its serial console. [`reason`] picks the first stage that has
//! not happened and says so, with the last error QEMU wrote if there is one.

use std::path::PathBuf;

/// What an engine can say about one machine's display connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkFacts {
    /// A connection to the display socket is up.
    pub connected: bool,
    /// QEMU's first message came and the control block is mapped.
    pub welcomed: bool,
    /// Memory regions QEMU has passed on this connection.
    pub regions: u32,
    /// Heads with a picture.
    pub active_heads: u32,
    /// Why the last attempt to reach the socket failed.
    pub connect_error: Option<String>,
    /// Why the last connection was dropped (a protocol or import failure).
    pub protocol_error: Option<String>,
    /// Connections made over the machine's life, and attempts that failed.
    pub connects: u64,
    pub connect_failures: u64,
    /// Messages read from QEMU, in total.
    pub messages: u64,
}

/// Everything [`reason`] weighs.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// The machine's runner is up: its run record exists and its process
    /// lives. A runner that is still starting has no record yet.
    pub running: bool,
    /// Where the display socket should be.
    pub socket: PathBuf,
    pub socket_exists: bool,
    pub link: LinkFacts,
    /// The last distinct error line in `logs/runner.log`, with its count.
    pub runner_error: Option<(String, u64)>,
    /// A head's size, once there is a picture.
    pub picture: Option<(u32, u32)>,
}

/// What a view says, in the order the stages come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// QEMU is not running.
    NotRunning,
    /// QEMU runs but the display socket is not there.
    NoSocket,
    /// The socket is there and nothing connected to it.
    NotConnected,
    /// Connected; QEMU has not sent its first message.
    NoWelcome,
    /// Connected; QEMU has passed no memory region.
    NoRegion,
    /// Regions passed; no head has a picture, as when the guest's console is
    /// serial and it never sets a video mode.
    NoPicture,
    /// A picture is on screen.
    Showing,
}

/// The first stage that has not happened.
pub fn stage(f: &Facts) -> Reason {
    if f.picture.is_some() && f.running {
        Reason::Showing
    } else if !f.running {
        Reason::NotRunning
    } else if !f.link.connected && !f.socket_exists {
        Reason::NoSocket
    } else if !f.link.connected {
        Reason::NotConnected
    } else if !f.link.welcomed {
        Reason::NoWelcome
    } else if f.link.regions == 0 {
        Reason::NoRegion
    } else {
        Reason::NoPicture
    }
}

/// A sentence for the view's status line.
pub fn reason(f: &Facts) -> String {
    let stage = stage(f);
    let mut text = match &stage {
        Reason::Showing => {
            let (w, h) = f.picture.unwrap_or_default();
            return format!("{w}x{h}");
        }
        Reason::NotRunning => "QEMU is not running".to_string(),
        Reason::NoSocket => format!(
            "QEMU is running but its display socket does not exist ({}): it was started without the shared-memory display, or this QEMU does not offer it",
            f.socket.display()
        ),
        Reason::NotConnected => match &f.link.connect_error {
            Some(e) => format!("display socket not connected: {e}"),
            None => "display socket not connected".to_string(),
        },
        Reason::NoWelcome => "connected to the display socket, waiting for QEMU's first message".to_string(),
        Reason::NoRegion => "connected, but QEMU has passed no display REGION yet".to_string(),
        Reason::NoPicture => format!(
            "connected with {} region{}, but the guest has set no video mode (is its console serial? see the Serial view)",
            f.link.regions,
            if f.link.regions == 1 { "" } else { "s" }
        ),
    };
    if let Some(e) = &f.link.protocol_error {
        if !matches!(stage, Reason::NotRunning) {
            text.push_str(&format!("; the display connection failed: {e}"));
        }
    }
    if let Some((line, count)) = &f.runner_error {
        let times = if *count > 1 {
            format!(" (\u{d7}{count})")
        } else {
            String::new()
        };
        let lead = if matches!(stage, Reason::NotRunning) {
            "; last error in runner.log: "
        } else {
            "; QEMU reported: "
        };
        text.push_str(&format!("{lead}{line}{times}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up() -> Facts {
        Facts {
            running: true,
            socket: "/m/state/display.sock".into(),
            socket_exists: true,
            link: LinkFacts {
                connected: true,
                welcomed: true,
                regions: 2,
                active_heads: 1,
                ..Default::default()
            },
            picture: Some((1280, 1024)),
            ..Default::default()
        }
    }

    #[test]
    fn a_machine_that_is_not_running_says_so() {
        let f = Facts::default();
        assert_eq!(stage(&f), Reason::NotRunning);
        assert_eq!(reason(&f), "QEMU is not running");
    }

    #[test]
    fn a_dead_machine_names_the_error_it_left_behind() {
        let f = Facts {
            runner_error: Some(("qemu: cannot open image".into(), 1)),
            ..Default::default()
        };
        assert_eq!(
            reason(&f),
            "QEMU is not running; last error in runner.log: qemu: cannot open image"
        );
    }

    #[test]
    fn each_stage_before_a_picture_has_its_own_reason() {
        let mut f = up();
        f.picture = None;
        f.link.active_heads = 0;
        assert_eq!(stage(&f), Reason::NoPicture);
        assert!(reason(&f).contains("set no video mode"), "{}", reason(&f));
        assert!(reason(&f).contains("2 regions"));

        f.link.regions = 0;
        assert_eq!(stage(&f), Reason::NoRegion);
        assert_eq!(
            reason(&f),
            "connected, but QEMU has passed no display REGION yet"
        );

        f.link.welcomed = false;
        assert_eq!(stage(&f), Reason::NoWelcome);

        f.link.connected = false;
        f.link.connect_error = Some("Connection refused (os error 111)".into());
        assert_eq!(stage(&f), Reason::NotConnected);
        assert_eq!(
            reason(&f),
            "display socket not connected: Connection refused (os error 111)"
        );

        f.socket_exists = false;
        assert_eq!(stage(&f), Reason::NoSocket);
        assert!(reason(&f).contains("/m/state/display.sock"));
    }

    #[test]
    fn the_runner_error_is_added_to_a_stuck_stage_with_its_count() {
        let mut f = up();
        f.picture = None;
        f.link.regions = 0;
        f.runner_error = Some(("display-shm: failed to pass region fd".into(), 65_371));
        assert_eq!(
            reason(&f),
            "connected, but QEMU has passed no display REGION yet; QEMU reported: \
             display-shm: failed to pass region fd (\u{d7}65371)"
        );
    }

    #[test]
    fn a_dropped_connection_gives_its_failure() {
        let mut f = up();
        f.picture = None;
        f.link.connected = false;
        f.link.welcomed = false;
        f.link.regions = 0;
        f.link.protocol_error = Some("QEMU speaks version 4".into());
        assert!(
            reason(&f).contains("QEMU speaks version 4"),
            "{}",
            reason(&f)
        );
    }

    #[test]
    fn a_picture_is_just_its_size_and_ignores_old_errors() {
        let mut f = up();
        f.runner_error = Some(("old error".into(), 3));
        assert_eq!(stage(&f), Reason::Showing);
        assert_eq!(reason(&f), "1280x1024");
    }

    #[test]
    fn a_stale_picture_of_a_machine_that_has_stopped_is_not_showing() {
        let mut f = up();
        f.running = false;
        assert_eq!(stage(&f), Reason::NotRunning);
    }
}
