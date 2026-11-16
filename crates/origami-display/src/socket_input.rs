//! Input over the display socket: typed events as version 3 `INPUT` messages,
//! written to the connection the transport holds. A batch is one write, so
//! QEMU applies it as one guest report.

use crate::input;
use crate::transport::Uplink;
use origami_engine::{InputEvent, InputSink, Result};

pub struct SocketInput {
    uplink: Uplink,
    seq: u32,
}

impl SocketInput {
    pub fn new(uplink: Uplink) -> SocketInput {
        SocketInput { uplink, seq: 0 }
    }
}

impl InputSink for SocketInput {
    fn send(&mut self, events: &[InputEvent]) -> Result<()> {
        let messages = input::messages(&input::coalesce(events));
        if messages.is_empty() {
            return Ok(());
        }
        // A diagnostic: ORIGAMI_GUI_DEBUG_INPUT=1 prints each batch sent.
        if std::env::var_os("ORIGAMI_GUI_DEBUG_INPUT").is_some() {
            eprintln!("input: {messages:?}");
        }
        let mut bytes = Vec::with_capacity(messages.len() * 32);
        for m in &messages {
            self.seq = self.seq.wrapping_add(1);
            bytes.extend_from_slice(&m.encode(false, self.seq));
        }
        let mut up = self.uplink.lock().unwrap_or_else(|e| e.into_inner());
        match up.as_mut() {
            Some(w) => {
                w.write_all(&bytes)?;
                w.flush()?;
                Ok(())
            }
            None => Err("the machine's display is not connected".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{message_type, MSG_INPUT};
    use origami_engine::{Key, PointerButton};
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_batch_is_one_write_of_numbered_messages() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let uplink: Uplink = Arc::new(Mutex::new(Some(Box::new(Capture(bytes.clone())))));
        let mut s = SocketInput::new(uplink.clone());
        s.send(&[
            InputEvent::Key {
                key: Key::A,
                down: true,
            },
            InputEvent::PointerMove { dx: 1, dy: 2 },
            InputEvent::PointerMove { dx: 1, dy: 2 },
            InputEvent::Button {
                button: PointerButton::Left,
                down: true,
            },
        ])
        .unwrap();
        let sent = bytes.lock().unwrap().clone();
        assert_eq!(sent.len(), 3 * 32, "the two motions merged");
        for (i, m) in sent.chunks(32).enumerate() {
            assert_eq!(message_type(m), MSG_INPUT);
            assert_eq!(
                u32::from_le_bytes(m[4..8].try_into().unwrap()),
                i as u32 + 1,
                "seq"
            );
        }
        assert_eq!(
            u32::from_le_bytes(sent[16..20].try_into().unwrap()),
            30,
            "evdev KEY_A"
        );
        assert_eq!(
            i32::from_le_bytes(sent[32 + 24..32 + 28].try_into().unwrap()),
            2,
            "merged dx"
        );
        // Nothing to send is not a write.
        s.send(&[InputEvent::PointerMove { dx: 0, dy: 0 }]).unwrap();
        assert_eq!(bytes.lock().unwrap().len(), 96);
        // No connection: an error, not a silent drop.
        *uplink.lock().unwrap() = None;
        assert!(s
            .send(&[InputEvent::Key {
                key: Key::A,
                down: false
            }])
            .is_err());
    }
}
