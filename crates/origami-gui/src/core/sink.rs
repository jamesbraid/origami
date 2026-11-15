use super::engine::Result;
use super::InputEvent;

/// Where an engine sends the guest's input. One per machine, used from one
/// thread. The QEMU engine's is QMP `input-send-event`; a faster channel
/// replaces it by implementing this and being handed to the engine.
pub trait InputSink: Send {
    /// Deliver `events` in order. A sink may hold a connection open while
    /// events keep coming and must give it up when `idle` is called after a
    /// quiet moment, so it never starves other users of the machine.
    fn send(&mut self, events: &[InputEvent]) -> Result<()>;

    /// Called after a short quiet spell with nothing to send.
    fn idle(&mut self) {}
}
