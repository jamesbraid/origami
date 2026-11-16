//! The terminal of a foreground run as one more console client.
//!
//! QEMU used to own the terminal through a stdio chardev. Now the runner
//! owns the console, so the terminal is bridged to the broker like any
//! other client, and `origami console` can attach beside it.

use crate::broker::Broker;
use crate::link::{Link, LinkSink};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct Terminal {
    output: JoinHandle<()>,
    _mode: Option<raw::RawMode>,
}

impl Terminal {
    /// Wait a moment for the last output to reach the screen.
    pub fn finish(self) {
        let _ = self.output.join();
    }
}

pub fn attach(broker: &Arc<Broker<LinkSink>>, link: &Arc<Link>) -> io::Result<Terminal> {
    link.ensure()?;
    let mode = raw::RawMode::enter();
    // Everything since the runner began, as a terminal attached from the
    // start would have shown.
    let mut reader = broker.reader_at(broker.live_from());
    let output = thread::Builder::new()
        .name("terminal-out".into())
        .spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            let mut out = io::stdout();
            loop {
                let o = reader.read_wait(&mut buf, Some(Duration::from_millis(250)));
                if o.n > 0
                    && out
                        .write_all(&buf[..o.n])
                        .and_then(|()| out.flush())
                        .is_err()
                {
                    break;
                }
                if o.eof {
                    break;
                }
            }
        })?;
    let conn = broker.connect(None);
    // Detached: a blocked stdin read cannot be interrupted, and the process
    // ends with the machine.
    thread::Builder::new()
        .name("terminal-in".into())
        .spawn(move || {
            let mut buf = [0u8; 1024];
            let mut stdin = io::stdin();
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let _ = conn.send(&buf[..n]);
                    }
                }
            }
            // Keep the connection until the end of input, then let it go; the
            // output side carries on.
        })?;
    Ok(Terminal {
        output,
        _mode: mode,
    })
}

#[cfg(unix)]
mod raw {
    use std::sync::OnceLock;

    static SAVED: OnceLock<libc::termios> = OnceLock::new();

    /// The terminal in the mode QEMU's stdio chardev used: no echo, no line
    /// editing, no output translation, signals still on. Restored on drop
    /// and when a signal ends the process.
    pub struct RawMode;

    extern "C" fn restore_and_die(sig: libc::c_int) {
        if let Some(t) = SAVED.get() {
            // SAFETY: tcsetattr is async-signal-safe and `t` is valid.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, t) };
        }
        // SAFETY: restoring the default action and re-raising is the
        // standard way to die of the signal after cleaning up.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    impl RawMode {
        pub fn enter() -> Option<RawMode> {
            // SAFETY: plain libc terminal calls on descriptor 0 with a
            // zeroed termios that tcgetattr fills.
            unsafe {
                if libc::isatty(0) == 0 {
                    return None;
                }
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) != 0 {
                    return None;
                }
                let _ = SAVED.set(t);
                t.c_iflag &= !(libc::IGNBRK
                    | libc::BRKINT
                    | libc::PARMRK
                    | libc::ISTRIP
                    | libc::INLCR
                    | libc::IGNCR
                    | libc::ICRNL
                    | libc::IXON);
                t.c_oflag &= !libc::OPOST;
                t.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::IEXTEN);
                t.c_cflag &= !(libc::CSIZE | libc::PARENB);
                t.c_cflag |= libc::CS8;
                if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                    return None;
                }
                for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                    libc::signal(sig, restore_and_die as *const () as libc::sighandler_t);
                }
            }
            Some(RawMode)
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            if let Some(t) = SAVED.get() {
                // SAFETY: as above.
                unsafe { libc::tcsetattr(0, libc::TCSANOW, t) };
            }
        }
    }
}

#[cfg(not(unix))]
mod raw {
    /// Windows consoles stay in line mode.
    pub struct RawMode;

    impl RawMode {
        pub fn enter() -> Option<RawMode> {
            None
        }
    }
}
